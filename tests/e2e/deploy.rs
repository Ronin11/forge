use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn deploy_targets_are_added_listed_and_forge_deploy_log_starts_empty() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    // No targets, no deploys, yet.
    let targets: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(targets.as_array().unwrap().len(), 0);

    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // Add two targets.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-user-service",
            "--arg",
            "unit=demo.service",
            "--check",
            "systemctl --user is-active demo.service",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "staging",
            "--repo",
            repo,
            "--scope",
            "web,api",
            "--method",
            "deploy-static",
            "--check",
            "curl -f https://staging.example.com/health",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // A duplicate (project, name) is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // A target for an unknown project is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "nope",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // `forge project deploy list --json` carries both, alphabetically.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["name"], "prod");
    assert_eq!(rows[0]["method"], "deploy-user-service");
    assert_eq!(rows[0]["args"]["unit"], "demo.service");
    assert_eq!(
        rows[0]["check_cmd"],
        "systemctl --user is-active demo.service"
    );
    assert_eq!(rows[0]["on_landing"], true);
    assert_eq!(rows[0]["scope"], serde_json::Value::Null);
    assert_eq!(rows[1]["name"], "staging");
    assert_eq!(rows[1]["method"], "deploy-static");
    assert_eq!(rows[1]["on_landing"], false);
    assert_eq!(rows[1]["scope"], "[\"web\",\"api\"]");

    // The text form lists both too.
    let out = String::from_utf8_lossy(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo"])
            .stdout,
    )
    .to_string();
    assert!(out.contains("prod"), "{out}");
    assert!(out.contains("staging"), "{out}");

    // No deploy has run yet: the log is still empty.
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // A target naming an unknown method is refused before any deploy row
    // starts, naming the method.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "ghost",
            "--repo",
            repo,
            "--method",
            "deploy-nonexistent",
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.forge("ok.sh", &["deploy", "demo", "ghost"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.contains("deploy-nonexistent"), "{err}");
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "ghost", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // A nonexistent target is refused.
    let bad = e.forge("ok.sh", &["deploy", "demo", "nope"]);
    assert!(!bad.status.success());
}

/// A fake `rsync`: records the call, then hands it to the real `rsync`
/// with the destination's `host:` label stripped off (the host is only
/// ever a label here, never dialled), so flags like `--delete` and
/// `--exclude` behave exactly as they do against a real target.
/// `{REAL_RSYNC}` is filled in by `write_fake_rsync` with the system
/// `rsync`'s own path, resolved before PATH is overridden with this
/// fake's directory (so the fake does not just call itself).
const FAKE_RSYNC_TEMPLATE: &str = r#"#!/bin/bash
echo "rsync $*" >> "$HOME/deploy-calls.log"
argv=()
for a in "$@"; do
  case "$a" in
    -*) argv+=("$a") ;;
    *:*) argv+=("${a#*:}") ;;
    *) argv+=("$a") ;;
  esac
done
dest="${argv[@]: -1}"
mkdir -p "$dest"
"{REAL_RSYNC}" "${argv[@]}"
"#;

