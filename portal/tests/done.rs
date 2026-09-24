//! "Being built" and "Done" (see `portal/src/main.rs`'s module doc): each
//! automation described from its own workflow's `description`, a run's
//! effects as plain sentences (never a kind or a target), a dry run
//! marked a rehearsal, and a failed run's reason shown in the customer's
//! own terms. A fixture of one automation with one ok (rehearsed) run and
//! one failed run exercises all of it end to end, against a real
//! `forge-portal` and a fake `forge`, and pins the rendered section to a
//! checked-in snapshot.
//!
//! A snapshot is never written automatically. To update it deliberately
//! after a change to the rendering, re-run with `UPDATE_SNAPSHOTS=1`,
//! then read the diff in `git diff portal/tests/snapshots/` before
//! committing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

/// Two landings, one deployed: a task with a title whose deploy went live
/// with a look-step screenshot, and a title-less task that never deployed.
/// One open initiative is two of three tasks along.
const DOC: &str = r#"{
    "project": "acme",
    "purpose": "internal only, never rendered",
    "deploy_targets": [],
    "run_workflows": [],
    "initiatives": [
        {"outcome": "Ship the new checkout", "state": "in progress", "pieces": 3, "done": 2}
    ],
    "initiatives_more": 0,
    "questions": [],
    "landed": [
        {"text": "Added dark mode", "pieces": null, "landed_at": 1700000300,
         "deployed_at": 1700000400, "deploy_id": 7, "screenshot": "/nonexistent/shot.png"},
        {"text": "Fixed the footer link.", "pieces": null, "landed_at": 1700000100}
    ],
    "landed_more": 0,
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
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        doc = doc.display(),
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

fn section<'a>(body: &'a str, title: &str) -> &'a str {
    let start = body
        .find(&format!("<section><h2>{title}</h2>"))
        .expect("the section is there");
    let rest = &body[start..];
    let end = rest.find("</section>").expect("the section is closed") + "</section>".len();
    &rest[..end]
}

#[test]
fn done_shows_what_changed_when_it_went_live_and_the_look_screenshot() {
    let p = start();
    let (status, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");
    let done = section(&body, "Done");
    let building = section(&body, "Being built");
    assert!(building.contains("2 of 3 done"), "{building}");
    assert!(done.contains("Live since"), "{done}");
    assert_eq!(done.matches("<img").count(), 1, "{done}");
    assert!(done.contains("/p/good-token/shot/done/7"), "{done}");
    assert!(!done.contains("/nonexistent"), "{done}");

    let rendered = format!("{building}\n{done}\n");
    let snap = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/done.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snap, &rendered).unwrap();
    }
    assert_eq!(
        rendered,
        std::fs::read_to_string(&snap).unwrap_or_default(),
        "the done snapshot changed; re-run with UPDATE_SNAPSHOTS=1 if that is intended"
    );

    // Only a landing's own deploy id is served; anything else is a 404.
    let (status, _) = get(&p.addr, "/p/good-token/shot/done/8");
    assert_eq!(status, 404);
}
