//! `forge adopt`: a branch made by hand, verified as it is and landed
//! through the integrator with no agent run. Every adoption here runs
//! under `neverrun.sh`, a coder that fails loudly if anything invokes it.

use crate::support::*;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn origin_sha(e: &Env, branch: &str) -> String {
    let o = Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
        ])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A branch `name` off the repository's current main, with `files`
/// written and committed by hand; the checkout is back on main after.
fn hand_branch(repo: &Path, name: &str, files: &[(&str, &str)]) -> String {
    git(repo, &["checkout", "-q", "-b", name, "main"]);
    for (path, text) in files {
        std::fs::write(repo.join(path), text).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", &format!("{name} by hand")]);
    let sha = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "-q", "main"]);
    sha
}

/// main carries the answer and is on the remote: the adopted branches
/// below then only need to keep the checks green.
fn answered_base(e: &Env) {
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "answer"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
}

fn adopt(e: &Env, extra: &[&str]) -> Output {
    let mut args = vec!["adopt", e.repo.to_str().unwrap()];
    args.extend_from_slice(extra);
    e.forge("neverrun.sh", &args)
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn origin_of(e: &Env, id: i64) -> (String, String, String) {
    e.db()
        .query_row(
            "SELECT origin, adoption_json, workflow FROM tasks WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// No attempt of the task was an agent's: no turns, no cost, and only
/// the kernel's own steps.
fn assert_no_agent(e: &Env, id: i64) {
    let steps: Vec<(String, i64, f64)> = e
        .db()
        .prepare("SELECT step, num_turns, COALESCE(cost_usd, 0) FROM attempts WHERE task_id=?1")
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!steps.is_empty());
    for (step, turns, cost) in steps {
        assert!(step == "adopt" || step == "integrate", "{step}");
        assert_eq!((turns, cost), (0, 0.0));
    }
}

#[test]
fn an_adopted_branch_lands_on_the_base_as_it_is_with_no_agent_run() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let sha = hand_branch(&e.repo, "jetpack", &[("answer.txt", "42\n")]);
    git(&e.repo, &["push", "-q", "origin", "jetpack"]);
    let o = adopt(&e, &["jetpack", "--title", "Jetpack yields to wall"]);
    assert!(o.status.success(), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("landed task 1 on main @ "), "{out}");
    // A fast-forward: the base is now exactly the hand-made commit.
    assert_eq!(origin_sha(&e, "main"), sha);
    let last = out.lines().last().unwrap();
    assert!(last.contains("pull --ff-only origin main"), "{out}");
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    let (origin, adoption, workflow) = origin_of(&e, 1);
    assert_eq!((origin.as_str(), workflow.as_str()), ("adopted", "adopt"));
    let a: serde_json::Value = serde_json::from_str(&adoption).unwrap();
    assert_eq!(a["branch"], "jetpack");
    assert_eq!(a["commit"], sha.as_str());
    let title: String = e
        .db()
        .query_row("SELECT title FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Jetpack yields to wall");
    assert_no_agent(&e, 1);
    let ops = op_names(&e, 1);
    for name in ["verify", "integrate", "land", "push"] {
        assert!(ops.contains(&(name.to_string(), true)), "{ops:?}");
    }
    // Marked manual wherever a human reads it.
    let log = text(&e.forge("ok.sh", &["log"]));
    assert!(log.contains("manual"), "{log}");
    let show = text(&e.forge("ok.sh", &["show", "1"]));
    assert!(show.contains("adopted jetpack @ "), "{show}");
    assert!(show.contains("manual"), "{show}");
}

#[test]
fn a_branch_only_on_the_remote_is_adopted_from_there() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let sha = hand_branch(&e.repo, "elsewhere", &[("answer.txt", "42\n")]);
    git(&e.repo, &["push", "-q", "origin", "elsewhere"]);
    git(&e.repo, &["branch", "-q", "-D", "elsewhere"]);
    let o = adopt(&e, &["elsewhere"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(origin_sha(&e, "main"), sha);
}

#[test]
fn an_adopted_branch_that_fails_a_check_is_blocked_naming_it_and_not_landed() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    hand_branch(&e.repo, "wrong", &[("answer.txt", "41\n")]);
    let o = adopt(&e, &["wrong"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("answer"), "{}", text(&o));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(reason.contains("answer"), "{reason}");
    assert!(reason.contains("forge retry 1"), "{reason}");
    assert_eq!(origin_sha(&e, "main"), before, "nothing landed");
    assert_no_agent(&e, 1);
    let req = e.requests_json();
    let q = req[0]["question"].as_str().unwrap_or_default().to_string()
        + req[0]["tried"].as_str().unwrap_or_default();
    assert!(q.contains("answer"), "{req}");
    // A retry verifies the branch again, as it is now: fixed by hand, it lands.
    git(&e.repo, &["checkout", "-q", "wrong"]);
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["commit", "-qam", "fix by hand"]);
    let fixed = git(&e.repo, &["rev-parse", "HEAD"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let o = e.forge("neverrun.sh", &["retry", "1"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(origin_sha(&e, "main"), fixed);
    let retry_of: i64 = e
        .db()
        .query_row("SELECT retry_of FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(retry_of, 1);
    assert_no_agent(&e, 2);
    // An answer would hand it to an agent: refused.
    let o = e.forge("neverrun.sh", &["answer", "1", "just merge it"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("adopted"), "{}", text(&o));
}

#[test]
fn an_adopted_branch_editing_forge_toml_is_refused_without_allow_protected() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    hand_branch(
        &e.repo,
        "loosen",
        &[
            ("answer.txt", "42\n"),
            ("forge.toml", "[checks]\nshell = [\"true\"]\n"),
        ],
    );
    let o = adopt(&e, &["loosen"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("forge.toml"), "{}", text(&o));
    assert!(text(&o).contains("--allow-protected"), "{}", text(&o));
    let (state, _, _) = e.task(1);
    assert_eq!(state, "failed");
    assert_eq!(origin_sha(&e, "main"), before);
    // No check ran on a refused branch.
    let attempts: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM attempts WHERE task_id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(attempts, 0);
    let d = e.decisions_json();
    assert!(d.to_string().contains("refused"), "{d}");
    assert!(d.to_string().contains("forge.toml"), "{d}");
}

#[test]
fn allow_protected_records_the_decision_and_verifies() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    hand_branch(
        &e.repo,
        "protected",
        &[
            ("answer.txt", "42\n"),
            ("hello.sh", "#!/bin/bash\necho hi\n"),
        ],
    );
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n[verify]\nprotected = [\"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "protect hello.sh"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    git(&e.repo, &["checkout", "-q", "protected"]);
    git(&e.repo, &["merge", "-q", "main", "-m", "catch up"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let o = adopt(&e, &["protected"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("hello.sh"), "{}", text(&o));
    let o = adopt(&e, &["protected", "--allow-protected"]);
    assert!(o.status.success(), "{}", text(&o));
    let d = e.decisions_json();
    assert!(d.to_string().contains("allowed (--allow-protected)"), "{d}");
    assert_eq!(
        origin_file(&e, "main", "hello.sh").as_deref(),
        Some("#!/bin/bash\necho hi\n")
    );
}

/// The incident this guards against: an adoption of a one-line forge.toml
/// change with `--allow-protected` blocks on an unrelated check failure;
/// `forge retry` must still carry `--allow-protected` forward so landing
/// does not refuse it on `forge.toml-untouched` a second time.
#[test]
fn retrying_an_adopted_branch_carries_allow_protected_to_a_landing() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    let toml = "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n# shared-target-on\n";
    hand_branch(
        &e.repo,
        "toggle",
        &[("answer.txt", "41\n"), ("forge.toml", toml)],
    );
    let o = adopt(&e, &["toggle", "--allow-protected"]);
    assert!(!o.status.success(), "{}", text(&o));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(reason.contains("answer"), "{reason}");
    assert_eq!(origin_sha(&e, "main"), before, "nothing landed yet");
    let d = e.decisions_json();
    assert!(
        d.to_string().contains("allowed (--allow-protected)"),
        "the protected-path allowance is recorded even though the check failed: {d}"
    );

    // Fixed by hand, as an unrelated failure would be; the retry carries
    // --allow-protected forward on its own and lands.
    git(&e.repo, &["checkout", "-q", "toggle"]);
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["commit", "-qam", "fix by hand"]);
    let fixed = git(&e.repo, &["rev-parse", "HEAD"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let o = e.forge("neverrun.sh", &["retry", "1"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("carrying --allow-protected from task 1"),
        "{}",
        text(&o)
    );
    assert_eq!(origin_sha(&e, "main"), fixed);
    let (state, _, pushed) = e.task(2);
    assert_eq!(state, "succeeded");
    assert!(pushed);
    let allow_protected: i64 = e
        .db()
        .query_row("SELECT allow_protected FROM tasks WHERE id=2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(allow_protected, 1, "the retry inherited allow_protected");
    let d = e.decisions_json();
    let allowed = d
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| {
            x["answer"]
                .as_str()
                .unwrap_or_default()
                .contains("allowed (--allow-protected)")
        })
        .count();
    assert!(
        allowed >= 2,
        "both the original adoption and its retry record the allowance: {d}"
    );
}

/// Rule (2): a retry gains `--allow-protected` for an adoption that was
/// refused without it, never touching a check.
#[test]
fn forge_retry_allow_protected_lands_what_the_original_adoption_was_refused_for() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    let toml = "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n# shared-target-on\n";
    let sha = hand_branch(
        &e.repo,
        "toggle2",
        &[("answer.txt", "42\n"), ("forge.toml", toml)],
    );
    let o = adopt(&e, &["toggle2"]);
    assert!(!o.status.success(), "{}", text(&o));
    let (state, _, _) = e.task(1);
    assert_eq!(state, "failed");
    assert_eq!(
        origin_sha(&e, "main"),
        before,
        "refused before any check ran"
    );

    let o = e.forge("neverrun.sh", &["retry", "1", "--allow-protected"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("--allow-protected added; task 1 did not have it"),
        "{}",
        text(&o)
    );
    assert_eq!(origin_sha(&e, "main"), sha);
    let d = e.decisions_json();
    assert!(d.to_string().contains("allowed (--allow-protected)"), "{d}");
}

#[test]
fn an_adopted_branch_that_conflicts_with_the_moved_base_is_blocked_and_nothing_lands() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    hand_branch(
        &e.repo,
        "stale",
        &[
            ("answer.txt", "42\n"),
            ("hello.sh", "#!/bin/bash\necho hi\n"),
        ],
    );
    std::fs::write(e.repo.join("hello.sh"), "#!/bin/bash\necho howdy\n").unwrap();
    git(&e.repo, &["commit", "-qam", "base moves"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    let branch_before = git(&e.repo, &["rev-parse", "stale"]);
    let o = adopt(&e, &["stale"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("hello.sh"), "{}", text(&o));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(reason.contains("landing failed"), "{reason}");
    assert_eq!(origin_sha(&e, "main"), before, "no half-merged result");
    assert_eq!(git(&e.repo, &["rev-parse", "stale"]), branch_before);
    assert_no_agent(&e, 1);
}

#[test]
fn no_land_verifies_only_and_leaves_the_branch_for_a_human() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let before = origin_sha(&e, "main");
    let sha = hand_branch(&e.repo, "gated", &[("answer.txt", "42\n")]);
    let o = adopt(&e, &["gated", "--no-land"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("forge land 1"), "{}", text(&o));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.contains("left for a human"), "{reason}");
    assert_eq!(origin_sha(&e, "main"), before, "not landed");
    // The human lands it later through the same integrator.
    let o = e.forge("neverrun.sh", &["land", "1"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(origin_sha(&e, "main"), sha);
}

#[test]
fn adopted_tasks_stay_out_of_the_workflow_outcomes_and_count_as_manual() {
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    hand_branch(&e.repo, "hand", &[("answer.txt", "42\n")]);
    assert!(adopt(&e, &["hand"]).status.success());
    let o = e.forge("ok.sh", &["stats", "--json"]);
    assert!(o.status.success(), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let workflows = v["workflows"].to_string();
    assert!(!workflows.contains("\"adopt\""), "{workflows}");
    assert_eq!(v["manual"]["tasks"], 1, "{v}");
    assert_eq!(v["manual"]["landed"], 1, "{v}");
    let o = e.forge("ok.sh", &["stats"]);
    assert!(text(&o).contains("manual"), "{}", text(&o));
}

#[test]
fn two_adoptions_and_an_agent_task_landing_at_once_all_land_verified() {
    let e = Env::new();
    answered_base(&e);
    hand_branch(&e.repo, "one", &[("one.txt", "1\n")]);
    hand_branch(&e.repo, "two", &[("two.txt", "2\n")]);
    let spawn = |fake: &str, args: &[&str]| {
        let mut c = e.cmd(fake);
        c.args(args)
            .env("FAKE_SLEEP", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c.spawn().unwrap()
    };
    let repo = e.repo.to_str().unwrap();
    let agent = spawn("addfile.sh", &["run", repo, "add extra", "--retries", "0"]);
    let one = spawn("neverrun.sh", &["adopt", repo, "one"]);
    let two = spawn("neverrun.sh", &["adopt", repo, "two"]);
    for child in [agent, one, two] {
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", text(&o));
    }
    for path in ["extra.txt", "one.txt", "two.txt", "answer.txt"] {
        assert!(origin_file(&e, "main", path).is_some(), "{path} lost");
    }
    let main = origin_sha(&e, "main");
    git(&e.repo, &["fetch", "-q", "origin", "main"]);
    let landed: Vec<(i64, String, String)> = e
        .db()
        .prepare("SELECT id, landed_sha, origin FROM tasks ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(landed.len(), 3);
    for (id, sha, _) in &landed {
        assert!(!sha.is_empty(), "task {id} did not land");
        // Every landing re-verified what it pushed.
        let ops = op_names(&e, *id);
        assert!(ops.contains(&("integrate".into(), true)), "{id}: {ops:?}");
        assert!(
            git(&e.repo, &["merge-base", "--is-ancestor", sha, &main]).is_empty(),
            "task {id}'s landing is on main"
        );
    }
    assert_eq!(landed.iter().filter(|(_, _, o)| o == "adopted").count(), 2);
    // Whoever landed after another found the base moved: it merged the
    // base in and verified the merged tree before pushing.
    let merged: i64 = e
        .db()
        .query_row(
            "SELECT COUNT(*) FROM ops WHERE name='integrate' AND ok=1 AND detail LIKE 'merged main @ %verified against main%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(merged >= 1, "{merged} landing(s) merged the moved base");
}
