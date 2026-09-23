use crate::support::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[test]
fn operations_run_in_order_and_appear_as_rows() {
    let e = Env::new();
    // The built-in direct workflow is setup → code; add a user operation after code.
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/stamp.toml"),
        "name = \"stamp\"\nkind = \"operation\"\ndescription = \"proves the change was made\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"grep -qx 42 answer.txt && echo stamped\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/stamped.toml"),
        "name = \"stamped\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"stamp\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "stamped",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let ops: Vec<(String, bool, bool, i64)> = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["name"].as_str().unwrap().to_string(),
                o["kernel"].as_bool().unwrap(),
                o["ok"].as_bool().unwrap(),
                o["seq"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            ("clone".to_string(), true, true, 0),
            ("setup".to_string(), false, true, 1),
            ("verify".to_string(), true, true, 2),
            ("stamp".to_string(), false, true, 3),
            ("push".to_string(), true, true, 4),
        ],
        "{ops:?}"
    );
    let setup = &doc["ops"][1];
    assert!(
        setup["detail"]
            .as_str()
            .unwrap()
            .contains("declares no check named"),
        "the test repo has no setup check, so it is skipped and says so"
    );
    assert_eq!(doc["resolved"]["steps"][2]["action"]["name"], "stamp");
    assert_eq!(
        doc["resolved"]["pins"].as_array().unwrap().len(),
        4,
        "workflow + setup + code + stamp"
    );
    assert!(doc["resolved"]["pins"][0]["hash"].as_str().unwrap().len() == 40);
    assert_eq!(doc["attempts"][0]["step"], "code");

    // A failing operation fails the task, without retrying the directive.
    std::fs::write(
        e.home.join("workflows/actions/stamp.toml"),
        "name = \"stamp\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"echo boom; exit 3\"]\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "stamped",
        ],
    );
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(2);
    assert_eq!(state, "failed");
    assert_eq!(reason, "operation stamp failed: boom");
    assert!(!pushed);
    assert_eq!(
        e.attempts(2).len(),
        1,
        "the directive is not retried for an operation failure"
    );
    let o = e.forge("ok.sh", &["show", "2"]);
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("actions/stamp.toml"),
        "the diagnosis names the file to fix"
    );

    // A timed-out operation says so in its reason.
    std::fs::write(
        e.home.join("workflows/actions/stamp.toml"),
        "name = \"stamp\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"sleep\", \"5\"]\ntimeout_secs = 1\n",
    )
    .unwrap();
    let start = Instant::now();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "stamped",
            "--retries",
            "0",
        ],
    );
    assert!(!o.status.success());
    assert!(start.elapsed() < Duration::from_secs(60));
    assert_eq!(e.task(3).1, "operation stamp failed: timed out after 1s");
}

#[test]
fn a_verifying_operation_sends_its_failure_back_to_the_coder() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/needs-extra.toml"),
        "name = \"needs-extra\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"test -f extra.txt || { echo 'extra.txt is missing'; exit 1; }\"]\nverifies = true\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/checked.toml"),
        "name = \"checked\"\ndescription = \"d\"\nsteps = [{ action = \"code\" }, { action = \"needs-extra\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "feedbackcoder.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "checked",
            "--retries",
            "1",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "the coder ran twice");
    assert!(a.iter().all(|x| x.1 == "succeeded"));
    assert!(
        e.log_text(1, 2).contains("extra.txt is missing"),
        "the operation's output reached the coder"
    );
    let doc: serde_json::Value = e.trace_json("1");
    let ops: Vec<(String, bool)> = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["name"].as_str().unwrap().to_string(),
                o["ok"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            ("clone".into(), true),
            ("verify".into(), true),
            ("needs-extra".into(), false),
            ("verify".into(), true),
            ("needs-extra".into(), true),
            ("push".into(), true)
        ],
        "{ops:?}"
    );
    assert!(e.task(1).2, "pushed");
    let ops = doc["ops"].as_array().unwrap();
    assert!(
        ops[2]["output"]
            .as_str()
            .unwrap()
            .contains("extra.txt is missing"),
        "a failing operation's output is kept: {}",
        ops[2]
    );
    assert!(
        ops[4]["output"].as_str().unwrap().is_empty() || ops[4]["ok"] == true,
        "a passing operation keeps whatever it printed"
    );

    // A coder that is right but never adds extra.txt runs out of attempts.
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 again",
            "--workflow",
            "checked",
            "--retries",
            "0",
        ],
    );
    assert!(!o.status.success());
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(
        reason.starts_with(
            "operation needs-extra (verifies) failed after 1 attempt(s): extra.txt is missing"
        ),
        "{reason}"
    );
    let o = e.forge("ok.sh", &["show", "2"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("kept failing after every attempt"));
}

