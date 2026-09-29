//! Fixtures (docs/JOBS.md, "Verifying an automation"): reading a
//! workflow's `.forge/fixtures/<workflow>/*.json`, replaying each through
//! the executor in dry-run mode (`forge job test`), gating an effectful
//! workflow on that replay passing, and measuring providers against the
//! same fixtures (`forge job bench`).

use super::{RunNow, run_now, string_fields};
use crate::ctx::Forge;
use crate::store::{Job, JobEffect, JobState};
use crate::workflows::{self, Kind};
use crate::{checks, config, git, unix_now};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The gate on automations that act on the world (docs/EXECUTION.md,
/// "Verifiable inside, optional outside"): a run workflow with any
/// `effect` step is not enabled — `forge job enable`, or the scheduler's
/// first run — until `forge job test` passes on a fixture for it in the
/// project's repository. A workflow with no effect step passes untouched.
/// The refusal names the fixture directory it looked for.
pub async fn require_fixture_pass(
    f: &Forge,
    project: &str,
    workflow: &str,
    landed_sha: &str,
) -> Result<()> {
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project:?} has no registered repository"))?;
    let repo = PathBuf::from(repo);
    let (_, steps, _) =
        workflows::resolve_job_for_project(&f.paths.home, &repo, landed_sha, workflow)?;
    if !steps.iter().any(|s| s.effect.is_some()) {
        return Ok(());
    }
    let dir = repo.join(".forge/fixtures").join(workflow);
    let refuse = |why: String| {
        anyhow::anyhow!(
            "run workflow {workflow:?} has an effect step and cannot be enabled until `forge job test` has passed on a fixture for it; looked for fixtures under {}: {why}",
            dir.display()
        )
    };
    let outcomes = test(&repo, Some(workflow))
        .await
        .map_err(|e| refuse(format!("{e:#}")))?;
    let failed: Vec<String> = outcomes
        .iter()
        .filter_map(|o| o.differences.first().map(|d| format!("{}: {d}", o.name)))
        .collect();
    if !failed.is_empty() {
        return Err(refuse(format!("a fixture fails: {}", failed.join("; "))));
    }
    Ok(())
}

/// One fixture, under a repository's `.forge/fixtures/<workflow>/*.json`
/// (docs/JOBS.md, "Verifying an automation"): the input document a real
/// trigger would have delivered, what a replay of it must produce, and
/// optionally the model output each directive step is to be given instead
/// of a model call:
///
/// ```json
/// {"input": {...},
///  "expect": {"state": "ok", "effects": [{"kind": "file", "target": "CHANGELOG.md", "summary_contains": "fix"}]},
///  "outputs": {"<step action>": {...}}}
/// ```
///
/// The older shape, `{"input": {...}, "expected_kind": "..."}` (what
/// `forge job bench` measures a judgment against), is still read, as
/// `expect.effects = [{kind}]`; `expected_kind` stays on the fixture for
/// `bench`, which compares it with what the model said.
#[derive(Debug)]
struct Fixture {
    input: serde_json::Value,
    expect: Expect,
    outputs: BTreeMap<String, serde_json::Value>,
    expected_kind: Option<String>,
}

/// What a replay must come to: the job's final state and its whole effect
/// log — every effect listed must have been logged, and no other.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// `ok` when the fixture does not say.
    #[serde(default = "expected_state_default")]
    state: String,
    #[serde(default)]
    effects: Vec<ExpectedEffect>,
}

/// One effect a replay must log: its `kind`, and, when given, its exact
/// `target` and a fragment its `summary` contains.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedEffect {
    kind: String,
    target: Option<String>,
    summary_contains: Option<String>,
}

fn expected_state_default() -> String {
    JobState::Ok.as_str().to_string()
}

/// The states a dry-run replay can end in.
const FIXTURE_STATES: [JobState; 4] = [
    JobState::Ok,
    JobState::Skipped,
    JobState::Failed,
    JobState::NeedsHuman,
];

#[derive(Deserialize)]
struct RawFixture {
    input: serde_json::Value,
    expect: Option<Expect>,
    #[serde(default)]
    outputs: BTreeMap<String, serde_json::Value>,
    expected_kind: Option<String>,
}

impl TryFrom<RawFixture> for Fixture {
    type Error = anyhow::Error;

