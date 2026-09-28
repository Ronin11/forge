//! The Chat page's routes against a fake forge (docs/CLIENT.md, "Ask
//! Forge"): a message is `forge chat --stream` relayed as server-sent
//! events, the sessions and a session's turns are `chat sessions` and
//! `chat show`, and confirming or rejecting a proposed action is `chat
//! confirm` / `chat reject`. What the fake prints is canned; what it was
//! given is logged, so the test sees the web layer pass the operator's
//! words and the action ids through as arguments, never as options.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

const FAKE: &str = r#"#!/bin/bash
[ "$1" = "chat" ] || { echo "unexpected: $*" >&2; exit 2; }
shift
case "$1" in
  sessions) echo '{"sessions":[{"id":3,"title":"why did 1 fail","provider":"p","created_at":1,"updated_at":2,"turns":2,"cost_usd":0.01}]}' ;;
  show) echo '{"session":{"id":3,"title":"t"},"turns":[{"id":10,"role":"user","text":"hi","tool_calls":[]}]}' ;;
  confirm|reject)
    echo "$*" >"$FORGE_HOME/chat-decision"
    echo '{"action":"'"$2"'","status":"confirmed","outcome":"filed task 2","turn":9}' ;;
  --stream)
    echo "$*" >"$FORGE_HOME/chat-args"
    echo '{"type":"session","session":3}'
    sleep 0.2
    echo '{"type":"tool","tool":"task","arguments":{"id":1},"result":{"task":{"state":"failed"}}}'
    echo '{"type":"reply","session":3,"turn":11,"text":"It failed.","cost_usd":0.01,"proposals":[]}' ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

struct Web {
    child: std::process::Child,
    addr: String,
    token: String,
    home: tempfile::TempDir,
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
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-web"))
        .args(["--bind", "127.0.0.1:0"])
        .env("FORGE_BIN", &fake)
        .env("FORGE_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let rest = line.trim().strip_prefix("http://").unwrap();
    let (addr, _) = rest.split_once("/?token=").unwrap();
    let token = std::fs::read_to_string(home.path().join("web.token")).unwrap();
    Web {
        child,
        addr: addr.to_string(),
        token,
        home,
    }
}

/// One raw HTTP/1.0 request; returns (status, body).
fn request(addr: &str, method: &str, path: &str, cookie: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let len = if method == "POST" {
        format!("Content-Length: {}\r\n", body.len())
    } else {
        String::new()
    };
    write!(
        s,
        "{method} {path} HTTP/1.0\r\nHost: x\r\n{cookie}{len}\r\n{body}"
    )
    .unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

#[test]
fn a_message_streams_its_events_and_the_operators_words_reach_forge_as_data() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let get = |p: &str| request(&w.addr, "GET", p, &cookie, "");
    let post = |p: &str, b: &str| request(&w.addr, "POST", p, &cookie, b);

    // The Chat page and its script are served; the nav names it.
    let (status, page) = get("/chat/3");
    assert_eq!(status, 200);
    assert!(page.contains(r#"<script src="/chat.js">"#), "{page}");
    let (_, js) = get("/chat.js");
    assert!(js.contains("parseFrames") && js.contains("renderProposal"));

    let (status, body) = get("/api/chat");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["sessions"][0]["id"], 3);
    let (status, body) = get("/api/chat/3");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["turns"][0]["text"], "hi");

    // A message: every event arrives as an SSE frame, in order. A message
    // that looks like an option is still only the message.
    let (status, body) = post(
        "/api/chat/message",
        r#"{"message":"--why did 1 fail","session":3}"#,
    );
    assert_eq!(status, 200, "{body}");
    let frames: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(json)
        .collect();
    let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["session", "tool", "reply"], "{body}");
    let args = std::fs::read_to_string(w.home.path().join("chat-args")).unwrap();
    assert_eq!(args.trim(), "--stream --session 3 -- --why did 1 fail");

    // A new conversation names no session.
    post("/api/chat/message", r#"{"message":"hello"}"#);
    let args = std::fs::read_to_string(w.home.path().join("chat-args")).unwrap();
    assert_eq!(args.trim(), "--stream -- hello");

    // Deciding a proposal is one verb, on an action id and nothing else.
    let (status, body) = post("/api/chat/confirm/11.2", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["outcome"], "filed task 2");
    let seen = std::fs::read_to_string(w.home.path().join("chat-decision")).unwrap();
    assert_eq!(seen.trim(), "confirm 11.2 --json");
    post("/api/chat/reject/11.3", "");
    let seen = std::fs::read_to_string(w.home.path().join("chat-decision")).unwrap();
    assert_eq!(seen.trim(), "reject 11.3 --json");
    std::fs::remove_file(w.home.path().join("chat-decision")).unwrap();
    for bad in [
        "/api/chat/confirm/--json",
        "/api/chat/confirm/1.0%20--x",
        "/api/chat/confirm/abc",
        "/api/chat/approve/1.0",
    ] {
        assert_eq!(post(bad, "").0, 404, "{bad}");
    }
    assert!(
        !w.home.path().join("chat-decision").exists(),
        "nothing reached forge"
    );

    // Empty and oversized messages are refused before forge is asked.
    assert_eq!(post("/api/chat/message", r#"{"message":"  "}"#).0, 422);
    let long = "x".repeat(8001);
    assert_eq!(
        post("/api/chat/message", &format!(r#"{{"message":"{long}"}}"#)).0,
        422
    );
    assert_eq!(post("/api/chat/message", "not json").0, 400);

    // Writes need the token, and only POST writes.
    assert_eq!(
        request(
            &w.addr,
            "POST",
            "/api/chat/message",
            "",
            r#"{"message":"x"}"#
        )
        .0,
        401
    );
    assert_eq!(get("/api/chat/message").0, 405);
}