#[test]
fn an_overlaying_operation_sees_the_hidden_suite() {
    let e = Env::new();
    tdd_repo(&e);
    git(&e.repo, &["checkout", "-q", "--orphan", "forge-verify"]);
    git(&e.repo, &["rm", "-rfq", "--cached", "."]);
    std::fs::create_dir_all(e.repo.join("tests/acceptance")).unwrap();
    std::fs::write(
        e.repo.join("tests/acceptance/hidden.sh"),
        "#!/bin/bash\ngrep -qx 42 answer.txt\n",
    )
    .unwrap();
    git(&e.repo, &["add", "tests/acceptance"]);
    git(&e.repo, &["commit", "-qm", "hidden suite"]);
    git(&e.repo, &["checkout", "-qf", "main"]);
    std::fs::remove_dir_all(e.repo.join("tests")).ok();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/hidden-e2e.toml"),
        "name = \"hidden-e2e\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"test -f tests/acceptance/hidden.sh && bash tests/acceptance/hidden.sh\"]\noverlay = true\nverifies = true\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/e2e.toml"),
        "name = \"e2e\"\ndescription = \"d\"\nsteps = [{ action = \"code\" }, { action = \"hidden-e2e\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "e2e",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("overlay  1 file(s) from forge-verify@") && err.contains(" for hidden-e2e"),
        "{err}"
    );
    assert!(
        !e.home
            .join("worktrees/1/tests/acceptance/hidden.sh")
            .exists(),
        "removed after the operation"
    );
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(doc["ops"][2]["name"], "hidden-e2e");
    assert_eq!(doc["ops"][2]["ok"], true);
}

#[test]
fn a_context_operation_shows_the_coder_where_things_are_unless_told_not_to() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/where.toml"),
        "name = \"where\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nproduces = [\"context\"]\nrun = [\"bash\", \"-c\", \"echo \\\"hello.sh: greet (task: $FORGE_TASK) bin=$FORGE_BIN_DIR\\\"\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/ctx.toml"),
        "name = \"ctx\"\ndescription = \"d\"\nsteps = [{ action = \"where\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "ctx",
            "--retries",
            "0",
            "--no-land",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("context  1 line(s) from where"));
    let prompt = e.log_text(1, 1);
    assert!(prompt.contains("Where things are"), "{prompt}");
    assert!(
        prompt.contains("hello.sh: greet (task: write 42)"),
        "the operation saw the task: {prompt}"
    );
    let doc: serde_json::Value = e.trace_json("1");
    assert!(
        doc["attempts"][0]["inputs"]["context"]
            .as_str()
            .unwrap()
            .contains("hello.sh: greet")
    );
    assert!(doc["task"]["context"].as_str().unwrap().contains("bin="));
    // The control arm runs the operation and shows nothing.
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 again",
            "--workflow",
            "ctx",
            "--retries",
            "0",
            "--no-land",
            "--no-context",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!e.log_text(2, 1).contains("Where things are"));
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["task"]["context_enabled"], false);
    assert!(doc["attempts"][0]["inputs"]["context"].is_null());
}