    fn try_from(raw: RawFixture) -> Result<Fixture> {
        let expect = match (raw.expect, &raw.expected_kind) {
            (Some(e), _) => e,
            (None, Some(kind)) => Expect {
                state: expected_state_default(),
                effects: vec![ExpectedEffect {
                    kind: kind.clone(),
                    target: None,
                    summary_contains: None,
                }],
            },
            (None, None) => {
                anyhow::bail!("a fixture needs an `expect` (or the older `expected_kind`)")
            }
        };
        if !FIXTURE_STATES.iter().any(|s| s.as_str() == expect.state) {
            let names: Vec<&str> = FIXTURE_STATES.iter().map(|s| s.as_str()).collect();
            anyhow::bail!(
                "expect.state {:?} is not one of {}",
                expect.state,
                names.join(", ")
            );
        }
        Ok(Fixture {
            input: raw.input,
            expect,
            outputs: raw.outputs,
            expected_kind: raw.expected_kind,
        })
    }
}

/// Every fixture under `<repo>/.forge/fixtures/<workflow>/`, name and
/// parsed content, sorted by file name so a run is reproducible.
fn load_fixtures(repo: &Path, workflow: &str) -> Result<Vec<(String, Fixture)>> {
    let dir = repo.join(".forge").join("fixtures").join(workflow);
    let entries = std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)?;
            let load =
                || -> Result<Fixture> { serde_json::from_str::<RawFixture>(&text)?.try_into() };
            let fx = load().with_context(|| format!("{}", p.display()))?;
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            Ok((name, fx))
        })
        .collect()
}

/// What a fixture's replay differs from its expectation by, first
/// difference first, none when it matches: a wrong state, then each
/// expected effect the log lacks, then each logged effect nothing
/// expected. An expected effect claims the first logged effect that fits
/// it, so the same effect logged twice must be expected twice. `why` says
/// what ended a run in a state other than the one expected.
fn differences(expect: &Expect, state: JobState, effects: &[JobEffect], why: &str) -> Vec<String> {
    let mut out = Vec::new();
    if state.as_str() != expect.state {
        let mut d = format!(
            "wrong state: expected {}, got {}",
            expect.state,
            state.as_str()
        );
        if !why.is_empty() {
            d.push_str(&format!(" ({why})"));
        }
        out.push(d);
    }
    let mut unclaimed: Vec<&JobEffect> = effects.iter().collect();
    for want in &expect.effects {
        let fits = |e: &&JobEffect| {
            e.kind == want.kind
                && want.target.as_ref().is_none_or(|t| *t == e.target)
                && want
                    .summary_contains
                    .as_ref()
                    .is_none_or(|c| e.summary.contains(c.as_str()))
        };
        match unclaimed.iter().position(fits) {
            Some(i) => {
                unclaimed.remove(i);
            }
            None => {
                let mut d = format!("missing effect: {}", want.kind);
                if let Some(t) = &want.target {
                    d.push_str(&format!(" {t}"));
                }
                if let Some(c) = &want.summary_contains {
                    d.push_str(&format!(" with a summary containing {c:?}"));
                }
                out.push(d);
            }
        }
    }
    for e in unclaimed {
        out.push(format!(
            "extra effect: {} {}: {}",
            e.kind, e.target, e.summary
        ));
    }
    out
}

/// One provider's measurement across every fixture (docs/JOBS.md,
/// "Steps": "the bounded judgment the local model is fit for").
#[derive(Debug, Clone)]
pub struct BenchStat {
    pub provider: String,
    pub runs: usize,
    pub schema_valid: usize,
    pub kind_correct: usize,
    pub cost_usd: f64,
    pub seconds: f64,
}

