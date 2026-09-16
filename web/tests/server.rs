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
  plugin)
    case "$2" in
      list) echo '[{"name":"echo","description":"says things","dir":"/p/echo","source":"/p","capabilities":["events"],"restart":"always","enabled":true}]' ;;
      status) echo '[{"name":"echo","enabled":true,"state":"running","pid":123,"uptime_secs":45,"restart_count":0,"last_exit":null}]' ;;
      enable) echo "$3 enabled" ;;
      disable) echo "$3 disabled" ;;
      logs) echo "log line 1"; echo "log line 2" ;;
      *) echo "unexpected plugin: $*" >&2; exit 2 ;;
    esac ;;
  project)
    case "$2" in
      list) echo '[{"name":"demo","purpose":"a demo project","queued":1,"running":0,"succeeded":2,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":3.5,"repos":[],"created_at":1}]' ;;
      show) echo "{\"name\":\"$3\",\"purpose\":\"a demo project\",\"queued\":1,\"running\":0,\"succeeded\":2,\"failed\":0,\"unverified\":0,\"blocked\":0,\"withdrawn\":0,\"cost_usd\":3.5,\"workflow\":null,\"per_task_usd\":null,\"per_initiative_usd\":null,\"repos\":[],\"created_at\":1}" ;;
      backlog) echo "[{\"id\":1,\"project\":\"$3\",\"text\":\"do the thing\",\"created_at\":1,\"done_at\":null}]" ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac ;;
  initiative)
    case "$2" in
      list) echo "[{\"id\":5,\"project\":\"$3\",\"outcome\":\"ship it\",\"state\":\"open\",\"held_rule\":null,\"queued\":1,\"running\":0,\"succeeded\":0,\"failed\":0,\"unverified\":0,\"blocked\":0,\"withdrawn\":0,\"cost_usd\":1.25,\"budget_usd\":null,\"stop_after_same_rule\":3,\"created_at\":1,\"settled_at\":null}]" ;;
      report) echo "{\"id\":$3,\"project\":\"demo\",\"outcome\":\"ship it\",\"state\":\"open\",\"held_rule\":null,\"budget_usd\":null,\"stop_after_same_rule\":3,\"tasks\":[{\"id\":9,\"state\":\"succeeded\",\"reason\":\"\"}],\"refused\":[],\"rulings\":[],\"questions\":[],\"cost_usd\":1.25,\"elapsed_secs\":null,\"created_at\":1,\"settled_at\":null}" ;;
      *) echo "unexpected initiative: $*" >&2; exit 2 ;;
    esac ;;
  stats) cat <<'JSON'
{"workflows":[],"steps":[],"journal":{"attempts":0,"succeeded":0,"succeeded_share":null,"mean_turns":0.0,"mean_first_edit":null,"mean_cost_usd":0.0},"no_journal":{"attempts":0,"succeeded":0,"succeeded_share":null,"mean_turns":0.0,"mean_first_edit":null,"mean_cost_usd":0.0},"by_role":[{"role":"code","provider":"anthropic","model":"claude-sonnet-5","attempts":10,"succeeded":8,"succeeded_share":0.8,"mean_turns":12.5,"mean_cost_usd":1.23,"mean_secs":340.0,"landed":6,"broke_base":1,"broke_base_share":0.16666666666666666},{"role":"review","provider":"anthropic","model":"claude-haiku-4-5","attempts":4,"succeeded":4,"succeeded_share":1.0,"mean_turns":3.0,"mean_cost_usd":0.1,"mean_secs":20.0}]}
JSON
  ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

