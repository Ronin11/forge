//! "Request something" and "Your requests" (see `portal/src/main.rs`'s
//! module doc): the form files through `forge ask` and answers in plain
//! words what happens next; each request shows its state as waiting,
//! being built, needs you, or done; and a question addressed to the
//! customer is answered from the same page. A fixture with one request in
//! each state, against a real `forge-portal` and a fake `forge`, pins the
//! rendered sections to a checked-in snapshot.
//!
//! A snapshot is never written automatically. To update it deliberately
//! after a change to the rendering, re-run with `UPDATE_SNAPSHOTS=1`,
//! then read the diff in `git diff portal/tests/snapshots/` before
//! committing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

/// One request in each state; the one that needs the customer carries its
/// question under Needs you too.
const DOC: &str = r#"{
    "project": "acme",
    "purpose": "internal only, never rendered",
    "deploy_targets": [],
    "run_workflows": [],
    "initiatives": [],
    "initiatives_more": 0,
    "questions": [
        {"task_id": 42, "text": "Which colour should the button be?", "asked_at": 1700000250}
    ],
    "landed": [],
    "landed_more": 0,
    "requests": [
        {"text": "Add a contact page", "state": "waiting", "created_at": 1700000400},
        {"text": "Make the button bigger", "state": "being built", "created_at": 1700000300},
        {"text": "Change the button colour", "state": "needs you", "created_at": 1700000200},
        {"text": "Fixed the footer link.", "state": "done", "created_at": 1700000100}
    ],
    "brief": null,
    "backlog": []
}"#;

fn fake_script(state_dir: &Path) -> String {
    let doc = state_dir.join("doc.json");
    std::fs::write(&doc, DOC).unwrap();
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
      list) echo '[]' ;;
      *) echo "unexpected message: $*" >&2; exit 2 ;;
    esac
    ;;
  decisions)
    echo '[]'
    ;;
  ask)
    echo "$*" >> "{calls}"
    echo "concierge: a request; filed task 9 (1 queued)"
    ;;
  answer)
    echo "$*" >> "{calls}"
    echo "answered; re-queued as task 43"
    ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        doc = doc.display(),
        calls = state_dir.join("calls").display(),
    )
}

struct Portal {
    child: std::process::Child,
    addr: String,
    home: tempfile::TempDir,
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
    Portal { child, addr, home }
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

fn section<'a>(body: &'a str, title: &str) -> &'a str {
    let start = body
        .find(&format!("<section><h2>{title}</h2>"))
        .expect("the section is there");
    let rest = &body[start..];
    let end = rest.find("</section>").expect("the section is closed") + "</section>".len();
    &rest[..end]
}

fn post_form(addr: &str, path: &str, form: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(
        s,
        "POST {path} HTTP/1.0\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    )
    .unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

#[test]
fn requests_show_their_state_in_plain_words_and_the_form_files_through_ask() {
    let p = start();
    let (status, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");
    let form = section(&body, "Request something");
    let requests = section(&body, "Your requests");
    for state in ["waiting", "being built", "needs you", "done"] {
        assert!(requests.contains(&format!("({state})")), "{requests}");
    }
    assert!(form.contains("/p/good-token/request"), "{form}");

    let rendered = format!("{form}\n{requests}\n");
    let snap = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/requests.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snap, &rendered).unwrap();
    }
    assert_eq!(
        rendered,
        std::fs::read_to_string(&snap).unwrap_or_default(),
        "the requests snapshot changed; re-run with UPDATE_SNAPSHOTS=1 if that is intended"
    );

    // The question addressed to the customer is answerable on this page.
    assert!(body.contains(r#"action="/p/good-token/answer""#), "{body}");
    let (status, _) = post_form(&p.addr, "/p/good-token/answer", "id=42&text=Blue");
    assert_eq!(status, 200);

    let (status, body) = post_form(&p.addr, "/p/good-token/request", "message=Add+a+blog");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("We&#39;ve filed that as a task") || body.contains("filed that as a task"),
        "{body}"
    );
    let calls = std::fs::read_to_string(p.home.path().join("calls")).unwrap();
    assert!(calls.contains("ask acme Add a blog --from"), "{calls}");
    assert!(calls.contains("answer 42 Blue --by"), "{calls}");
}