/// `forge job bench <project> <workflow> --providers a,b`: every fixture
/// under the project repository's `.forge/fixtures/<workflow>/`, run once
/// per named provider, in dry-run mode, with every directive step's role
/// forced to that provider regardless of the operator's or project's own
/// routing — the same judgment, on the same inputs, under each candidate,
/// so the local model and the hosted ones are measured against each other
/// rather than against a moving target (docs/JOBS.md, "Steps").
pub async fn bench(
    f: &Forge,
    project: &str,
    workflow: &str,
    providers: &[String],
) -> Result<Vec<BenchStat>> {
    let project_row = f
        .store
        .project(project)?
        .with_context(|| format!("no project {project}"))?;
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} has no registered repository"))?;
    let repo_path = PathBuf::from(&repo);
    let cfg = config::load_working(&repo_path).await?;
    let landed_sha = git::rev_parse(&repo_path, &format!("refs/heads/{}", cfg.base_branch))
        .await
        .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo_path.display()))?;
    let (wf, steps) = workflows::resolve_job_in_repo(&repo_path, workflow)?;
    let fixtures = load_fixtures(&repo_path, workflow)?;
    if fixtures.is_empty() {
        anyhow::bail!(
            "no fixtures under {}",
            repo_path.join(".forge/fixtures").join(workflow).display()
        );
    }
    let directive_roles: Vec<String> = steps
        .iter()
        .filter(|s| s.action.kind == Kind::Directive)
        .filter_map(|s| s.role.clone())
        .collect();

    let mut out = Vec::new();
    for provider in providers {
        f.providers
            .get(provider.as_str())
            .with_context(|| format!("unknown provider {provider:?}; see `forge providers`"))?;
        let mut forced_roles = project_row.role_providers.clone();
        for role in &directive_roles {
            forced_roles.insert(role.clone(), provider.clone());
        }

        let mut stat = BenchStat {
            provider: provider.clone(),
            runs: 0,
            schema_valid: 0,
            kind_correct: 0,
            cost_usd: 0.0,
            seconds: 0.0,
        };
        for (name, fx) in &fixtures {
            let input_text = serde_json::to_string(&fx.input)?;
            let input_fields = string_fields(&fx.input)?;
            let job = Job {
                id: 0,
                project: project.to_string(),
                workflow: workflow.to_string(),
                workflow_hash: wf.hash.clone(),
                landed_sha: landed_sha.clone(),
                trigger_kind: workflows::TriggerOn::Manual.as_str().to_string(),
                trigger_ref: String::new(),
                state: JobState::Running,
                workflow_source: workflows::JobSource::Repo.as_str().to_string(),
                dry_run: true,
                started_at: unix_now(),
                finished_at: None,
                cost_usd: None,
                verdict_json: "[]".to_string(),
                due_at: None,
                retry_count: 0,
            };
            let job_id = f.store.create_job(&job)?;
            let t0 = Instant::now();
            run_now(RunNow {
                f,
                job_id,
                project,
                workflow,
                repo: &repo_path,
                landed_sha: &landed_sha,
                steps: &steps,
                assert: &wf.assert,
                skip_if: &wf.skip_if,
                limits: wf.limits.as_ref(),
                trigger: wf.trigger.as_ref(),
                workflow_env: &wf.env,
                dry_run: true,
                input_text: &input_text,
                input_fields: &input_fields,
                check_timeout_secs: cfg.check_timeout_secs,
                project_roles: &forced_roles,
                recorded: &BTreeMap::new(),
            })
            .await
            .with_context(|| format!("fixture {name:?} under provider {provider:?}"))?;
            let elapsed = t0.elapsed().as_secs_f64();

            let doc = f
                .store
                .job(job_id)?
                .with_context(|| format!("job {job_id} vanished"))?;
            let jsteps = f.store.job_steps(job_id)?;
            stat.runs += 1;
            stat.cost_usd += doc.cost_usd.unwrap_or(0.0);
            stat.seconds += elapsed;
            if let Some(d) = jsteps.iter().find(|s| s.kind == "directive")
                && !d.output_ref.is_empty()
            {
                stat.schema_valid += 1;
                if let Ok(text) = std::fs::read_to_string(&d.output_ref)
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                    && let Some(want) = fx.expected_kind.as_deref()
                    && v.get("kind").and_then(|k| k.as_str()) == Some(want)
                {
                    stat.kind_correct += 1;
                }
            }
        }
        out.push(stat);
    }
    Ok(out)
}

/// A fixture replay's verdict: which workflow and fixture, and every
/// difference from what it expected, first first — none means it passed.
#[derive(Debug, PartialEq, Eq)]
pub struct FixtureOutcome {
    pub workflow: String,
    pub name: String,
    pub differences: Vec<String>,
}

