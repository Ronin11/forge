//! The draft editor's routes against a fake forge (docs/CLIENT.md, "The
//! draft editor"): a two-step draft with one placeholder is composed,
//! put, and the build task it filed and its `incomplete` status come
//! back. What the fake prints is canned for that draft; what it was
//! given is logged, so the test sees the web layer pass the draft
//! document and the arguments through untouched.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

const FAKE: &str = r#"#!/bin/bash
ANNOTATED='{"name":"docs-lint","kind":"build","description":"","project":"demo","steps":[{"action":"code"},{"action":"lint-docs","placeholder":{"kind":"operation","inputs":"the tree","outputs":"a verdict"}}],"settings":{},"tasks":[],"toml":"name = \"docs-lint\"\n","problems":[],"info":[],"clean":true,"pending":["lint-docs"]'
case "$1" in
  project)
    echo '{"name":"demo","repos":[{"repo":"/repos/demo","scope":null}]}' ;;
  workflows)
    [ "$2" = "draft" ] || { echo "unexpected: $*" >&2; exit 2; }
    shift 2
    verb="$1"; shift
    case "$verb" in
      list) echo '[]' ;;
      actions) echo '[{"name":"code","kind":"directive","contract":"code","description":"writes the change","consumes":["branch"],"produces":["branch"],"outcomes":[]}]' ;;
      show)
        if [ -f "$FORGE_HOME/draft-put" ]; then
          echo "$ANNOTATED,\"status\":\"incomplete\",\"tasks\":[{\"action\":\"lint-docs\",\"task_id\":51}]}" | sed 's/"tasks":\[\],//'
        else
          echo "no draft $1" >&2; exit 1
        fi ;;
      import) cat >/dev/null; echo "$ANNOTATED,\"status\":\"draft\"}" ;;
      check|save)
        cat >"$FORGE_HOME/draft-$verb.json"
        echo "$ANNOTATED,\"status\":\"draft\"}" ;;
      put)
        name="$1"; shift
        cat >"$FORGE_HOME/draft-put.json"
        echo "$name $*" >"$FORGE_HOME/draft-put"
        if grep -q '"placeholder"' "$FORGE_HOME/draft-put.json"; then
          echo '{"result":"incomplete","status":"incomplete","tasks":[{"action":"lint-docs","task_id":51}],"filed":[{"action":"lint-docs","task_id":51}],"pending":["lint-docs"]}'
        elif grep -q -- '--repo' "$FORGE_HOME/draft-put"; then
          echo '{"result":"filed","task_id":42}'
        else
          echo '{"result":"committed","hash":"abc123abc123abc123abc123abc123abc123abcd"}'
        fi ;;
    esac ;;
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
fn a_two_step_draft_with_a_placeholder_is_put_files_its_build_task_and_is_marked_incomplete() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let (get, post) = (
        |p: &str| request(&w.addr, "GET", p, &cookie, ""),
        |p: &str, b: &str| request(&w.addr, "POST", p, &cookie, b),
    );

    // The Draft view and the editor's own script are served.
    let (status, page) = get("/workflows/draft");
    assert_eq!(status, 200);
    assert!(page.contains(r#"<script src="/drafts.js">"#), "{page}");
    let (_, js) = get("/drafts.js");
    assert!(js.contains("renderGraph") && js.contains("addPlaceholder"));

    // The picker: every action a step may name, with its contract.
    let (status, body) = get("/api/drafts/actions");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)[0]["contract"], "code");

    // Two steps — `code`, and `lint-docs`, which is not in the catalog and
    // so carries the one-line contract the operator wrote — linted: clean,
    // the placeholder pending.
    let draft = r#"{"name":"docs-lint","kind":"build","description":"","project":"demo","steps":[{"action":"code"},{"action":"lint-docs","placeholder":{"kind":"operation","inputs":"the tree","outputs":"a verdict"}}],"settings":{},"status":"draft","tasks":[]}"#;
    let (status, body) = post("/api/drafts/check", draft);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["clean"], true, "{body}");
    assert_eq!(json(&body)["pending"][0], "lint-docs");
    let seen = std::fs::read_to_string(w.home.path().join("draft-check.json")).unwrap();
    assert!(
        seen.contains("lint-docs") && seen.contains("the tree"),
        "the document reaches forge whole: {seen}"
    );

    // Nothing is saved before a put.
    assert_eq!(get("/api/drafts/docs-lint").0, 502);

    // Put: the build task is filed, and the draft is incomplete.
    let (status, body) = post(
        "/api/drafts/docs-lint/put",
        &format!(r#"{{"draft":{draft},"message":"add docs-lint","to_repo":false}}"#),
    );
    assert_eq!(status, 200, "{body}");
    let v = json(&body);
    assert_eq!(v["result"], "incomplete");
    assert_eq!(v["filed"][0]["action"], "lint-docs");
    assert_eq!(v["filed"][0]["task_id"], 51);
    let args = std::fs::read_to_string(w.home.path().join("draft-put")).unwrap();
    assert_eq!(args.trim(), "docs-lint --message add docs-lint");

    // ...and reading it back says so, with the task it waits on.
    let (status, body) = get("/api/drafts/docs-lint");
    assert_eq!(status, 200, "{body}");
    let v = json(&body);
    assert_eq!(v["status"], "incomplete");
    assert_eq!(v["tasks"][0]["task_id"], 51);

    // A draft with no placeholder commits, or with `to_repo` files a task
    // on the project's first repository, as `forge workflows put --repo`.
    let plain = r#"{"name":"plain","kind":"build","description":"","project":"demo","steps":[{"action":"code"}],"settings":{}}"#;
    let (_, body) = post(
        "/api/drafts/plain/put",
        &format!(r#"{{"draft":{plain},"message":"m","to_repo":false}}"#),
    );
    assert_eq!(json(&body)["result"], "committed", "{body}");
    let (_, body) = post(
        "/api/drafts/plain/put",
        &format!(r#"{{"draft":{plain},"message":"m","to_repo":true}}"#),
    );
    assert_eq!(json(&body)["result"], "filed", "{body}");
    let args = std::fs::read_to_string(w.home.path().join("draft-put")).unwrap();
    assert!(args.contains("--repo /repos/demo"), "{args}");

    // A suggestion's file text is imported as a step list.
    let (status, body) = post("/api/drafts/import", "name = \"x\"\n");
    assert_eq!(status, 200, "{body}");

    // Writes need the token, and only POST writes.
    assert_eq!(
        request(&w.addr, "POST", "/api/drafts/check", "", draft).0,
        401
    );
    assert_eq!(get("/api/drafts/docs-lint/put").0, 405);
}