const FAKE_REPOMAP: &str = r#"#!/bin/bash
case "$1" in
  edges) cat <<'JSON'
{"nodes":[{"path":"src/a.rs","lang":"rust","symbols":3},{"path":"src/b.rs","lang":"rust","symbols":1},{"path":"web/x.js","lang":"javascript","symbols":0}],"edges":[{"from":"src/a.rs","to":"src/b.rs","kind":"import"}]}
JSON
  ;;
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
    let bin_dir = home.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_repomap = bin_dir.join("forge-repomap");
    std::fs::write(&fake_repomap, FAKE_REPOMAP).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&fake_repomap, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-web"))
        .args(["--bind", "127.0.0.1:0"])
        .env("FORGE_BIN", &fake)
        .env("FORGE2_HOME", home.path())
        .env("PATH", path)
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
        "/api/plugins",
        "/app.js",
        "/projects",
        "/projects/demo",
        "/initiatives/5",
        "/api/projects",
        "/api/projects/demo",
        "/api/initiatives/5",
        "/graph",
        "/api/graph?repo=%2Fsome%2Frepo",
        "/stats",
        "/api/stats",
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
fn the_plugins_route_merges_list_and_status_and_the_action_routes_hit_the_cli() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let (status, _, body) = get(&w.addr, "/plugins", &cookie);
    assert_eq!(status, 200);
    assert!(body.contains(r#"<script src="/app.js">"#), "{body}");

    let (status, _, body) = get(&w.addr, "/api/plugins", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["name"], "echo");
    assert_eq!(v[0]["description"], "says things");
    assert_eq!(v[0]["capabilities"][0], "events");
    assert_eq!(v[0]["enabled"], true);
    assert_eq!(v[0]["state"], "running");
    assert_eq!(v[0]["pid"], 123);
    assert_eq!(v[0]["uptime_secs"], 45);

    let (status, _, body) = post(&w.addr, "/api/plugins/echo/enable", &cookie);
    assert_eq!(status, 200);
    assert!(body.contains("echo enabled"), "{body}");
    let (status, _, _) = get(&w.addr, "/api/plugins/echo/enable", &cookie);
    assert_eq!(status, 405);

    let (status, _, body) = post(&w.addr, "/api/plugins/echo/disable", &cookie);
    assert_eq!(status, 200);
    assert!(body.contains("echo disabled"), "{body}");

    let (status, head, body) = get(&w.addr, "/api/plugins/echo/logs", &cookie);
    assert_eq!(status, 200);
    assert!(head.contains("Content-Type: text/plain"), "{head}");
    assert!(
        body.contains("log line 1") && body.contains("log line 2"),
        "{body}"
    );
    let (status, _, _) = post(&w.addr, "/api/plugins/echo/logs", &cookie);
    assert_eq!(status, 405);
}

#[test]
fn the_projects_and_initiatives_routes_pass_forge_json_through() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);

    for view in ["/projects", "/projects/demo", "/initiatives/5"] {
        let (status, _, body) = get(&w.addr, view, &cookie);
        assert_eq!(status, 200, "{view}");
        assert!(body.contains(r#"<script src="/app.js">"#), "{view}: {body}");
    }

    let (status, _, body) = get(&w.addr, "/api/projects", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["name"], "demo");
    assert_eq!(v[0]["cost_usd"], 3.5);

    let (status, _, body) = get(&w.addr, "/api/projects/demo", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["name"], "demo");
    assert_eq!(v["purpose"], "a demo project");

    let (status, _, body) = get(&w.addr, "/api/projects/demo/backlog", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["project"], "demo");
    assert_eq!(v[0]["text"], "do the thing");

    let (status, _, body) = get(&w.addr, "/api/projects/demo/initiatives", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["id"], 5);
    assert_eq!(v[0]["project"], "demo");

    let (status, _, body) = get(&w.addr, "/api/initiatives/5", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], 5);
    assert_eq!(v["outcome"], "ship it");
    assert_eq!(v["tasks"][0]["id"], 9);

    let (status, _, _) = get(&w.addr, "/api/initiatives/x", &cookie);
    assert_eq!(status, 404);

    // The project filter on /tasks passes through as --project.
    let (status, _, body) = get(&w.addr, "/api/tasks?project=demo", &cookie);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["args"], "--json --limit 100 --project demo");
}

#[test]
fn the_stats_page_and_route_show_the_by_role_breakdown() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);

    let (status, _, body) = get(&w.addr, "/stats", &cookie);
    assert_eq!(status, 200);
    assert!(body.contains(r#"<script src="/app.js">"#), "{body}");

    let (status, _, body) = get(&w.addr, "/api/stats", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["by_role"][0]["role"], "code");
    assert_eq!(v["by_role"][0]["provider"], "anthropic");
    assert_eq!(v["by_role"][0]["model"], "claude-sonnet-5");
    assert_eq!(v["by_role"][0]["attempts"], 10);
    assert_eq!(v["by_role"][0]["landed"], 6);
    assert_eq!(v["by_role"][0]["broke_base"], 1);
    assert_eq!(v["by_role"][1]["role"], "review");
    assert_eq!(v["by_role"][1]["model"], "claude-haiku-4-5");
    assert!(v["by_role"][1]["landed"].is_null(), "{body}");
}

#[test]
fn the_graph_page_and_route_run_forge_repomap_edges_and_pass_its_json_through() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);

    let (status, _, body) = get(&w.addr, "/graph", &cookie);
    assert_eq!(status, 200);
    assert!(body.contains(r#"<script src="/app.js">"#), "{body}");

    let (status, _, body) = get(&w.addr, "/api/graph?repo=%2Fsome%2Frepo", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["nodes"][0]["path"], "src/a.rs");
    assert_eq!(v["nodes"][0]["symbols"], 3);
    assert_eq!(v["edges"][0]["from"], "src/a.rs");
    assert_eq!(v["edges"][0]["to"], "src/b.rs");

    let (status, _, body) = get(&w.addr, "/api/graph", &cookie);
    assert_eq!(status, 400, "{body}");
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
