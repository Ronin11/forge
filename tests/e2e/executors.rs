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

#[test]
fn ssh_executor_syncs_runs_and_retains_an_unverified_branch() {
    assert_ssh_attempt(0);
}

#[test]
fn ssh_executor_syncs_back_after_a_failed_remote_command() {
    assert_ssh_attempt(7);
}

fn ssh_fixture() -> (Env, std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let bin = e._dir.path().join("transport");
    std::fs::create_dir_all(&bin).unwrap();
    let write = |name: &str, script: &str| {
        let path = bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let real_rsync = std::process::Command::new("sh")
        .args(["-c", "command -v rsync"])
        .output()
        .unwrap();
    assert!(real_rsync.status.success(), "rsync is required");
    write(
        "rsync",
        &format!(
            r#"#!/bin/bash
args=()
for arg in "$@"; do
    case "$arg" in *@remote:*) args+=("${{arg#*:}}");; *) args+=("$arg");; esac
done
exec {} "${{args[@]}}"
"#,
            String::from_utf8(real_rsync.stdout).unwrap().trim()
        ),
    );
    write(
        "ssh",
        r#"#!/bin/bash
while [ "$1" = "-o" ]; do shift 2; done
test "$1" = "forge@remote" || exit 91
shift
if [ "$*" = "claude --version" ]; then echo 'claude fake'; exit 0; fi
case "$*" in mktemp*) cat >/dev/null;; esac
exec /bin/sh -c "$*"
"#,
    );
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/ok.sh");
    let remote_agent = write(
        "remote-agent",
        &format!(
            r#"#!/bin/bash
read -r first_line
test -n "$first_line" || exit 95
case "$PWD" in /tmp/forge-executor.*) ;; *) exit 92;; esac
test "$(cat outbound.txt)" = "synced to remote" || exit 93
test "$ANTHROPIC_SSH_TEST" = "spaces and 'quotes' \$dollars" || exit 94
rm outbound.txt
printf '%s' "$PWD" > remote-location.txt
{}
exit "$ANTHROPIC_EXIT"
"#,
            fake.display()
        ),
    );
    let config_path = e.repo.join("forge.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        &config_path,
        format!("{config}[execution]\nbackend = \"ssh\"\nhost = \"remote\"\nuser = \"forge\"\n"),
    )
    .unwrap();
    std::fs::write(e.repo.join("outbound.txt"), "synced to remote").unwrap();
    git(&e.repo, &["add", "."]);
    git(&e.repo, &["commit", "-qm", "select remote executor"]);
    (e, bin, remote_agent)
}

fn assert_ssh_attempt(exit: i32) {
    let (e, bin, remote_agent) = ssh_fixture();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("FORGE_SANDBOX", "1")
        .env("FORGE_CLAUDE_BIN", &remote_agent)
        .env("ANTHROPIC_SSH_TEST", "spaces and 'quotes' $dollars")
        .env("ANTHROPIC_EXIT", exit.to_string())
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(
        e.task(1).0,
        "unverified",
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let reason: String = e
        .db()
        .query_row("SELECT reason FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        reason,
        "remote executor: the kernel could not run the checks itself"
    );
    let (state, worktree): (String, String) = e.db().query_row(
        "SELECT a.state, t.worktree FROM attempts a JOIN tasks t ON a.task_id=t.id WHERE t.id=1",
        [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(state, "unverified");
    let worktree = std::path::Path::new(&worktree);
    assert_eq!(
        std::fs::read_to_string(worktree.join("answer.txt")).unwrap(),
        "42\n"
    );
    assert!(!worktree.join("outbound.txt").exists());
    assert!(
        std::fs::read_to_string(worktree.join("remote-location.txt"))
            .unwrap()
            .starts_with("/tmp/forge-executor.")
    );
    assert!(
        !e.repo.join("answer.txt").exists(),
        "remote work must not land"
    );
    let inputs = executor_inputs(&e);
    assert_eq!(inputs["executor"], "ssh");
    assert_eq!(
        inputs["guarantees"],
        serde_json::json!({
            "worktree_private": false, "egress_bounded": false,
            "credentials_seeded": false, "checks_under_kernel_control": false
        })
    );
    let output = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("FORGE_SANDBOX", "1")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        rows.iter()
            .any(|r| r["name"] == "executors.ssh" && r["status"] == "warn")
    );
    assert!(rows.iter().any(|r| {
        r["detail"]
            .as_str()
            .unwrap()
            .contains("ssh forge@remote claude --version: answers")
    }));
    assert!(
        rows.iter().any(|r| r["status"] == "warn"
            && r["detail"].as_str().unwrap().contains("subscription login"))
    );
}