/// Write the fake `rsync` to `path`, resolved against the real system
/// `rsync` found on the current `PATH`.
fn write_fake_rsync(path: &Path) {
    let real = String::from_utf8(
        std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v rsync")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let script = FAKE_RSYNC_TEMPLATE.replace("{REAL_RSYNC}", real.trim());
    write_fake(path, &script);
}

/// A fake `ssh`: records the call, then runs the remote command locally,
/// ignoring the host, exactly as docs/DEPLOY.md's build order describes
/// testing a method: "a fake ssh on PATH" reaching "a directory on the
/// same machine".
const FAKE_SSH: &str = r#"#!/bin/bash
echo "ssh $1 $2" >> "$HOME/deploy-calls.log"
bash -c "$2"
"#;

fn write_fake(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

#[test]
fn a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    // A "remote" the fake ssh/rsync actually reach: a directory on this
    // machine, exactly as docs/DEPLOY.md's build order intends.
    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "command=true",
            "--arg",
            "exclude=node_modules",
            "--check",
            "cat flag.txt; grep -qx good flag.txt",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // Two commits: one whose check will pass, one whose check will fail.
    std::fs::write(e.repo.join("flag.txt"), "good\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "good"]);
    let good_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    std::fs::write(e.repo.join("flag.txt"), "bad\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "bad"]);
    let bad_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    // A path already on the host that matches the exclude arg (e.g.
    // node_modules, rebuilt once and kept between deploys) must survive
    // rsync's --delete; a stray path that is not excluded must still be
    // swept away.
    std::fs::create_dir_all(remote.join("node_modules")).unwrap();
    std::fs::write(remote.join("node_modules/keep.txt"), "keep\n").unwrap();
    std::fs::write(remote.join("stray.txt"), "stray\n").unwrap();

    let run_deploy = |sha: &str| -> std::process::Output {
        e.cmd("ok.sh")
            .env("PATH", &path)
            .env("HOME", &fakehome)
            .args(["deploy", "demo", "prod", "--sha", sha])
            .output()
            .unwrap()
    };

    // A deploy that passes its check records ok.
    let o = run_deploy(&good_sha);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["sha"], good_sha);
    assert_eq!(rows[0]["check_ok"], true);
    assert_eq!(rows[0]["rolled_back_to"], serde_json::Value::Null);

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(calls.contains("rsync"), "{calls}");
    assert!(calls.contains("ssh remotebox"), "{calls}");
    assert_eq!(
        std::fs::read_to_string(remote.join("flag.txt")).unwrap(),
        "good\n"
    );

    // The excluded directory survived rsync's --delete; the stray file
    // that was not excluded did not.
    assert_eq!(
        std::fs::read_to_string(remote.join("node_modules/keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(!remote.join("stray.txt").exists());

    // A deploy whose check fails rolls back to the last deploy that
    // passed, and blocks a question naming the check's output.
    let o = run_deploy(&bad_sha);
    assert!(!o.status.success());
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let failed = &rows[0]; // newest first
    assert_eq!(failed["sha"], bad_sha);
    assert_eq!(failed["check_ok"], false);
    assert_eq!(failed["rolled_back_to"], good_sha);
    let output = failed["check_output"].as_str().unwrap();
    assert!(output.contains("bad"), "{output}");

    // The rollback actually redeployed the last passing commit.
    assert_eq!(
        std::fs::read_to_string(remote.join("flag.txt")).unwrap(),
        "good\n"
    );

    // A blocked question was filed on a new task (this project's
    // repository never had one), naming the check's output.
    let (state, reason): (String, String) = e
        .db()
        .query_row(
            "SELECT state, reason FROM tasks WHERE project = 'demo' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "blocked");
    assert!(reason.contains("rolled back to"), "{reason}");
    assert!(reason.contains("bad"), "{reason}");
}

/// A minimal local HTTP server for the smoke e2e test below: one page with
/// a failing subresource (`/missing.png`, 404) and one deliberate console
/// error, exactly the shape a check's curl of two endpoints cannot see
/// (see docs/DEPLOY.md, "A deterministic smoke step").
fn serve_fake_page() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let (status, body) = if path.starts_with("/missing.png") {
                    ("404 Not Found", "nope".to_string())
                } else {
                    (
                        "200 OK",
                        "<!doctype html><html><head><title>Smoke Test Page</title></head>\
                         <body><img src=\"/missing.png\">\
                         <script>console.error(\"deliberate smoke test console error\")</script>\
                         </body></html>"
                            .to_string(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    addr
}

#[test]
fn a_deploy_smoke_check_records_a_console_error_and_a_failed_subresource_and_fails_the_deploy() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let addr = serve_fake_page();
    let url = format!("http://{addr}/index.html");
    let dest = e._dir.path().join("remote");

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={}", dest.to_str().unwrap()),
            "--arg",
            "command=true",
            "--check",
            "true",
            "--smoke",
            &url,
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // The target's own check passes (host=local, command and check are
    // both `true`), but the smoke step finds the console error and the
    // failed subresource, and fails the deploy.
    let o = e
        .cmd("ok.sh")
        .args(["deploy", "demo", "prod"])
        .output()
        .unwrap();
    assert!(
        !o.status.success(),
        "the smoke failure should fail the deploy: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["check_ok"], false, "{row:?}");
    assert_eq!(row["smoke_ok"], false, "{row:?}");

    let smoke: serde_json::Value = serde_json::from_str(
        row["smoke_json"]
            .as_str()
            .unwrap_or_else(|| panic!("no smoke_json recorded: {row:?}")),
    )
    .unwrap();
    assert_eq!(smoke["ok"], false, "{smoke:?}");
    assert_eq!(smoke["title"], "Smoke Test Page", "{smoke:?}");
    assert!(
        smoke["console_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["text"]
                .as_str()
                .unwrap_or_default()
                .contains("deliberate smoke test console error")),
        "{smoke:?}"
    );
    let failed = smoke["failed_requests"].as_array().unwrap();
    assert!(
        failed.iter().any(|f| f["url"]
            .as_str()
            .unwrap_or_default()
            .contains("/missing.png")
            && f["status"].as_u64() == Some(404)
            && f["origin"] == "own"),
        "{smoke:?}"
    );

    // A full-page screenshot lands beside the deploy's record.
    let id = row["id"].as_i64().unwrap();
    let screenshot = e
        .home
        .join("deploys")
        .join(id.to_string())
        .join("screenshot.png");
    assert!(screenshot.exists(), "{}", screenshot.display());
    assert!(std::fs::metadata(&screenshot).unwrap().len() > 0);
}

/// A page whose only failure is a subresource pointed at a `dead_port`
/// nothing is listening on, so the only failed request (and Chromium's
/// own "Failed to load resource" console message for it) is third-party.
fn serve_third_party_failure_page(dead_port: u16) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).unwrap_or(0);
                let body = format!(
                    "<!doctype html><html><head><title>3P Test</title></head>\
                     <body><img src=\"http://127.0.0.1:{dead_port}/nope.png\"></body></html>"
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    addr
}

#[test]
fn a_deploy_smoke_check_records_a_third_party_failure_without_failing_the_deploy() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "demo3p",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );

    // A port that was bound and then released, so nothing listens on it:
    // the subresource request to it fails, but it is a third party from
    // the smoke page's point of view.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let addr = serve_third_party_failure_page(dead_port);
    let url = format!("http://{addr}/index.html");
    let dest = e._dir.path().join("remote");

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo3p",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={}", dest.to_str().unwrap()),
            "--arg",
            "command=true",
            "--check",
            "true",
            "--smoke",
            &url,
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // The target's check passes and the smoke step's only failure is a
    // third-party subresource, so the deploy itself succeeds.
    let o = e
        .cmd("ok.sh")
        .args(["deploy", "demo3p", "prod"])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "a third-party-only failure should not fail the deploy: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo3p", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["smoke_ok"], true, "{row:?}");

    let smoke: serde_json::Value = serde_json::from_str(
        row["smoke_json"]
            .as_str()
            .unwrap_or_else(|| panic!("no smoke_json recorded: {row:?}")),
    )
    .unwrap();
    assert_eq!(smoke["ok"], true, "{smoke:?}");

    let dead_url = format!("http://127.0.0.1:{dead_port}/nope.png");
    let failed = smoke["failed_requests"].as_array().unwrap();
    assert!(
        failed
            .iter()
            .any(|f| f["url"].as_str().unwrap_or_default() == dead_url
                && f["origin"] == "third_party"),
        "{smoke:?}"
    );

    let console_errors = smoke["console_errors"].as_array().unwrap();
    assert!(
        console_errors.iter().any(|c| c["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Failed to load resource")
            && c["origin"] == "third_party"),
        "{smoke:?}"
    );
}

/// A fake `systemctl`: records every call, and simulates a unit that
/// takes a couple of polls after `restart` before `is-active` reports
/// `active`, so the wait loop in deploy-user-service.toml is exercised
/// for real rather than passing on its first check.
const FAKE_SYSTEMCTL: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$HOME/deploy-calls.log"
count_file="$HOME/systemctl-is-active-count"
if [ "$1" = "--user" ] && [ "$2" = "restart" ]; then
  echo 0 > "$count_file"
  exit 0
fi
if [ "$1" = "--user" ] && [ "$2" = "is-active" ]; then
  n=$(cat "$count_file" 2>/dev/null || echo 0)
  n=$((n + 1))
  echo "$n" > "$count_file"
  if [ "$n" -ge 3 ]; then
    echo "active"
    exit 0
  fi
  echo "activating"
  exit 3
fi
echo "unexpected systemctl invocation: $*" >&2
exit 1
"#;

#[test]
fn deploy_user_service_restarts_the_unit_and_waits_for_it_to_report_active() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "svc", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "svc",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-user-service",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "unit=demo.service",
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    write_fake(&fakebin.join("systemctl"), FAKE_SYSTEMCTL);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["deploy", "svc", "prod"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(
        calls.contains("systemctl --user restart demo.service"),
        "{calls}"
    );
    let is_active_calls = calls.matches("systemctl --user is-active").count();
    assert!(
        is_active_calls >= 3,
        "expected the wait loop to poll is-active more than once: {calls}"
    );
    assert_eq!(
        std::fs::read_to_string(remote.join("hello.sh")).unwrap(),
        "#!/bin/bash\necho hello\n"
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "svc", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap()[0]["check_ok"], true);
}

/// A fake `curl`: records every call and the URL it was given, and
/// always answers 200 with a canned body containing "MARKER123", so a
/// deploy-static target's default check can be driven without a network.
const FAKE_CURL: &str = r#"#!/bin/bash
echo "curl $*" >> "$HOME/deploy-calls.log"
out=""
url=""
args=("$@")
i=0
while [ $i -lt ${#args[@]} ]; do
  case "${args[$i]}" in
    -o)
      i=$((i + 1))
      out="${args[$i]}"
      ;;
    -w)
      i=$((i + 1))
      ;;
    -s) ;;
    *) url="${args[$i]}" ;;
  esac
  i=$((i + 1))
done
echo "$url" >> "$HOME/deploy-curl-urls.log"
body="hello world MARKER123 goodbye"
if [ -n "$out" ]; then
  printf '%s' "$body" > "$out"
fi
printf '200'
"#;

#[test]
fn deploy_static_rsyncs_and_defaults_the_check_to_a_url_fetch_with_a_marker() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "site", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    // No --check: deploy-static defaults to fetching `url` and requiring
    // the `marker` string in the body.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "site",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-static",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "url=http://static.example.invalid/",
            "--arg",
            "marker=MARKER123",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("curl"), FAKE_CURL);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["deploy", "site", "prod"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let urls = std::fs::read_to_string(fakehome.join("deploy-curl-urls.log")).unwrap();
    assert!(urls.contains("http://static.example.invalid/"), "{urls}");
    assert!(remote.join("hello.sh").exists());

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "site", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap()[0]["check_ok"], true);

    // A target whose marker never shows up in the body fails the check.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "site",
            "prod2",
            "--repo",
            repo_s,
            "--method",
            "deploy-static",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "url=http://static.example.invalid/",
            "--arg",
            "marker=NOPE",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["deploy", "site", "prod2"])
        .output()
        .unwrap();
    assert!(!o.status.success());

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "site", "prod2", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap()[0]["check_ok"], false);
}

