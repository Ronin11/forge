//! forge-portal against a fake forge: a fixture `PortalDoc` renders the
//! four read-only sections in plain words, an unknown or revoked token is
//! a plain 404, and a deploy target's screenshot streams through.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

/// Words that must never leak into the page: raw JSON field names from
/// `PortalDoc` and the task/operator vocabulary the portal is built to
/// keep off this page entirely (see docs/PORTAL.md, "What they see").
const FORBIDDEN: &[&str] = &[
    "check_ok",
    "look_ok",
    "task_id",
    "landed_at",
    "created_at",
    "last_deployed_at",
    "deploy_targets",
    "workflow_hash",
    "cost_usd",
    "branch",
    "verdict",
    "attempts",
];

fn fixture_doc(shot_path: &str) -> String {
    serde_json::json!({
        "project": "acme",
        "purpose": "Keeps the orders flowing",
        "deploy_targets": [
            {
                "name": "prod",
                "where_it_runs": "acme.example.com",
                "last_deployed_at": 1_700_000_000,
                "check_ok": true,
                "look_ok": true,
                "screenshot": shot_path,
            },
            {
                "name": "staging",
                "where_it_runs": "staging.acme.example.com",
                "last_deployed_at": null,
                "check_ok": null,
                "look_ok": null,
                "screenshot": null,
            },
        ],
        "initiatives": [
            {"outcome": "Ship the new checkout", "state": "in progress"},
        ],
        "questions": [
            {"task_id": 42, "text": "Should annual plans get a discount?"},
        ],
        "landed": [
            {"text": "Added dark mode", "landed_at": 1_699_999_999_i64},
        ],
        "brief": {
            "where_it_runs": "on our cloud",
            "workflows": ["Order intake syncs nightly"],
        },
        "backlog": [
            {"id": 7, "text": "Add CSV export", "created_at": 1_699_999_000_i64},
        ],
    })
    .to_string()
}

fn fake_script(shot_path: &str) -> String {
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
      view) echo '{doc}' ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac
    ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        doc = fixture_doc(shot_path)
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

fn start() -> (Portal, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let shot = home.path().join("shot.png");
    std::fs::write(&shot, b"not-a-real-png-but-bytes").unwrap();
    let fake = home.path().join("forge");
    std::fs::write(&fake, fake_script(&shot.display().to_string())).unwrap();
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
    (
        Portal {
            child,
            addr,
            _home: home,
        },
        shot,
    )
}

/// One raw HTTP/1.0 request; returns (status, headers, body).
fn get(addr: &str, path: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: x\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_string(), body.to_string())
}

#[test]
fn the_four_sections_render_in_plain_words_with_no_forbidden_keys() {
    let (p, _shot) = start();
    let (status, head, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("Content-Type: text/html"), "{head}");

    for section in ["Running for you", "Being built", "Done", "Your plan"] {
        assert!(body.contains(&format!("<h2>{section}</h2>")), "{body}");
    }

    assert!(body.contains("acme"), "{body}");
    assert!(body.contains("Keeps the orders flowing"), "{body}");
    assert!(body.contains("acme.example.com"), "{body}");
    assert!(
        body.contains("Up and running, and looking right."),
        "{body}"
    );
    assert!(
        body.contains("Hasn&#39;t shipped yet.") || body.contains("Hasn't shipped yet."),
        "{body}"
    );
    assert!(body.contains("Ship the new checkout"), "{body}");
    assert!(body.contains("In progress"), "{body}");
    assert!(body.contains("Added dark mode"), "{body}");
    assert!(body.contains("Shipped Nov 14, 2023"), "{body}");
    assert!(body.contains("on our cloud"), "{body}");
    assert!(body.contains("Order intake syncs nightly"), "{body}");
    assert!(body.contains("Add CSV export"), "{body}");
    assert!(
        body.contains(r#"<img src="/p/good-token/shot/prod""#),
        "{body}"
    );

    // Needs you and the Ask box are a later build-order step (docs/PORTAL.md):
    // step 2 is read-only and shows only the four sections above.
    assert!(
        !body.contains("Should annual plans get a discount?"),
        "{body}"
    );

    for key in FORBIDDEN {
        assert!(!body.contains(key), "forbidden key {key} leaked: {body}");
    }
}

#[test]
fn an_unknown_or_revoked_token_is_a_plain_404() {
    let (p, _shot) = start();
    let (status, _, body) = get(&p.addr, "/p/wrong-token");
    assert_eq!(status, 404);
    assert!(!body.contains("acme"), "{body}");
    assert!(!body.contains("Keeps the orders flowing"), "{body}");

    let (status, _, _) = get(&p.addr, "/p/");
    assert_eq!(status, 404);

    let (status, _, _) = get(&p.addr, "/");
    assert_eq!(status, 404);
}

#[test]
fn a_deploy_targets_screenshot_streams_and_a_missing_one_is_404() {
    let (p, shot) = start();
    let (status, head, body) = get(&p.addr, "/p/good-token/shot/prod");
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("Content-Type: image/png"), "{head}");
    assert_eq!(body.as_bytes(), std::fs::read(&shot).unwrap().as_slice());

    // staging has no screenshot field set.
    let (status, _, _) = get(&p.addr, "/p/good-token/shot/staging");
    assert_eq!(status, 404);

    // unknown target name.
    let (status, _, _) = get(&p.addr, "/p/good-token/shot/nope");
    assert_eq!(status, 404);

    // wrong token, even for a target that exists.
    let (status, _, _) = get(&p.addr, "/p/wrong-token/shot/prod");
    assert_eq!(status, 404);
}