#[test]
fn operations_are_told_the_task_facts_and_diff_size_caps_the_change() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    // An operation that checks every fact it is handed, with git against base.
    std::fs::write(
        e.home.join("workflows/actions/facts.toml"),
        "name = \"facts\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"set -e; [[ $FORGE_TASK_ID =~ ^[0-9]+$ ]]; test \\\"$FORGE_WORKFLOW\\\" = capped; test \\\"$FORGE_STEP\\\" = facts; test \\\"$FORGE_BASE_BRANCH\\\" = main; [[ $FORGE_BRANCH == forge/$FORGE_TASK_ID-* ]]; test \\\"$FORGE_NAMESPACE\\\" = ''; git diff --quiet \\\"$FORGE_BASE_SHA\\\" -- hello.sh; ! git diff --quiet \\\"$FORGE_BASE_SHA\\\" -- answer.txt; test -z \\\"$FORGE_HOME\\\"\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/capped.toml"),
        "name = \"capped\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"facts\" }, { action = \"diff-size\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "capped",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let ops = doc["ops"].as_array().unwrap();
    let facts = ops.iter().find(|o| o["name"] == "facts").unwrap();
    assert_eq!(facts["ok"], true, "{}", facts["detail"]);
    let size = ops.iter().find(|o| o["name"] == "diff-size").unwrap();
    assert_eq!(size["ok"], true, "{}", size["detail"]);
    assert_eq!(e.task(1).0, "succeeded");

    // The caps are the last two elements of `run`; a cap of zero lines fails
    // with the measurement as the reason.
    let p = e.home.join("workflows/actions/diff-size.toml");
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("\"800\", \"25\"]"), "{text}");
    std::fs::write(&p, text.replace("\"800\", \"25\"]", "\"0\", \"25\"]")).unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "capped",
            "--retries",
            "0",
        ],
    );
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(2);
    assert_eq!(state, "failed");
    assert_eq!(
        reason,
        "operation diff-size failed: 1 file(s), 1 line(s) changed against base (cap 25 files, 0 lines)"
    );
    assert!(!pushed);
}

