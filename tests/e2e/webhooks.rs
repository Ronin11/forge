//! The webhook trigger (docs/JOBS.md, "Triggers"): `forge job fire` behind a
//! per-hook token, and the web client's `POST /hooks/<project>/<name>` on
//! top of it.

use crate::support::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

/// Register the fixture repository as project `shop` and commit a run
/// workflow `ship` triggered by the webhook `orders`, whose one operation
/// writes the delivery's `order` field to `got.txt` in the job's scratch tree.
fn setup_webhook_workflow(e: &Env) {
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "shop", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );
    std::fs::create_dir_all(e.repo.join(".forge/workflows/actions")).unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/actions/keep-order.toml"),
        "name = \"keep-order\"\nkind = \"operation\"\ndescription = \"writes the order it was fired with to got.txt\"\nrun = [\"bash\", \"-c\", \"printf '%s' \\\"$FORGE_INPUT_ORDER\\\" > got.txt\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/ship.toml"),
        r#"name = "ship"
kind = "run"
description = "starts on a webhook, for e2e coverage of the webhook trigger"

steps = [
  { action = "keep-order" },
]

[trigger]
on = "webhook"
name = "orders"

[assert]
ok = ["true"]
"#,
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "add the ship automation"]);
}

fn mint(e: &Env, name: &str) -> String {
    let o = e.forge("ok.sh", &["project", "webhook", "token", "shop", name]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn job_rows(e: &Env) -> Vec<serde_json::Value> {
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    rows.as_array().unwrap().clone()
}

fn fire(
    e: &Env,
    token: Option<&str>,
    input: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    let mut args = vec![
        "job",
        "fire",
        "shop",
        "--webhook",
        "orders",
        "--input",
        input.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    if let Some(t) = token {
        args.extend(["--token", t]);
    }
    e.forge("ok.sh", &args)
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `forge job fire` starts nothing without an active token minted for that
/// very hook; with one it queues a job keyed on the caller's `--ref` (else
/// the input's SHA-256), so firing the same delivery again starts no second
/// job, and `forge work --once` runs it with the body as its input.
#[test]
fn forge_job_fire_needs_a_valid_hook_token_and_one_delivery_starts_one_job() {
    let e = Env::new();
    setup_webhook_workflow(&e);
    let body = e.home.join("body.json");
    std::fs::write(&body, r#"{"order":"17"}"#).unwrap();

    // Nothing to present, a stranger's token, and another hook's token.
    let other = mint(&e, "refunds");
    for token in [None, Some("not-a-token"), Some(other.as_str())] {
        let o = fire(&e, token, &body, &[]);
        assert!(!o.status.success(), "{token:?}");
        assert!(
            stderr(&o).contains("invalid webhook token"),
            "{}",
            stderr(&o)
        );
    }
    assert!(job_rows(&e).is_empty());

    let token = mint(&e, "orders");
    let o = fire(&e, Some(&token), &body, &["--ref", "delivery-1"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let job_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    let rows = job_rows(&e);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["id"], job_id);
    assert_eq!(rows[0]["workflow"], "ship");
    assert_eq!(rows[0]["trigger_kind"], "webhook");
    assert_eq!(rows[0]["trigger_ref"], "delivery-1");
    assert_eq!(rows[0]["state"], "queued", "queued, never run inline");

    // The sender retries: the same ref names the same job, and the body
    // does not matter once the ref is given.
    let retry = e.home.join("retry.json");
    std::fs::write(&retry, r#"{"order":"18"}"#).unwrap();
    let o = fire(&e, Some(&token), &retry, &["--ref", "delivery-1"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        job_id.to_string()
    );
    assert_eq!(job_rows(&e).len(), 1);

    // No ref: the body is the key.
    let o = fire(&e, Some(&token), &body, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let hashed: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    assert_ne!(hashed, job_id);
    let o = fire(&e, Some(&token), &body, &[]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        hashed.to_string()
    );
    assert_eq!(job_rows(&e).len(), 2);

    // A revoked token is refused, and a fresh one works again.
    let o = e.forge("ok.sh", &["project", "webhook", "revoke", "shop", "orders"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = fire(&e, Some(&token), &retry, &["--ref", "delivery-2"]);
    assert!(
        stderr(&o).contains("invalid webhook token"),
        "{}",
        stderr(&o)
    );
    assert_eq!(job_rows(&e).len(), 2);
    let fresh = mint(&e, "orders");
    assert!(
        fire(&e, Some(&fresh), &retry, &["--ref", "delivery-2"])
            .status
            .success()
    );
    assert_eq!(job_rows(&e).len(), 3);

    // The worker runs the first with the body as its input.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &job_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let scratch = e.home.join("worktrees").join(format!("job-{job_id}"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("got.txt")).unwrap(),
        "17"
    );

    // Tokens are listed by hook, never as the secret.
    let o = e.forge("ok.sh", &["project", "webhook", "list", "shop", "--json"]);
    let listed = String::from_utf8_lossy(&o.stdout).into_owned();
    let rows: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 3, "{listed}");
    assert!(
        !listed.contains(&token) && !listed.contains(&fresh),
        "{listed}"
    );
    assert!(rows[0]["revoked_at"].is_null(), "refunds: {listed}");
    assert!(
        rows[1]["revoked_at"].is_i64(),
        "the first orders token: {listed}"
    );
    assert!(rows[2]["revoked_at"].is_null(), "the fresh one: {listed}");
}

#[test]
fn forge_job_fire_refuses_a_hook_no_workflow_claims_and_a_body_that_is_not_an_object() {
    let e = Env::new();
    setup_webhook_workflow(&e);
    let body = e.home.join("body.json");
    std::fs::write(&body, "[1, 2]").unwrap();
    let token = mint(&e, "orders");
    let o = fire(&e, Some(&token), &body, &[]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("JSON object"), "{}", stderr(&o));

    let ghost = mint(&e, "ghost");
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "fire",
            "shop",
            "--webhook",
            "ghost",
            "--token",
            &ghost,
        ],
    );
    assert!(!o.status.success());
    assert!(stderr(&o).contains("no run workflow"), "{}", stderr(&o));

    // A name is a URL path segment.
    let o = e.forge("ok.sh", &["project", "webhook", "token", "shop", "a/b"]);
    assert!(!o.status.success());
    assert!(job_rows(&e).is_empty());
}

/// `forge-web` running against this environment's forge.
struct Web {
    child: Child,
    addr: String,
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_web(e: &Env) -> Web {
    let forge = std::path::Path::new(env!("CARGO_BIN_EXE_forge"));
    let web = forge.with_file_name("forge-web");
    assert!(
        web.exists(),
        "{} is not built; run `cargo test --workspace` (or `cargo build -p forge-web`) first",
        web.display()
    );
    let mut c = Command::new(&web);
    c.args(["--bind", "127.0.0.1:0"]).env("FORGE_BIN", forge);
    for (k, v) in e.cmd("ok.sh").get_envs() {
        if let Some(v) = v {
            c.env(k, v);
        }
    }
    let mut child = c
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("forge-web");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("http://")
        .and_then(|r| r.split_once("/?token="))
        .expect("forge-web's link")
        .0
        .to_string();
    Web { child, addr }
}

/// One raw POST: (status, body).
fn post(w: &Web, path: &str, headers: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(&w.addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    write!(
        s,
        "POST {path} HTTP/1.0\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

/// A POST with the right token starts one job; the same body with the same
/// ref does not start a second; a wrong token is a 401 that starts nothing.
#[test]
fn a_webhook_post_with_the_right_token_starts_one_job_and_a_retry_starts_no_second() {
    let e = Env::new();
    setup_webhook_workflow(&e);
    let token = mint(&e, "orders");
    let w = start_web(&e);
    let auth = format!("Authorization: Bearer {token}\r\n");
    let body = r#"{"order":"17"}"#;

    let (status, resp) = post(&w, "/hooks/shop/orders?ref=delivery-1", &auth, body);
    assert_eq!(status, 200, "{resp}");
    let rows = job_rows(&e);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["trigger_kind"], "webhook");
    assert_eq!(rows[0]["trigger_ref"], "delivery-1");
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["job"], rows[0]["id"], "{resp}");

    // The sender's retry, same body and same ref.
    let (status, _) = post(&w, "/hooks/shop/orders?ref=delivery-1", &auth, body);
    assert_eq!(status, 200);
    assert_eq!(job_rows(&e).len(), 1);
    // And with no ref at all, the body is the key.
    let (status, _) = post(&w, "/hooks/shop/orders", &auth, body);
    assert_eq!(status, 200);
    let (status, _) = post(&w, "/hooks/shop/orders", &auth, body);
    assert_eq!(status, 200);
    assert_eq!(job_rows(&e).len(), 2);

    // A wrong token, another hook's token, and no token: 401, nothing started.
    let other = mint(&e, "refunds");
    for headers in [
        "Authorization: Bearer nope\r\n".to_string(),
        format!("Authorization: Bearer {other}\r\n"),
        String::new(),
    ] {
        let (status, resp) = post(&w, "/hooks/shop/orders?ref=delivery-9", &headers, body);
        assert_eq!(status, 401, "{headers:?}: {resp}");
    }
    assert_eq!(job_rows(&e).len(), 2);

    // A revoked token stops working at once.
    assert!(
        e.forge("ok.sh", &["project", "webhook", "revoke", "shop", "orders"])
            .status
            .success()
    );
    let (status, _) = post(&w, "/hooks/shop/orders?ref=delivery-9", &auth, body);
    assert_eq!(status, 401);
    assert_eq!(job_rows(&e).len(), 2);

    // A hook that exists for no workflow is a 404, a body that is no JSON object a 422.
    let ghost = mint(&e, "ghost");
    let (status, _) = post(
        &w,
        "/hooks/shop/ghost",
        &format!("Authorization: Bearer {ghost}\r\n"),
        "{}",
    );
    assert_eq!(status, 404);
    let fresh = mint(&e, "orders");
    let (status, _) = post(
        &w,
        "/hooks/shop/orders",
        &format!("Authorization: Bearer {fresh}\r\n"),
        "[1]",
    );
    assert_eq!(status, 422);
    assert_eq!(job_rows(&e).len(), 2);
}
