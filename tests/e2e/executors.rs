use crate::support::*;

fn executor_inputs(e: &Env) -> serde_json::Value {
    let text: String = e
        .db()
        .query_row(
            "SELECT inputs_json FROM attempts WHERE task_id=1 AND step='code'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn host_executor_runs_unsandboxed_and_records_guarantees() {
    let e = Env::new();
    // The check requires a file outside the worktree, invisible to bwrap.
    let canary = e.home.join("host-only");
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(&canary, "host").unwrap();
    let path = e.repo.join("forge.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{config}host = [\"test\", \"-f\", {:?}]\n[execution]\nbackend = \"host\"\n",
            canary.to_str().unwrap()
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "select host executor"]);
    // An uncommitted configuration cannot change the trusted executor.
    let trusted = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        trusted.replace("backend = \"host\"", "backend = \"bwrap\""),
    )
    .unwrap();
    let output = e
        .cmd("ok.sh")
        .env("FORGE_SANDBOX", "1")
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("sandboxed"));
    let inputs = executor_inputs(&e);
    assert_eq!(inputs["executor"], "host");
    assert_eq!(
        inputs["guarantees"],
        serde_json::json!({
            "worktree_private": false, "egress_bounded": false,
            "credentials_seeded": false, "checks_under_kernel_control": true,
        })
    );
    std::fs::write(&path, trusted).unwrap();
    let output = e
        .cmd("ok.sh")
        .env("FORGE_SANDBOX", "1")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows.iter().find(|r| r["name"] == "executors.host").unwrap();
    assert_eq!(row["status"], "warn");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("egress is unbounded")
    );
}

#[test]
fn default_executor_is_bwrap_and_records_guarantees() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    assert!(e.run("ok.sh", &[]).status.success());
    let inputs = executor_inputs(&e);
    assert_eq!(inputs["executor"], "bwrap");
    assert_eq!(
        inputs["guarantees"],
        serde_json::json!({
            "worktree_private": true, "egress_bounded": true,
            "credentials_seeded": true, "checks_under_kernel_control": true,
        })
    );
    let output = e.forge("ok.sh", &["doctor", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.iter().any(|r| r["name"] == "executors.bwrap"));
}

#[test]
fn review_host_repo_operation_on_verify_ref_runs_on_host() {
    let e = Env::new();
    tdd_repo(&e);
    let canary = e.home.join("host-only");
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(&canary, "host").unwrap();
    let path = e.repo.join("forge.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{config}[execution]\nbackend = \"host\"\n")).unwrap();
    git(&e.repo, &["commit", "-qam", "select host executor"]);
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    for (name, consumes) in [("onbranch", "branch"), ("onref", "verify_ref")] {
        std::fs::write(
            e.home.join(format!("workflows/actions/{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"{consumes}\"]\nrun = [\"bash\", \"-c\", \"test -f {} && echo HOSTVISIBLE\"]\n",
                canary.display()
            ),
        )
        .unwrap();
    }
    std::fs::write(
        e.home.join("workflows/hostops.toml"),
        "name = \"hostops\"\ndescription = \"d\"\nsteps = [{ action = \"tests\" }, { action = \"setup\" }, { action = \"code\" }, { action = \"onbranch\" }, { action = \"onref\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = run_wf(
        &e,
        "ok.sh",
        &[
            ("FORGE_CLAUDE_BIN_TESTS", "testwriter.sh"),
            ("FORGE_SANDBOX", "1"),
        ],
        "hostops",
        "write 42 to answer.txt",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    for name in ["onbranch", "onref"] {
        let op = doc["ops"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap();
        assert_eq!(op["ok"], true, "{name}: {op}");
        assert!(
            op["output"].as_str().unwrap().contains("HOSTVISIBLE"),
            "{name}"
        );
    }
}
