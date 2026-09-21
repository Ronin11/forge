//! forge-portal against a fake forge: a fixture `PortalDoc` renders all
//! six sections in plain words, an unknown or revoked token is a plain
//! 404, a deploy target's screenshot streams through, and the two write
//! routes — `POST /p/<token>/answer` and `POST /p/<token>/ask` — reach
//! `forge answer`/`forge ask` with the right arguments, token-scoped and
//! rate-limited.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

/// Words that must never leak into the page: raw JSON field names from
/// `PortalDoc` and the task/operator vocabulary the portal is built to
/// keep off this page entirely (see docs/PORTAL.md, "What they see").
const FORBIDDEN: &[&str] = &[
    "check_ok",
    "look_ok",
    "task_id",
    "landed_at",
    "asked_at",
    "started_at",
    "created_at",
    "last_deployed_at",
    "deploy_targets",
    "workflow_hash",
    "cost_usd",
    "branch",
    "verdict",
    "attempts",
];

/// The fixture `PortalDoc`. `answered` drops the open question (as a real
/// answer's re-queue would), and `asked` adds a landed line (as a real
/// filed request would, once it lands) — the fake's stand-in for both
/// write verbs actually changing what `forge project view` reports.
fn fixture_doc(shot_path: &str, answered: bool, asked: bool) -> String {
    let questions: Vec<serde_json::Value> = if answered {
        vec![]
    } else {
        vec![
            serde_json::json!({"task_id": 42, "text": "Should annual plans get a discount?", "asked_at": 1_700_000_200_i64}),
        ]
    };
    let mut landed = vec![
        serde_json::json!({"text": "Checkout redesign shipped", "pieces": 2, "landed_at": 1_699_999_999_i64}),
        serde_json::json!({"text": "Added dark mode", "pieces": null, "landed_at": 1_699_999_998_i64}),
    ];
    if asked {
        landed.insert(
            0,
            serde_json::json!({"text": "Gift wrap orders over $50", "pieces": null, "landed_at": 1_700_000_500_i64}),
        );
    }
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
        "run_workflows": [
            {
                "name": "nightly-order-sync",
                "jobs": [
                    {"started_at": 1_700_000_100_i64, "state": "failed", "reason": "the order feed timed out"},
                ],
            },
        ],
        "initiatives": [
            {"outcome": "Ship the new checkout", "state": "in progress", "pieces": 3},
        ],
        "initiatives_more": 4,
        "questions": questions,
        "landed": landed,
        "landed_more": 5,
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

/// A fake `forge` that resolves `good-token` to project `acme`, serves
/// `project view` from whichever of the three docs (initial, post-answer,
/// post-ask) the state directory's marker files select, and logs every
/// `answer`/`ask` invocation (pipe-separated argv) to `calls.log` in that
/// same directory so a test can check exactly what the portal ran.
fn fake_script(shot_path: &str, state_dir: &Path) -> String {
    let calls = state_dir.join("calls.log");
    let answered_marker = state_dir.join("answered");
    let asked_marker = state_dir.join("asked");
    let doc_initial = state_dir.join("doc-initial.json");
    let doc_answered = state_dir.join("doc-answered.json");
    let doc_asked = state_dir.join("doc-asked.json");
    std::fs::write(&doc_initial, fixture_doc(shot_path, false, false)).unwrap();
    std::fs::write(&doc_answered, fixture_doc(shot_path, true, false)).unwrap();
    std::fs::write(&doc_asked, fixture_doc(shot_path, false, true)).unwrap();
    format!(
        r#"#!/bin/bash
CALLS="{calls}"
ANSWERED="{answered_marker}"
ASKED="{asked_marker}"
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
      view)
        if [ -f "$ANSWERED" ]; then
          cat "{doc_answered}"
        elif [ -f "$ASKED" ]; then
          cat "{doc_asked}"
        else
          cat "{doc_initial}"
        fi
        ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac
    ;;
  answer)
    printf 'answer|%s|%s|%s|%s\n' "$2" "$3" "$4" "$5" >> "$CALLS"
    touch "$ANSWERED"
    echo "answered task $2 as 99"
    ;;
  ask)
    printf 'ask|%s|%s|%s|%s\n' "$2" "$3" "$4" "$5" >> "$CALLS"
    touch "$ASKED"
    echo "concierge: a request; filed task 50 (1 queued)"
    ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        calls = calls.display(),
        answered_marker = answered_marker.display(),
        asked_marker = asked_marker.display(),
        doc_initial = doc_initial.display(),
        doc_answered = doc_answered.display(),
        doc_asked = doc_asked.display(),
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