#[test]
fn an_operation_sees_prev_sha_verify_ref_hot_files_and_cache_dir() {
    let e = Env::new();
    // A first task reads hello.sh, so it becomes a hot file for the next one.
    assert!(e.run("tooly.sh", &["--retries", "0"]).status.success());
    tdd_repo(&e);
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/envs.toml"),
        "name = \"envs\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"verify_ref\"]\nrun = [\"bash\", \"-c\", \"echo PREV=$FORGE_PREV_SHA REF=$FORGE_VERIFY_REF HOT=$FORGE_HOT_FILES CACHE=$FORGE_CACHE_DIR\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/env-wf.toml"),
        "name = \"env-wf\"\ndescription = \"d\"\nsteps = [{ action = \"tests\" }, { action = \"setup\" }, { action = \"code\" }, { action = \"envs\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_TESTS", "testwriter.sh")],
        "env-wf",
        "write 42 to answer.txt",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("2");
    let envs_op = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "envs")
        .unwrap();
    assert_eq!(envs_op["ok"], true, "{}", envs_op["detail"]);
    let out = envs_op["output"].as_str().unwrap();
    let prev = out
        .split("PREV=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap();
    assert_eq!(
        prev.len(),
        40,
        "FORGE_PREV_SHA should be a commit sha: {out}"
    );
    assert!(out.contains("REF=verify/2"), "{out}");
    assert!(out.contains("HOT=hello.sh"), "{out}");
    // FORGE_CACHE_DIR is this repository's own directory under `cache/`
    // (src/ctx.rs, `Forge::cache_dir`), not the shared top-level one, so
    // another repository's attempts can never poison what this one reads.
    let cache_prefix = format!("{}/", e.home.join("cache").display());
    let cache_val = out
        .split("CACHE=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or_else(|| panic!("no CACHE= in {out}"));
    assert!(
        cache_val.starts_with(&cache_prefix),
        "expected a repository cache dir under {cache_prefix}, got {cache_val} ({out})"
    );
    assert!(std::path::Path::new(cache_val).is_dir());
}

#[test]
fn a_mutating_operation_is_committed_and_verified_by_the_kernel() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let stamp = e.home.join("workflows/actions/stamp.toml");
    let action = |cmd: &str| {
        format!(
            "name = \"stamp\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\nrun = [\"bash\", \"-c\", {}]\n",
            serde_json::to_string(cmd).unwrap()
        )
    };
    std::fs::write(&stamp, action("echo '# stamped' >> hello.sh")).unwrap();
    std::fs::write(
        e.home.join("workflows/stamped.toml"),
        "name = \"stamped\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"stamp\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let run = |task: &str| {
        e.forge(
            "ok.sh",
            &[
                "run",
                "--no-land",
                e.repo.to_str().unwrap(),
                task,
                "--workflow",
                "stamped",
                "--retries",
                "0",
            ],
        )
    };

    // Changed the tree: committed as Forge, verified, pushed.
    let o = run("write 42 to answer.txt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let ops: Vec<(String, bool, bool, i64)> = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["name"].as_str().unwrap().to_string(),
                o["kernel"].as_bool().unwrap(),
                o["ok"].as_bool().unwrap(),
                o["seq"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            ("clone".to_string(), true, true, 0),
            ("setup".to_string(), false, true, 1),
            ("verify".to_string(), true, true, 2),
            ("stamp".to_string(), false, true, 3),
            ("verify".to_string(), true, true, 3),
            ("push".to_string(), true, true, 4),
        ],
        "{ops:?}"
    );
    assert!(
        doc["ops"][4]["detail"]
            .as_str()
            .unwrap()
            .starts_with("1 file(s) committed as "),
        "{}",
        doc["ops"][4]["detail"]
    );
    let branch = doc["task"]["branch"].as_str().unwrap().to_string();
    let base = doc["task"]["base_sha"].as_str().unwrap().to_string();
    let wt = PathBuf::from(doc["task"]["worktree"].as_str().unwrap());
    let log = git(&wt, &["log", "--format=%s", &format!("{base}..HEAD")]);
    assert_eq!(
        log, "forge: stamp\nanswer",
        "the operation's commit is on the branch, after the agent's"
    );
    assert!(e.origin_branches().contains(&branch));
    let hello = git(&e.origin, &["show", &format!("{branch}:hello.sh")]);
    assert!(hello.ends_with("# stamped"), "{hello}");
    assert!(e.task(1).2, "pushed");

    // Changed nothing: nothing committed, nothing to verify, still a success.
    std::fs::write(&stamp, action("true")).unwrap();
    assert!(run("write 42 to answer.txt").status.success());
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["ops"][4]["name"], "verify");
    assert_eq!(
        doc["ops"][4]["detail"],
        "no changes; the verified tree stands"
    );
    let wt = PathBuf::from(doc["task"]["worktree"].as_str().unwrap());
    let base = doc["task"]["base_sha"].as_str().unwrap().to_string();
    assert_eq!(
        git(&wt, &["log", "--format=%s", &format!("{base}..HEAD")]),
        "answer"
    );

    // Broke a check: the task fails on the verify row, the commit stays for
    // inspection, nothing is pushed, and the diagnosis says which.
    std::fs::write(&stamp, action("echo 'if' > hello.sh")).unwrap();
    let o = run("write 42 to answer.txt");
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(3);
    assert_eq!(state, "failed");
    assert_eq!(reason, "operation stamp failed: L1 failed: shell");
    assert!(!pushed);
    let doc: serde_json::Value = e.trace_json("3");
    assert_eq!(doc["ops"][4]["name"], "verify");
    assert_eq!(doc["ops"][4]["ok"], false);
    let wt = PathBuf::from(doc["task"]["worktree"].as_str().unwrap());
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "forge: stamp");
    let o = e.forge("ok.sh", &["show", "3"]);
    assert!(
        String::from_utf8_lossy(&o.stdout)
            .contains("changed the tree and the result failed verification"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert_eq!(
        e.attempts(3).len(),
        1,
        "no retry for an operation's failure"
    );
}

#[test]
fn an_operation_can_extract_the_interface_from_the_hidden_tests() {
    let e = Env::new();
    tdd_repo(&e);
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/tdd-fact.toml"),
        "name = \"tdd-fact\"\ndescription = \"d\"\nsteps = [{ action = \"tests\" }, { action = \"interface\" }, { action = \"setup\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = run_wf(
        &e,
        "promptdump.sh",
        &[("FORGE_CLAUDE_BIN_TESTS", "testwriter.sh")],
        "tdd-fact",
        "write 42 to answer.txt",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let iface = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "interface")
        .unwrap();
    assert_eq!(iface["ok"], true, "{}", iface["detail"]);
    let out = iface["output"].as_str().unwrap();
    assert!(out.contains("== tests/acceptance/answer.sh"), "{out}");
    assert!(
        out.contains("Hidden tests, under tests/acceptance/"),
        "{out}"
    );
    // The fact replaces the claim as the interface the coder is shown; the
    // claim is still on the tests attempt's record.
    assert_eq!(doc["task"]["interface"], out);
    assert!(
        doc["attempts"][0]["outputs"]["interface"]
            .as_str()
            .unwrap()
            .contains("Trailing whitespace"),
        "{}",
        doc["attempts"][0]["outputs"]
    );
    let code = &doc["attempts"][1];
    assert_eq!(code["step"], "code");
    assert_eq!(code["inputs"]["interface"], out);
    let prompt = e.log_text(1, 2);
    assert!(
        prompt.contains("== tests/acceptance/answer.sh"),
        "the coder saw the extracted interface"
    );
    assert!(
        !prompt.contains("Trailing whitespace"),
        "and not the agent's summary"
    );
    // The scratch is gone and the coder's clone never held the tests.
    let wt = doc["task"]["worktree"].as_str().unwrap();
    assert!(!Path::new(&format!("{wt}-op")).exists());
    assert!(!Path::new(wt).join("tests/acceptance").exists());
    assert!(e.task(1).2, "pushed");
}

