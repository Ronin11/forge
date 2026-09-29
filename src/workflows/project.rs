//! Reading a run workflow straight from a project's own repository
//! (docs/JOBS.md, "Where an automation lives"): from a working copy
//! (`resolve_job_in_repo`, for `forge job bench`) or pinned to a commit
//! with no checkout (`resolve_job_at`, `load_all_at`, `fixtures_at`, for
//! `forge job start` and `forge workflows --project`).

use super::*;

/// A run workflow read straight from a project's own repository, at
/// `.forge/workflows/<name>.toml` with its actions under
/// `.forge/workflows/actions/` (docs/JOBS.md, "Where an automation
/// lives"), rather than the operator's catalog `resolve_job` reads. Every
/// sibling `.forge/workflows/*.toml` is parsed too, so a step that splices
/// in another run workflow (`job_steps`) resolves against the repository's
/// own. `forge job bench` uses this: it measures an automation that is
/// checked into the project it belongs to, not a built-in. No `ensure`:
/// this directory is the project's own and is never git-initialised or
/// seeded with built-ins the way the operator's catalog is.
pub fn resolve_job_in_repo(repo: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let dir = repo.join(".forge").join("workflows");
    let path = dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no {} (see docs/JOBS.md)", path.display()))?;
    let wf = parse_workflow(&path, &text, blob_hash(repo, &path)?)?;
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job bench` runs kind = \"run\" workflows only");
    }
    let actions_dir = dir.join("actions");
    let mut actions = BTreeMap::new();
    for p in
        toml_files(&actions_dir).with_context(|| format!("reading {}", actions_dir.display()))?
    {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let a = parse_action(&p, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let mut workflows = BTreeMap::new();
    for p in toml_files(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let w = parse_workflow(&p, &text, hash)?;
        workflows.insert(w.name.clone(), w);
    }
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok((wf, steps))
}

/// One blob at `rev` under `dir` in `repo`: its path relative to the
/// repository root and the git blob hash `git ls-tree` already carries —
/// the same content-addressed identity `blob_hash` derives from a working
/// copy (docs/WORKFLOWS.md, "Identity is the git blob hash of the file"),
/// here read straight out of the commit with no checkout.
fn ls_tree_dir(repo: &Path, rev: &str, dir: &str) -> Result<Vec<(PathBuf, String)>> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", rev, "--", dir])
        .output()?;
    if !o.status.success() {
        bail!(
            "git ls-tree {rev} -- {dir} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let hash = meta.split_whitespace().nth(2)?;
            Some((PathBuf::from(path), hash.to_string()))
        })
        .collect())
}

/// The bytes of one file at `rev` in `repo`, read with `git show`, no
/// checkout.
fn show_at(repo: &Path, rev: &str, path: &Path) -> Result<String> {
    let rel = path.to_str().context("path is not UTF-8")?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{rev}:{rel}")])
        .output()?;
    if !o.status.success() {
        bail!(
            "git show {rev}:{rel} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Where a job's workflow was resolved from (docs/JOBS.md, "Where an
/// automation lives"): the project's own repository at its pinned commit,
/// tried first, or the operator's catalog when the repository holds no
/// workflow of that name there. Recorded on the job (`Job::workflow_source`)
/// so `forge job show` says which it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobSource {
    Repo,
    Catalog,
}

impl JobSource {
    pub fn as_str(self) -> &'static str {
        match self {
            JobSource::Repo => "repo",
            JobSource::Catalog => "catalog",
        }
    }
}

/// Every workflow under `.forge/workflows/*.toml` in a project's own
/// repository at `rev`, read with `git show` (no checkout, no working
/// tree) — for `forge workflows --project` (docs/JOBS.md, "Where an
/// automation lives"). Actions under `.forge/workflows/actions/` are not
/// themselves workflows and are skipped.
pub fn load_all_at(repo: &Path, rev: &str) -> Result<Vec<Workflow>> {
    let wf_dir = Path::new(".forge/workflows");
    let mut out = Vec::new();
    for (path, hash) in ls_tree_dir(repo, rev, wf_dir.to_str().unwrap())? {
        if path.parent() != Some(wf_dir) || path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        out.push(parse_workflow(&path, &text, hash)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A run workflow resolved straight from a project's own repository at a
/// pinned commit — `.forge/workflows/<name>.toml`, read with `git show`,
/// no checkout — the same content-addressed pinning the operator's
/// catalog uses (docs/JOBS.md, "Where an automation lives"). Its steps
/// resolve against the operator's own action catalog (built-ins and
/// anything the operator has added) overlaid by the project's own
/// `.forge/workflows/actions/*.toml`: an automation names a built-in
/// effect operation directly, the way docs/JOBS.md's own example does, and
/// only needs a file of its own for an action the catalog does not have.
/// Every sibling `.forge/workflows/*.toml` at the same commit (`load_all_at`)
/// is parsed too, so a step that splices in another run workflow
/// (`job_steps`) resolves against the repository's own. `None` when the
/// repository has no workflow of this name at that commit, so
/// `resolve_job_for_project` falls back to the operator's catalog
/// entirely.
pub fn resolve_job_at(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<Option<(Workflow, Vec<RunStep>)>> {
    let wf_path = Path::new(".forge/workflows").join(format!("{name}.toml"));
    let Some((path, hash)) = ls_tree_dir(repo, rev, wf_path.to_str().unwrap())?
        .into_iter()
        .find(|(p, _)| p == &wf_path)
    else {
        return Ok(None);
    };
    let text = show_at(repo, rev, &path)?;
    let wf = parse_workflow(&path, &text, hash)?;
    if wf.kind != WorkflowKind::Run {
        bail!(
            "{name:?} is kind = \"build\" in the project's repository; `forge job start` runs kind = \"run\" workflows only"
        );
    }
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let mut actions = cat.actions;
    for (path, hash) in ls_tree_dir(repo, rev, ".forge/workflows/actions")? {
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let a = parse_action(&path, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let workflows: BTreeMap<String, Workflow> = load_all_at(repo, rev)?
        .into_iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok(Some((wf, steps)))
}

/// A job's workflow, resolved the way `forge job start` does: first from
/// the project's own repository at `rev` (`resolve_job_at`), falling back
/// to the operator's catalog only when the repository has no workflow of
/// that name there (docs/JOBS.md, "Where an automation lives").
pub fn resolve_job_for_project(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<(Workflow, Vec<RunStep>, JobSource)> {
    if let Some((wf, steps)) = resolve_job_at(home, repo, rev, name)? {
        return Ok((wf, steps, JobSource::Repo));
    }
    let (wf, steps) = resolve_job(home, name)?;
    Ok((wf, steps, JobSource::Catalog))
}

/// Every fixture under `.forge/fixtures/<workflow>/*.json` in a project's
/// own repository at `rev`, read the same way `resolve_job_at` reads the
/// workflow itself — `git show`, no checkout (docs/JOBS.md, "Verifying an
/// automation"). `(name, contents)`
/// pairs, sorted by name. No caller: `forge job test` reads the working
/// tree instead (`job::load_fixtures`), so a check sees work in progress;
/// this is the same discovery pinned to a commit.
#[allow(dead_code)]
pub fn fixtures_at(repo: &Path, rev: &str, workflow: &str) -> Result<Vec<(String, String)>> {
    let dir = Path::new(".forge/fixtures").join(workflow);
    let mut out = Vec::new();
    for (path, _hash) in ls_tree_dir(repo, rev, dir.to_str().context("path is not UTF-8")?)? {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.push((name, text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    #[test]
    fn resolve_job_at_reads_a_run_workflow_from_the_repository_at_a_pinned_commit_merging_operator_and_project_actions()
     {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        git(r, &["init", "-q", "-b", "main"]);
        git(r, &["config", "user.email", "t@example.com"]);
        git(r, &["config", "user.name", "t"]);
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::create_dir_all(r.join(".forge/fixtures/publish-snapshot")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/fixtures/publish-snapshot/01-example.json"),
            "{\"a\":1}\n",
        )
        .unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-q", "-m", "automation"]);
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();

        let home = tempfile::tempdir().unwrap();
        load_all(home.path()).unwrap();

        let (wf, steps) = resolve_job_at(home.path(), r, &sha, "publish-snapshot")
            .unwrap()
            .unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        let names: Vec<&str> = steps.iter().map(|s| s.action.name.as_str()).collect();
        assert_eq!(names, vec!["write-file", "custom-op"]);

        // A name the repository does not have at that commit: no fallback
        // to the catalog inside `resolve_job_at` itself.
        assert!(
            resolve_job_at(home.path(), r, &sha, "no-such-workflow")
                .unwrap()
                .is_none()
        );

        let (_, _, source) =
            resolve_job_for_project(home.path(), r, &sha, "publish-snapshot").unwrap();
        assert_eq!(source, JobSource::Repo);
        // Falls to the operator's catalog when the repository has none by
        // that name.
        write(
            home.path(),
            "catalog-only.toml",
            r#"name = "catalog-only"
kind = "run"
description = "a run workflow the operator's catalog carries but the repository does not"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        );
        let (_, _, source) = resolve_job_for_project(home.path(), r, &sha, "catalog-only").unwrap();
        assert_eq!(source, JobSource::Catalog);

        let all = load_all_at(r, &sha).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["publish-snapshot"]
        );

        let fixtures = fixtures_at(r, &sha, "publish-snapshot").unwrap();
        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].0, "01-example");
        assert!(fixtures[0].1.contains("\"a\""));
    }
}