impl Portal {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.home.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn start() -> (Portal, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let shot = home.path().join("shot.png");
    std::fs::write(&shot, b"not-a-real-png-but-bytes").unwrap();
    let fake = home.path().join("forge");
    std::fs::write(&fake, fake_script(&shot.display().to_string(), home.path())).unwrap();
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
    (Portal { child, addr, home }, shot)
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

/// One raw HTTP/1.0 form POST; returns (status, headers, body).
fn post_form(addr: &str, path: &str, form: &str) -> (u16, String, String) {
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
    (status, head.to_string(), body.to_string())
}

#[test]
fn the_six_sections_render_in_plain_words_with_no_forbidden_keys() {
    let (p, _shot) = start();
    let (status, head, body) = get(&p.addr, "/p/good-token");
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("Content-Type: text/html"), "{head}");

    for section in [
        "Running for you",
        "Being built",
        "Needs you",
        "Done",
        "Ask",
        "Your plan",
    ] {
        assert!(body.contains(&format!("<h2>{section}</h2>")), "{body}");
    }

    assert!(body.contains("acme"), "{body}");
    // A project's purpose is the operator's own words, never rendered.
    assert!(!body.contains("Keeps the orders flowing"), "{body}");
    assert!(body.contains("acme.example.com"), "{body}");
    assert!(
        body.contains("Up and running, and looking right."),
        "{body}"
    );
    // Running for you: a run workflow's last jobs render beside the
    // deploy targets, with a plain-word status and its one-line reason
    // on failure.
    assert!(body.contains("nightly-order-sync"), "{body}");
    assert!(
        body.contains("Failed \u{2014} the order feed timed out."),
        "{body}"
    );
    assert!(
        body.contains("Hasn&#39;t shipped yet.") || body.contains("Hasn't shipped yet."),
        "{body}"
    );
    assert!(body.contains("Ship the new checkout"), "{body}");
    assert!(body.contains("In progress"), "{body}");
    assert!(body.contains("3 pieces of work"), "{body}");
    assert!(body.contains("and 4 more"), "{body}");
    assert!(body.contains("Checkout redesign shipped"), "{body}");
    assert!(body.contains("2 pieces of work"), "{body}");
    assert!(body.contains("Added dark mode"), "{body}");
    // Every moment goes through the same tag: Unix seconds in a data
    // attribute for the script, UTC text inside for a viewer without it.
    assert!(
        body.contains(
            r#"<time data-ts="1699999999" data-prefix="Shipped ">Shipped Nov 14, 2023, 22:13 UTC</time>"#
        ),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<time data-ts="1700000000" data-prefix="Last updated ">Last updated Nov 14, 2023, 22:13 UTC</time>"#
        ),
        "{body}"
    );
    assert!(
        body.contains(r#"<time data-ts="1700000100">Nov 14, 2023, 22:15 UTC</time>"#),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<time data-ts="1700000200" data-prefix="Asked ">Asked Nov 14, 2023, 22:16 UTC</time>"#
        ),
        "{body}"
    );
    assert_eq!(body.matches("<time ").count(), 6, "{body}");
    assert!(body.contains("<script>"), "{body}");
    assert!(body.contains("and 5 more"), "{body}");
    assert!(body.contains("on our cloud"), "{body}");
    assert!(body.contains("Order intake syncs nightly"), "{body}");
    assert!(body.contains("Add CSV export"), "{body}");
    assert!(
        body.contains(r#"<img src="/p/good-token/shot/prod""#),
        "{body}"
    );

    // Needs you: the open question renders as a form that posts the
    // answer back through this same token.
    assert!(
        body.contains("Should annual plans get a discount?"),
        "{body}"
    );
    assert!(
        body.contains(r#"<form class="ask" method="post" action="/p/good-token/answer">"#),
        "{body}"
    );
    assert!(body.contains(r#"name="id" value="42""#), "{body}");

    // Ask: a box posting to this token's own ask route.
    assert!(
        body.contains(r#"<form class="ask" method="post" action="/p/good-token/ask">"#),
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

#[test]
fn answering_posts_through_to_forge_answer_and_requeues() {
    let (p, _shot) = start();
    let (status, _, body) = post_form(&p.addr, "/p/good-token/answer", "id=42&text=Yes%2C+10%25.");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        p.calls(),
        vec!["answer|42|Yes, 10%.|--by|customer".to_string()]
    );
    // The re-read page no longer lists the question the fake's answer
    // handler just re-queued.
    assert!(
        !body.contains("Should annual plans get a discount?"),
        "{body}"
    );

    // A GET never answers: no such route, no state change.
    let (status, _, _) = get(&p.addr, "/p/good-token/answer");
    assert_eq!(status, 405);

    // Wrong token: the same 404 as everything else, and no call reaches
    // `forge` at all.
    let before = p.calls().len();
    let (status, _, _) = post_form(&p.addr, "/p/wrong-token/answer", "id=1&text=x");
    assert_eq!(status, 404);
    assert_eq!(p.calls().len(), before);
}

#[test]
fn asking_posts_through_to_forge_ask_and_shows_the_reply_line() {
    let (p, _shot) = start();
    let (status, _, body) = post_form(&p.addr, "/p/good-token/ask", "message=Please+add+gift+wrap");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        p.calls(),
        vec!["ask|acme|Please add gift wrap|--from|customer".to_string()]
    );
    // The reply line `forge ask` printed comes back on the page.
    assert!(
        body.contains("concierge: a request; filed task 50 (1 queued)"),
        "{body}"
    );
    // The re-read page reflects what the fake's ask handler filed.
    assert!(body.contains("Gift wrap orders over $50"), "{body}");
}

#[test]
fn a_blank_message_or_answer_is_the_fixed_write_error_and_calls_forge_not_at_all() {
    let (p, _shot) = start();
    let (status, _, _) = post_form(&p.addr, "/p/good-token/ask", "message=");
    assert_eq!(status, 400);
    let (status, _, _) = post_form(&p.addr, "/p/good-token/answer", "id=42&text=");
    assert_eq!(status, 400);
    assert!(p.calls().is_empty());
}

#[test]
fn the_eleventh_write_in_a_minute_is_refused() {
    let (p, _shot) = start();
    for n in 0..10 {
        let (status, _, body) = post_form(&p.addr, "/p/good-token/ask", &format!("message=req{n}"));
        assert_eq!(status, 200, "write {n}: {body}");
    }
    assert_eq!(p.calls().len(), 10);
    let (status, _, body) = post_form(&p.addr, "/p/good-token/ask", "message=one+too+many");
    assert_eq!(status, 429, "{body}");
    // The refused write never reaches `forge`.
    assert_eq!(p.calls().len(), 10);
}