#[test]
fn a_task_landing_on_a_repository_deploys_its_on_landing_targets_tied_to_the_task() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    // Exactly the arrangement `a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question`
    // uses: a fake ssh/rsync reaching a directory on this machine, and the
    // check reading what got deployed there.
    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "command=true",
            "--check",
            "grep -qx 42 answer.txt",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    // The task lands on `main`, which runs the on-landing target through
    // exactly the path `forge deploy` uses.
    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["run", repo_s, "write 42", "--retries", "0"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(calls.contains("rsync"), "{calls}");
    assert!(calls.contains("ssh remotebox"), "{calls}");

    // The deploy row is tied to the task and its check passed.
    let (task_id, check_ok): (Option<i64>, Option<i64>) = e
        .db()
        .query_row(
            "SELECT task_id, check_ok FROM deploys WHERE project='demo' AND target='prod'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(task_id, Some(1));
    assert_eq!(check_ok, Some(1));

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["check_ok"], true);

    // DeployStarted and DeployFinished carry the task id.
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    let parsed: Vec<serde_json::Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let started = parsed
        .iter()
        .find(|v| v["type"] == "deploy_started")
        .unwrap_or_else(|| panic!("no deploy_started event in:\n{events}"));
    assert_eq!(started["task"], 1, "{started}");
    let finished = parsed
        .iter()
        .find(|v| v["type"] == "deploy_finished")
        .unwrap_or_else(|| panic!("no deploy_finished event in:\n{events}"));
    assert_eq!(finished["task"], 1, "{finished}");
    assert_eq!(finished["ok"], true, "{finished}");
}

