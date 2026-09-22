//! "Running for you" (see `portal/src/main.rs`'s module doc): each
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

/// One automation, `quote-by-text`, with its workflow's own description
/// and two runs: a rehearsed (`dry_run`) run that went ok and logged one
/// effect, and a real run that failed, its reason the human rung's
/// question in plain words — never the job id, the workflow name, or any
/// effect's `kind`/`target`, which only the operator's own `forge job
/// show` carries.
const DOC: &str = r#"{
    "project": "acme",
    "purpose": "internal only, never rendered",
    "deploy_targets": [],
    "run_workflows": [
        {
            "name": "quote-by-text",
            "description": "a customer texts a photo of a job; they get a quote back and it goes in the book",
            "jobs": [
                {
                    "started_at": 1700000200,
                    "state": "ok",
                    "dry_run": true,
                    "effects": ["quoted the Hendersons' fence job at $1,240"],
                    "reason": null
                },
                {
                    "started_at": 1700000100,
                    "state": "failed",
                    "dry_run": false,
                    "effects": [],
                    "reason": "no price sheet entry for this job"
                }
            ]
        }
    ],
    "initiatives": [],
    "initiatives_more": 0,
    "questions": [],
    "landed": [],
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

/// "Running for you"'s own markup, nothing else, to keep the snapshot
/// narrow.
fn running_section(body: &str) -> &str {
    let start = body
        .find("<section><h2>Running for you</h2>")
        .expect("a Running for you section");
    let rest = &body[start..];
    let end = rest.find("</section>").expect("the section is closed") + "</section>".len();
    &rest[..end]
}

#[test]
fn an_automation_describes_itself_and_each_run_speaks_in_plain_sentences() {
    let p = start();
    let (status, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");

    let section = running_section(&body);

    // Described from the workflow's own words, not its file name alone.
    assert!(
        section.contains("a customer texts a photo of a job; they get a quote back"),
        "{section}"
    );

    // The ok run was only a rehearsal, and its effect renders as the
    // operation's own sentence — never a "message"/"row" kind or a
    // target (a phone number, a table name).
    assert!(
        section.contains("Rehearsal \u{2014} Ran fine."),
        "{section}"
    );
    assert!(
        section.contains("quoted the Hendersons&#39; fence job at $1,240"),
        "{section}"
    );
    assert!(!section.contains("kind"), "{section}");
    assert!(!section.contains("target"), "{section}");

    // The failed run's reason is the human rung's question, in the
    // customer's own words — no job id, no workflow name repeated.
    assert!(
        section.contains("Failed \u{2014} no price sheet entry for this job."),
        "{section}"
    );

    let snap = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/running.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snap, section).unwrap();
    }
    let want = std::fs::read_to_string(&snap).unwrap_or_default();
    assert_eq!(
        section, want,
        "the running-for-you snapshot changed; re-run with UPDATE_SNAPSHOTS=1 if that is intended"
    );
}