/// A directory under the system temp dir that is removed when this is
/// dropped, however the replay ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("forge-job-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files of the tree at `root` a replay's steps may see: what git
/// tracks or would track (so `target/` and the like stay behind), or every
/// file but `.git` when `root` is not in a repository at all.
fn tree_files(root: &Path) -> Result<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt;
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output();
    if let Ok(o) = listed
        && o.status.success()
    {
        return Ok(o
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
            .collect());
    }
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            if e.file_type()?.is_dir() {
                if e.file_name() != ".git" && e.file_name() != "target" {
                    walk(root, &path, out)?;
                }
            } else {
                out.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

/// Copy the working tree at `root` — what is on disk, committed or not,
/// so a check sees the work in progress — into `dest`.
fn copy_tree(root: &Path, dest: &Path) -> Result<()> {
    for rel in tree_files(root)? {
        let from = root.join(&rel);
        // Listed but gone (deleted, not yet staged), or not a file.
        if !from.is_file() {
            continue;
        }
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

/// The project a replay's job rows are recorded under, in its own store.
const TEST_PROJECT: &str = "job-test";

/// `forge job test [<workflow>] [<path>]`: replay every fixture of the
/// named run workflow — of every run workflow under `root` when none is
/// named — through the executor in dry-run mode and compare what each one
/// did with what it expected (docs/JOBS.md, "Verifying an automation").
///
/// Nothing of the operator's is read or written: the replay has a scratch
/// home of its own, holding a store the job rows go into and are thrown
/// away with, and a copy of the working tree at `root` committed as one
/// revision for the executor to archive from, so no `FORGE_HOME` is
/// needed and the built-in actions are all the catalog there is. A
/// directive step named in a fixture's `outputs` takes that output in
/// place of a model call; any other runs the model live, under the
/// built-in provider. A workflow with no fixtures has nothing to replay
/// and is not an error unless it was named.
pub async fn test(root: &Path, workflow: Option<&str>) -> Result<Vec<FixtureOutcome>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("no such directory {}", root.display()))?;
    let mut plan = Vec::new();
    for (name, resolved) in workflows::resolve_jobs_in_tree(&root, workflow)? {
        let fixtures = if root.join(".forge/fixtures").join(&name).is_dir() {
            load_fixtures(&root, &name)?
        } else {
            Vec::new()
        };
        if fixtures.is_empty() && workflow.is_none() {
            continue;
        }
        plan.push((name, resolved, fixtures));
    }
    if let Some(name) = workflow
        && plan.iter().all(|(_, _, fixtures)| fixtures.is_empty())
    {
        anyhow::bail!(
            "no fixtures under {}",
            root.join(".forge/fixtures").join(name).display()
        );
    }
    if plan.is_empty() {
        return Ok(Vec::new());
    }

    let scratch = Scratch::new()?;
    let snapshot = scratch.0.join("repo");
    copy_tree(&root, &snapshot)?;
    let sha = git::init_commit_all(&snapshot, "forge job test: the working tree").await?;
    let cfg = config::load_working(&snapshot).await?;

    let home = scratch.0.join("home");
    let paths = crate::ctx::Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home,
    };
    std::fs::create_dir_all(&paths.worktrees)?;
    std::fs::create_dir_all(&paths.logs)?;
    let store = crate::store::Store::open(&paths.home.join("forge.db"))?;
    store.create_project(&crate::store::Project {
        name: TEST_PROJECT.to_string(),
        purpose: "replaying fixtures".to_string(),
        created_at: unix_now(),
        ..Default::default()
    })?;
    let mut f = Forge::open_with(paths, store)?;
    f.report = crate::report::Reporter::quiet();

    let mut out = Vec::new();
    for (workflow, resolved, fixtures) in plan {
        let (wf, steps) = match resolved {
            Ok(r) => r,
            Err(e) => {
                out.push(FixtureOutcome {
                    workflow,
                    name: String::new(),
                    differences: vec![format!("the workflow does not resolve: {e:#}")],
                });
                continue;
            }
        };
        for (name, fx) in fixtures {
            let differences = match replay(
                &f,
                &snapshot,
                &sha,
                cfg.check_timeout_secs,
                &wf,
                &steps,
                &fx,
            )
            .await
            {
                Ok(d) => d,
                Err(e) => vec![format!("the replay itself failed: {e:#}")],
            };
            out.push(FixtureOutcome {
                workflow: workflow.clone(),
                name,
                differences,
            });
        }
    }
    Ok(out)
}

