//! forge-web against a fake forge: the token gate, the JSON routes passed
//! through untouched, and the event stream as server-sent events.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

const FAKE: &str = r#"#!/bin/bash
case "$1" in
  snapshot) echo '{"events_offset":7,"tasks":[{"id":1,"state":"queued"}],"requests":[],"worker":{"running":false}}' ;;
  trace) echo "{\"task\":{\"id\":$2},\"attempts\":[]}" ;;
  journal) echo "[{\"task\":$2,\"note\":\"journal\"}]" ;;
  requests) echo '[{"id":1,"status":"pending"}]' ;;
  log) shift; printf '[{"id":9,"args":"%s"}]\n' "$*" ;;
  retry) echo "retried task $2 as 99" ;;
  events) echo '{"type":"note","task":1,"text":"first","ts":1}'; echo '{"type":"note","task":1,"text":"second","ts":2}'; sleep 5 ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

struct Web {
    child: std::process::Child,
    addr: String,
    token: String,
    _home: tempfile::TempDir,
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Web {
    let home = tempfile::tempdir().unwrap();
    let fake = home.path().join("forge");
    std::fs::write(&fake, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-web"))
        .args(["--bind", "127.0.0.1:0"])
        .env("FORGE_BIN", &fake)
        .env("FORGE2_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    // http://127.0.0.1:PORT/?token=...
    let rest = line.trim().strip_prefix("http://").unwrap();
    let (addr, q) = rest.split_once("/?token=").unwrap();
    let token = std::fs::read_to_string(home.path().join("web.token")).unwrap();
    assert_eq!(q, token, "the printed link carries the stored token");
    Web {
        child,
        addr: addr.to_string(),
        token,
        _home: home,
    }
}

/// One raw HTTP/1.0 request; returns (status, headers, body).
fn get(addr: &str, path: &str, extra: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: x\r\n{extra}\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_string(), body.to_string())
}

/// One raw HTTP/1.0 POST; returns (status, headers, body).
fn post(addr: &str, path: &str, extra: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(s, "POST {path} HTTP/1.0\r\nHost: x\r\n{extra}\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_string(), body.to_string())
}

#[test]
fn without_the_token_nothing_is_served() {
    let w = start();
    for path in [
        "/",
        "/tasks",
        "/tasks/1/run",
        "/api/snapshot",
        "/api/tasks",
        "/api/events",
        "/api/task/1",
        "/api/journal/1",
        "/api/requests",
        "/app.js",
    ] {
        let (status, _, _) = get(&w.addr, path, "");
        assert_eq!(status, 401, "{path}");
    }
    let (status, _, _) = get(&w.addr, "/api/snapshot", "Cookie: forge_token=wrong\r\n");
    assert_eq!(status, 401);
}

#[test]
fn the_first_visit_sets_the_cookie_and_the_routes_pass_forge_json_through() {
    let w = start();
    let (status, head, _) = get(&w.addr, &format!("/?token={}", w.token), "");
    assert_eq!(status, 303);
    assert!(head.contains(&format!("forge_token={}", w.token)), "{head}");
    assert!(head.contains("Location: /tasks"), "{head}");
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let (status, head, _) = get(&w.addr, "/", &cookie);
    assert_eq!(status, 303, "{head}");
    for view in ["/tasks", "/tasks/12", "/tasks/12/run"] {
        let (status, _, body) = get(&w.addr, view, &cookie);
        assert_eq!(status, 200, "{view}");
        assert!(body.contains("<title>Forge</title>"), "{view}");
        assert!(body.contains(r#"<script src="/app.js">"#), "{view}");
    }
    let (status, head, body) = get(&w.addr, "/app.js", &cookie);
    assert_eq!(status, 200);
    assert!(
        head.contains("Content-Type: application/javascript"),
        "{head}"
    );
    assert!(body.contains("INVALIDATES"), "{body}");
    // The task listing is forge log --json with the page's filters as argv.
    let (status, _, body) = get(
        &w.addr,
        "/api/tasks?limit=50&before=120&q=doctor+json&state=failed&workflow=direct",
        &cookie,
    );
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v[0]["args"],
        "--json --limit 50 --before 120 --grep doctor json --state failed --workflow direct"
    );
    let (_, _, body) = get(&w.addr, "/api/tasks", &cookie);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["args"], "--json --limit 100");
    let (status, _, body) = get(&w.addr, "/api/snapshot", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["events_offset"], 7);
    let bearer = format!("Authorization: Bearer {}\r\n", w.token);
    let (status, _, body) = get(&w.addr, "/api/task/5", &bearer);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["task"]["id"], 5);
    let (status, _, _) = get(&w.addr, "/api/task/x", &cookie);
    assert_eq!(status, 404);
    let (status, _, body) = get(&w.addr, "/api/journal/5", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["task"], 5);
    let (status, _, body) = get(&w.addr, "/api/requests", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["id"], 1);
}

#[test]
fn retrying_a_task_posts_through_to_forge_retry_and_a_get_is_refused() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let (status, _, body) = post(&w.addr, "/api/retry/1", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["output"].as_str().unwrap().contains("1"), "{body}");
    let (status, _, _) = get(&w.addr, "/api/retry/1", &cookie);
    assert_eq!(status, 405);
}

#[test]
fn events_stream_as_server_sent_events_from_the_offset() {
    let w = start();
    let mut s = TcpStream::connect(&w.addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(
        s,
        "GET /api/events?since=7 HTTP/1.1\r\nHost: x\r\nCookie: forge_token={}\r\n\r\n",
        w.token
    )
    .unwrap();
    let mut r = BufReader::new(s);
    let mut head = String::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        if l == "\r\n" || l.is_empty() {
            break;
        }
        head.push_str(&l);
    }
    assert!(head.contains("text/event-stream"), "{head}");
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap() == 0 {
            break;
        }
        if let Some(d) = l.trim().strip_prefix("data: ") {
            seen.push(d.to_string());
        }
    }
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].contains("first") && seen[1].contains("second"));
}