/// An on-landing deploy's row shows up where people look at the task: a
/// `forge show` line starting with "deploy" (target, sha, ok or rolled
/// back, when), and `forge trace --json`'s `deploys` array.
#[test]
fn an_on_landing_deploy_shows_up_on_forge_show_and_trace_json() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "command=true",
            "--check",
            "grep -qx 42 answer.txt",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["run", repo_s, "write 42", "--retries", "0"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let sha: String = e
        .db()
        .query_row(
            "SELECT sha FROM deploys WHERE project='demo' AND target='prod'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    let deploy_line = show
        .lines()
        .find(|l| l.trim_start().starts_with("deploy"))
        .unwrap_or_else(|| panic!("no deploy line in forge show:\n{show}"));
    assert!(deploy_line.contains("prod"), "{deploy_line}");
    assert!(deploy_line.contains(&sha[..8]), "{deploy_line}");
    assert!(deploy_line.contains("ok"), "{deploy_line}");

    let doc = e.trace_json(1);
    let deploys = doc["deploys"].as_array().unwrap();
    assert_eq!(deploys.len(), 1, "{deploys:?}");
    assert_eq!(deploys[0]["target"], "prod");
    assert_eq!(deploys[0]["sha"], sha);
    assert_eq!(deploys[0]["check_ok"], true);
}
