//! The conversation thread (see `portal/src/main.rs`'s module doc): every
//! message on the project's record, both directions, merged with any
//! question still open at the point it was asked, oldest first. A fixture
//! of three messages and one question exercises the ordering end to end,
//! against a real `forge-portal` and a fake `forge`, and pins the
//! rendered thread to a checked-in snapshot.
//!
//! A snapshot is never written automatically. To update it deliberately
//! after a change to the rendering, re-run with `UPDATE_SNAPSHOTS=1`,
//! then read the diff in `git diff portal/tests/snapshots/` before
//! committing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

/// The fixture: three messages (in, out, in) and one still-open question,
/// timed so the question falls between the second and third message —
/// proof the thread merges by when things happened, not by which kind of
/// row they came from.
const DOC: &str = r#"{
    "project": "acme",
    "purpose": "internal only, never rendered",
    "deploy_targets": [],
    "run_workflows": [],
    "initiatives": [],
    "initiatives_more": 0,
    "questions": [
        {"task_id": 7, "text": "Do you want the yearly plan instead?", "asked_at": 1700000300}
    ],
    "landed": [],
    "landed_more": 0,
    "brief": null,
    "backlog": []
}"#;

const MESSAGES: &str = r#"[
    {"id": 1, "project": "acme", "channel": "signal", "contact": "customer", "direction": "in", "text": "Can you add gift wrap at checkout?", "at": 1700000000, "task_id": null},
    {"id": 2, "project": "acme", "channel": "signal", "contact": "customer", "direction": "out", "text": "On it — filing that now.", "at": 1700000100, "task_id": 50},
    {"id": 3, "project": "acme", "channel": "signal", "contact": "customer", "direction": "in", "text": "Also, do you support international shipping?", "at": 1700000500, "task_id": null}
]"#;

fn fake_script(state_dir: &Path) -> String {
    let doc = state_dir.join("doc.json");
    let messages = state_dir.join("messages.json");
    std::fs::write(&doc, DOC).unwrap();
    std::fs::write(&messages, MESSAGES).unwrap();
    format!(
        r#"#!/bin/bash
case "$1" in
  project)
    case "$2" in
      resolve-token)
        if [ "$3" = "good-token" ]; then
          echo '{{"project":"acme"}}'
        else
          echo "unknown or revoked token" >&2
          exit 1
        fi
        ;;
      view) cat "{doc}" ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac
    ;;
  message)
    case "$2" in
      list) cat "{messages}" ;;
      *) echo "unexpected message: $*" >&2; exit 2 ;;
    esac
    ;;
  decisions)
    echo '[]'
    ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        doc = doc.display(),
        messages = messages.display(),
    )
}

struct Portal {
    child: std::process::Child,
    addr: String,
    _home: tempfile::TempDir,
}

impl Drop for Portal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Portal {
    let home = tempfile::tempdir().unwrap();
    let fake = home.path().join("forge");
    std::fs::write(&fake, fake_script(home.path())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-portal"))
        .args(["--bind", "127.0.0.1:0"])
        .env("FORGE_BIN", &fake)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("http://")
        .expect("forge-portal prints http://<addr>")
        .to_string();
    Portal {
        child,
        addr,
        _home: home,
    }
}

fn get(addr: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: x\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

/// The "Ask" section's own markup: the thread plus the box at its foot,
/// with none of the page's other sections to keep the snapshot narrow.
fn ask_section(body: &str) -> &str {
    let start = body.find("<section><h2>Ask</h2>").expect("an Ask section");
    let rest = &body[start..];
    let end = rest.find("</section>").expect("the Ask section is closed") + "</section>".len();
    &rest[..end]
}

#[test]
fn three_messages_and_one_question_render_as_one_thread_in_order() {
    let p = start();
    let (status, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");

    let section = ask_section(&body);

    // Ordering: the two early messages, then the question (asked between
    // them and the third message), then the third message — the thread
    // merges by when things happened, not by source.
    let gift_wrap = section.find("Can you add gift wrap").unwrap();
    let on_it = section.find("On it").unwrap();
    let question = section.find("Do you want the yearly plan").unwrap();
    let shipping = section.find("Also, do you support international").unwrap();
    assert!(gift_wrap < on_it, "{section}");
    assert!(on_it < question, "{section}");
    assert!(question < shipping, "{section}");

    // The open question renders as an answerable form, posting back
    // through this same token, the same way "Needs you" does.
    assert!(
        section.contains(
            r#"<form class="ask msg question" method="post" action="/p/good-token/answer">"#
        ),
        "{section}"
    );
    assert!(section.contains(r#"name="id" value="7""#), "{section}");

    // The Ask box itself is still there, posting into this same section.
    assert!(
        section.contains(r#"<form class="ask" method="post" action="/p/good-token/ask">"#),
        "{section}"
    );

    let snap = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/conversation.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snap, section).unwrap();
    }
    let want = std::fs::read_to_string(&snap).unwrap_or_default();
    assert_eq!(
        section, want,
        "the conversation snapshot changed; re-run with UPDATE_SNAPSHOTS=1 if that is intended"
    );
}