/// One fixture through the executor, recorded as a dry-run job in the
/// replay's own store, and what it did against what the fixture expects.
async fn replay(
    f: &Forge,
    snapshot: &Path,
    sha: &str,
    check_timeout_secs: u64,
    wf: &workflows::Workflow,
    steps: &[workflows::RunStep],
    fx: &Fixture,
) -> Result<Vec<String>> {
    let input_text = serde_json::to_string(&fx.input)?;
    let input_fields = string_fields(&fx.input)?;
    let job = Job {
        id: 0,
        project: TEST_PROJECT.to_string(),
        workflow: wf.name.clone(),
        workflow_hash: wf.hash.clone(),
        landed_sha: sha.to_string(),
        trigger_kind: workflows::TriggerOn::Manual.as_str().to_string(),
        trigger_ref: String::new(),
        state: JobState::Running,
        workflow_source: workflows::JobSource::Repo.as_str().to_string(),
        dry_run: true,
        started_at: unix_now(),
        finished_at: None,
        cost_usd: None,
        verdict_json: "[]".to_string(),
        due_at: None,
        retry_count: 0,
    };
    let job_id = f.store.create_job(&job)?;
    run_now(RunNow {
        f,
        job_id,
        project: TEST_PROJECT,
        workflow: &wf.name,
        repo: snapshot,
        landed_sha: sha,
        steps,
        assert: &wf.assert,
        skip_if: &wf.skip_if,
        limits: wf.limits.as_ref(),
        trigger: wf.trigger.as_ref(),
        workflow_env: &wf.env,
        dry_run: true,
        input_text: &input_text,
        input_fields: &input_fields,
        check_timeout_secs,
        project_roles: &BTreeMap::new(),
        recorded: &fx.outputs,
    })
    .await?;
    let done = f
        .store
        .job(job_id)?
        .with_context(|| format!("job {job_id} vanished"))?;
    let effects = f.store.job_effects(job_id)?;
    let verdict: Vec<checks::CheckResult> =
        serde_json::from_str(&done.verdict_json).unwrap_or_default();
    // What ended a run that did not end as expected: the first check that
    // failed, by name and its first line.
    let why = verdict
        .iter()
        .find(|c| !c.ok)
        .map(|c| format!("{}: {}", c.name, c.tail.lines().next().unwrap_or_default()))
        .unwrap_or_default();
    let mut out = differences(&fx.expect, done.state, &effects, &why);
    // A failed `setup` is the fixture's first difference even when the
    // fixture expects `failed`: the run never reached its steps, so it
    // cannot be the failure the fixture meant.
    if let Some(c) = verdict.iter().find(|c| c.name == "setup" && !c.ok)
        && !out.first().is_some_and(|d| d.starts_with("wrong state"))
    {
        out.insert(
            0,
            format!(
                "setup failed: {}",
                c.tail.lines().next().unwrap_or_default()
            ),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_fixtures_reads_and_sorts_by_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let fixtures_dir = dir.path().join(".forge").join("fixtures").join("triage");
        std::fs::create_dir_all(&fixtures_dir).unwrap();
        std::fs::write(
            fixtures_dir.join("b.json"),
            r#"{"input": {"title": "b"}, "expected_kind": "bug"}"#,
        )
        .unwrap();
        std::fs::write(
            fixtures_dir.join("a.json"),
            r#"{"input": {"title": "a"}, "expected_kind": "feature"}"#,
        )
        .unwrap();
        std::fs::write(fixtures_dir.join("ignored.txt"), "not json").unwrap();

        let fixtures = load_fixtures(dir.path(), "triage").unwrap();
        let names: Vec<&str> = fixtures.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(fixtures[0].1.expected_kind.as_deref(), Some("feature"));
        assert_eq!(fixtures[1].1.expected_kind.as_deref(), Some("bug"));
        assert_eq!(fixtures[0].1.input, serde_json::json!({"title": "a"}));
    }

    fn fixture_in(dir: &Path, text: &str) -> Result<Fixture> {
        let fixtures_dir = dir.join(".forge").join("fixtures").join("wf");
        std::fs::create_dir_all(&fixtures_dir).unwrap();
        std::fs::write(fixtures_dir.join("one.json"), text).unwrap();
        load_fixtures(dir, "wf").map(|mut v| v.remove(0).1)
    }

    #[test]
    fn the_older_fixture_shape_is_expect_effects_of_its_kind() {
        let dir = tempfile::tempdir().unwrap();
        let fx = fixture_in(dir.path(), r#"{"input": {}, "expected_kind": "bug"}"#).unwrap();
        assert_eq!(fx.expect.state, "ok");
        assert_eq!(fx.expect.effects.len(), 1);
        assert_eq!(fx.expect.effects[0].kind, "bug");
        assert!(fx.expect.effects[0].target.is_none());
        assert!(fx.outputs.is_empty());
    }

    #[test]
    fn the_new_fixture_shape_carries_expect_and_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let fx = fixture_in(
            dir.path(),
            r#"{"input": {"a": "b"},
                "expect": {"state": "skipped", "effects": [
                    {"kind": "file", "target": "x.txt", "summary_contains": "hi"}]},
                "outputs": {"judge": {"kind": "bug"}}}"#,
        )
        .unwrap();
        assert_eq!(fx.expect.state, "skipped");
        let e = &fx.expect.effects[0];
        assert_eq!(
            (
                e.kind.as_str(),
                e.target.as_deref(),
                e.summary_contains.as_deref()
            ),
            ("file", Some("x.txt"), Some("hi"))
        );
        assert_eq!(fx.outputs["judge"], serde_json::json!({"kind": "bug"}));
        assert!(fx.expected_kind.is_none());
    }

    #[test]
    fn a_fixture_without_an_expectation_or_with_a_typo_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let e = fixture_in(dir.path(), r#"{"input": {}}"#).unwrap_err();
        assert!(format!("{e:#}").contains("needs an `expect`"), "{e:#}");
        let e = fixture_in(
            dir.path(),
            r#"{"input": {}, "expect": {"effects": [{"kind": "file", "summary_contain": "x"}]}}"#,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("summary_contain"), "{e:#}");
        let e =
            fixture_in(dir.path(), r#"{"input": {}, "expect": {"state": "bogus"}}"#).unwrap_err();
        assert!(format!("{e:#}").contains("bogus"), "{e:#}");
    }

    fn logged(kind: &str, target: &str, summary: &str) -> JobEffect {
        JobEffect {
            id: 0,
            job_id: 1,
            seq: 0,
            kind: kind.to_string(),
            target: target.to_string(),
            summary: summary.to_string(),
            dry_run: true,
        }
    }

    fn expecting(state: &str, effects: &[(&str, Option<&str>, Option<&str>)]) -> Expect {
        Expect {
            state: state.to_string(),
            effects: effects
                .iter()
                .map(|(k, t, c)| ExpectedEffect {
                    kind: k.to_string(),
                    target: t.map(str::to_string),
                    summary_contains: c.map(str::to_string),
                })
                .collect(),
        }
    }

    #[test]
    fn a_replay_that_matches_has_no_differences() {
        let log = [
            logged("file", "a.txt", "wrote a (dry run)"),
            logged("row", "t", "r"),
        ];
        let want = expecting(
            "ok",
            &[("row", None, None), ("file", Some("a.txt"), Some("wrote"))],
        );
        assert!(differences(&want, JobState::Ok, &log, "").is_empty());
    }

    #[test]
    fn a_missing_effect_is_named_with_what_was_asked_of_it() {
        let log = [logged("file", "a.txt", "wrote")];
        let want = expecting(
            "ok",
            &[
                ("file", None, None),
                ("message", Some("+1555"), Some("quote")),
            ],
        );
        assert_eq!(
            differences(&want, JobState::Ok, &log, ""),
            vec![r#"missing effect: message +1555 with a summary containing "quote""#]
        );
    }

    #[test]
    fn an_effect_nothing_expected_is_extra_and_a_target_or_summary_mismatch_is_missing() {
        let log = [logged("file", "a.txt", "wrote"), logged("row", "t", "r")];
        let want = expecting("ok", &[("file", Some("b.txt"), None)]);
        let d = differences(&want, JobState::Ok, &log, "");
        assert_eq!(
            d,
            vec![
                "missing effect: file b.txt".to_string(),
                "extra effect: file a.txt: wrote".to_string(),
                "extra effect: row t: r".to_string(),
            ]
        );
    }

    #[test]
    fn one_logged_effect_satisfies_one_expected_effect() {
        let log = [logged("file", "a.txt", "wrote")];
        let want = expecting("ok", &[("file", None, None), ("file", None, None)]);
        assert_eq!(
            differences(&want, JobState::Ok, &log, ""),
            vec!["missing effect: file"]
        );
    }

    #[test]
    fn a_wrong_state_comes_first_and_says_what_ended_the_run() {
        let want = expecting("ok", &[("file", None, None)]);
        let d = differences(&want, JobState::Failed, &[], "appended: exit 1");
        assert_eq!(
            d,
            vec![
                "wrong state: expected ok, got failed (appended: exit 1)".to_string(),
                "missing effect: file".to_string(),
            ]
        );
    }

    #[test]
    fn load_fixtures_errors_when_the_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_fixtures(dir.path(), "nope").is_err());
    }
}