#[test]
fn an_operation_with_output_full_keeps_the_whole_thing_instead_of_the_tail() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let print100 = "for i in $(seq 1 100); do echo \"line $i\"; done";
    std::fs::write(
        e.home.join("workflows/actions/loud-tail.toml"),
        format!(
            "name = \"loud-tail\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"bash\", \"-c\", {}]\n",
            serde_json::to_string(print100).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/actions/loud-full.toml"),
        format!(
            "name = \"loud-full\"\nkind = \"operation\"\ndescription = \"d\"\noutput = \"full\"\nrun = [\"bash\", \"-c\", {}]\n",
            serde_json::to_string(print100).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/loud.toml"),
        "name = \"loud\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"loud-tail\" }, { action = \"loud-full\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "loud",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let ops = doc["ops"].as_array().unwrap();
    let tail_op = ops.iter().find(|o| o["name"] == "loud-tail").unwrap();
    let full_op = ops.iter().find(|o| o["name"] == "loud-full").unwrap();
    assert!(tail_op["ok"].as_bool().unwrap(), "{}", tail_op["detail"]);
    assert!(full_op["ok"].as_bool().unwrap(), "{}", full_op["detail"]);

    // Default `output = "tail"`: only the last 40 lines survive.
    let tail_out = tail_op["output"].as_str().unwrap();
    assert_eq!(tail_out.lines().count(), 40, "{tail_out}");
    assert_eq!(tail_out.lines().next().unwrap(), "line 61");
    assert_eq!(tail_out.lines().last().unwrap(), "line 100");

    // `output = "full"`: the whole 100 lines are kept.
    let full_out = full_op["output"].as_str().unwrap();
    assert_eq!(full_out.lines().count(), 100, "{full_out}");
    assert_eq!(full_out.lines().next().unwrap(), "line 1");
    assert_eq!(full_out.lines().last().unwrap(), "line 100");
}
