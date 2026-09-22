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
      list) echo '[{"name":"demo","purpose":"a demo project","queued":1,"running":0,"succeeded":2,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":3.5,"repos":[{"repo":"/repos/demo","scope":null}],"created_at":1}]' ;;
      show) echo "{\"name\":\"$3\",\"purpose\":\"a demo project\",\"queued\":1,\"running\":0,\"succeeded\":2,\"failed\":0,\"unverified\":0,\"blocked\":0,\"withdrawn\":0,\"cost_usd\":3.5,\"workflow\":null,\"per_task_usd\":null,\"per_initiative_usd\":null,\"repos\":[{\"repo\":\"/repos/$3\",\"scope\":null}],\"created_at\":1}" ;;
      backlog) echo "[{\"id\":1,\"project\":\"$3\",\"text\":\"do the thing\",\"created_at\":1,\"done_at\":null}]" ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac ;;
  workflows)
    shift
    case "$1" in
      show)
        name="$2"; proj=""
        args=("$@")
        for ((i=0; i<${#args[@]}; i++)); do
          if [ "${args[i]}" = "--project" ]; then proj="${args[i+1]}"; fi
        done
        if [ "$proj" = "demo" ] && [ "$name" = "repo-flow" ]; then
          echo '{"name":"repo-flow","source":"repo","path":".forge/workflows/repo-flow.toml","kind":"build","text":"name = \"repo-flow\"\n","steps":[{"name":"setup","kind":"operation","contract":"setup","model":null,"max_turns":null,"timeout_secs":null,"description":"prepares the tree"}],"measured":{"current":{"n":0,"known":false},"previous":null,"all_versions":{"n":0,"known":false},"regressed":false,"by_provider":[]}}'
        else
          echo '{"name":"direct","source":"catalog","path":"/home/x/workflows/direct.toml","kind":"build","text":"name = \"direct\"\n","steps":[{"name":"setup","kind":"operation","contract":"setup","model":null,"max_turns":null,"timeout_secs":null,"description":"prepares the tree"},{"name":"code","kind":"directive","contract":"code","model":"claude-sonnet-5","max_turns":40,"timeout_secs":1800,"description":"writes the change"}],"measured":{"current":{"n":12,"known":true,"succeeded":10,"rate":0.8333333333333334,"rate_lo":0.55,"rate_hi":0.95,"cost_per_task":1.2,"cost_per_success":1.44,"mean_secs":300.0,"mean_attempts":1.1,"lineages":12,"lineages_verified":10},"previous":null,"all_versions":{"n":12,"known":true},"regressed":false,"by_provider":[]}}'
        fi ;;
      lint)
        cat >/dev/null
        nm=""
        args=("$@")
        for ((i=0; i<${#args[@]}; i++)); do
          if [ "${args[i]}" = "--name" ]; then nm="${args[i+1]}"; fi
        done
        if [ "$nm" = "bad" ]; then
          echo '{"problems":[{"line":3,"message":"unknown action \"nope\""}]}'
          exit 1
        else
          echo '{"problems":[]}'
        fi ;;
      put)
        name="$2"; shift 2
        cat >/dev/null
        repo=""
        while [ $# -gt 0 ]; do
          case "$1" in
            --repo) repo="$2"; shift 2 ;;
            *) shift ;;
          esac
        done
        if [ -n "$repo" ]; then
          echo "42"
        else
          echo "abc123abc123abc123abc123abc123abc123abcd"
        fi ;;
      *)
        proj=""
        args=("$@")
        for ((i=0; i<${#args[@]}; i++)); do
          if [ "${args[i]}" = "--project" ]; then proj="${args[i+1]}"; fi
        done
        if [ "$proj" = "demo" ]; then
          echo '{"workflows":[{"name":"direct","kind":"build","source":"catalog","hash":"h1","description":"d","path":"/x/direct.toml","steps":[],"resolved":[{"action":"setup","kind":"operation","contract":"setup"}],"meta":{},"measured":{"current":{"n":12,"known":true,"succeeded":10,"rate":0.8333333333333334,"rate_lo":0.55,"rate_hi":0.95,"cost_per_task":1.2,"cost_per_success":1.44,"mean_secs":300.0,"mean_attempts":1.1,"lineages":12,"lineages_verified":10},"regressed":false}},{"name":"repo-flow","kind":"build","source":"repo","hash":"h2","description":"r","path":".forge/workflows/repo-flow.toml","steps":[{"action":"setup"}],"resolved":null,"meta":{},"measured":{"current":{"n":0,"known":false},"regressed":false}}],"actions":[],"min_runs_for_known":5,"lookback":50}'
        else
          echo '{"workflows":[{"name":"direct","kind":"build","source":"catalog","hash":"h1","description":"d","path":"/x/direct.toml","steps":[],"resolved":[{"action":"setup","kind":"operation","contract":"setup"}],"meta":{},"measured":{"current":{"n":12,"known":true,"succeeded":10,"rate":0.8333333333333334,"rate_lo":0.55,"rate_hi":0.95,"cost_per_task":1.2,"cost_per_success":1.44,"mean_secs":300.0,"mean_attempts":1.1,"lineages":12,"lineages_verified":10},"regressed":false}}],"actions":[],"min_runs_for_known":5,"lookback":50}'
        fi ;;
    esac ;;
  initiative)
    case "$2" in
      list) echo "[{\"id\":5,\"project\":\"$3\",\"outcome\":\"ship it\",\"state\":\"open\",\"held_rule\":null,\"queued\":1,\"running\":0,\"succeeded\":0,\"failed\":0,\"unverified\":0,\"blocked\":0,\"withdrawn\":0,\"cost_usd\":1.25,\"budget_usd\":null,\"stop_after_same_rule\":3,\"created_at\":1,\"settled_at\":null}]" ;;
      report) echo "{\"id\":$3,\"project\":\"demo\",\"outcome\":\"ship it\",\"state\":\"open\",\"held_rule\":null,\"budget_usd\":null,\"stop_after_same_rule\":3,\"tasks\":[{\"id\":9,\"state\":\"succeeded\",\"reason\":\"\"}],\"refused\":[],\"rulings\":[],\"questions\":[],\"cost_usd\":1.25,\"elapsed_secs\":null,\"created_at\":1,\"settled_at\":null}" ;;
      *) echo "unexpected initiative: $*" >&2; exit 2 ;;
    esac ;;
  job)
    case "$2" in
      list) cat <<'JSON'
[{"id":1,"project":"demo","workflow":"nightly","workflow_hash":"abc123","landed_sha":"","trigger_kind":"cron","trigger_ref":"0 * * * *","state":"ok","workflow_source":"repo","dry_run":false,"started_at":1000,"finished_at":1010,"cost_usd":0.42,"verdict_json":"[]","due_at":null},{"id":2,"project":"demo","workflow":"nightly","workflow_hash":"abc123","landed_sha":"","trigger_kind":"cron","trigger_ref":"0 * * * *","state":"running","workflow_source":"repo","dry_run":false,"started_at":2000,"finished_at":null,"cost_usd":null,"verdict_json":"","due_at":null}]
JSON
        ;;
      show) echo "{\"id\":$3,\"project\":\"demo\",\"workflow\":\"nightly\",\"workflow_hash\":\"abc123\",\"landed_sha\":\"\",\"trigger_kind\":\"cron\",\"trigger_ref\":\"0 * * * *\",\"state\":\"ok\",\"workflow_source\":\"repo\",\"dry_run\":false,\"started_at\":1000,\"finished_at\":1010,\"cost_usd\":0.42,\"verdict_json\":\"[]\",\"due_at\":null,\"steps\":[{\"id\":1,\"job_id\":$3,\"seq\":1,\"action\":\"notify\",\"kind\":\"operation\",\"provider\":\"\",\"model\":\"\",\"cost_usd\":null,\"started_at\":1000,\"finished_at\":1005,\"exit_code\":0,\"output_ref\":\"out/1\"}],\"effects\":[{\"id\":1,\"job_id\":$3,\"seq\":1,\"kind\":\"message\",\"target\":\"ops-channel\",\"summary\":\"posted status\",\"dry_run\":false}]}" ;;
      fire)
        proj="$3"; shift 3
        while [ $# -gt 0 ]; do
          case "$1" in
            --webhook) hook="$2" ;;
            --input) input="$2" ;;
            --ref) ref="$2" ;;
            --token) token="$2" ;;
          esac
          shift
        done
        { echo "fire $proj $hook ref=${ref:-none} mode=$(stat -c %a "$input")"; cat "$input"; echo; } >> "$FORGE2_HOME/fire.log"
        case "$token" in
          good) echo 7 ;;
          *) echo "invalid webhook token for $proj/$hook: pass --token (got $token)" >&2; exit 1 ;;
        esac ;;
      *) echo "unexpected job: $*" >&2; exit 2 ;;
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
        home,
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
        "/time.js",
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
        "/jobs",
        "/jobs/1",
        "/api/jobs",
        "/api/job/1",
        "/workflows",
        "/workflows/direct",
        "/api/workflows",
        "/api/workflows/direct",
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
    // The one time helper is served ahead of the page that uses it.
    let (status, head, body) = get(&w.addr, "/time.js", &cookie);
    assert_eq!(status, 200);
    assert!(
        head.contains("Content-Type: application/javascript"),
        "{head}"
    );
    assert!(body.contains("fmtTime"), "{body}");
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
fn the_jobs_page_lists_two_fixture_jobs_and_shows_one_with_its_steps_and_effects() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);

    for view in ["/jobs", "/jobs/1"] {
        let (status, _, body) = get(&w.addr, view, &cookie);
        assert_eq!(status, 200, "{view}");
        assert!(body.contains(r#"<script src="/app.js">"#), "{view}: {body}");
    }
    // The nav links to /jobs from every page, including the header on /tasks.
    let (_, _, body) = get(&w.addr, "/tasks", &cookie);
    assert!(body.contains(r#"<script src="/app.js">"#), "{body}");
    let (_, _, app_js) = get(&w.addr, "/app.js", &cookie);
    assert!(
        app_js.contains("href=\"/jobs\""),
        "app.js must link /jobs from the header nav"
    );
    assert!(
        app_js.contains("INVALIDATES")
            && app_js.contains("job_started")
            && app_js.contains("job_finished"),
        "app.js must invalidate the jobs views on job_started/job_finished"
    );

    // forge job list --json, through /api/jobs: two fixture jobs.
    let (status, _, body) = get(&w.addr, "/api/jobs", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2, "{body}");
    assert_eq!(v[0]["id"], 1);
    assert_eq!(v[0]["project"], "demo");
    assert_eq!(v[0]["workflow"], "nightly");
    assert_eq!(v[0]["state"], "ok");
    assert_eq!(v[0]["cost_usd"], 0.42);
    assert_eq!(v[0]["started_at"], 1000);
    assert_eq!(v[1]["id"], 2);
    assert_eq!(v[1]["state"], "running");

    // forge job show ID --json, through /api/job/<id>: steps and effects.
    let (status, _, body) = get(&w.addr, "/api/job/1", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["steps"][0]["action"], "notify");
    assert_eq!(v["steps"][0]["kind"], "operation");
    assert_eq!(v["effects"][0]["kind"], "message");
    assert_eq!(v["effects"][0]["summary"], "posted status");

    let (status, _, _) = get(&w.addr, "/api/job/x", &cookie);
    assert_eq!(status, 404);
}

#[test]
fn the_workflows_page_lists_catalog_and_repo_workflows_and_the_editor_lints_and_saves() {
    let w = start();
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);

    for view in ["/workflows", "/workflows/direct"] {
        let (status, _, body) = get(&w.addr, view, &cookie);
        assert_eq!(status, 200, "{view}");
        assert!(body.contains(r#"<script src="/app.js">"#), "{view}: {body}");
    }
    let (_, _, body) = get(&w.addr, "/workflows.js", &cookie);
    assert!(body.contains("renderWorkflowRows"), "{body}");
    let (_, _, index) = get(&w.addr, "/workflows", &cookie);
    assert!(index.contains(r#"<script src="/workflows.js">"#), "{index}");
    let (_, _, app_js) = get(&w.addr, "/app.js", &cookie);
    assert!(
        app_js.contains("href=\"/workflows\""),
        "app.js must link /workflows from the header nav"
    );
    assert!(
        app_js.contains("workflows:")
            && app_js.contains("task_done")
            && app_js.contains("job_finished"),
        "app.js must invalidate the workflows views on task_done/job_finished"
    );

    // forge workflows --json, through /api/workflows: the catalog once,
    // plus every project's own repo workflows, tagged with the project.
    let (status, _, body) = get(&w.addr, "/api/workflows", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{body}");
    let direct = rows.iter().find(|r| r["name"] == "direct").unwrap();
    assert_eq!(direct["source"], "catalog");
    assert_eq!(direct["kind"], "build");
    assert!(direct["project"].is_null(), "{body}");
    let repo_flow = rows.iter().find(|r| r["name"] == "repo-flow").unwrap();
    assert_eq!(repo_flow["source"], "repo");
    assert_eq!(repo_flow["project"], "demo");

    // forge workflows show NAME --json, through /api/workflows/<name>.
    let (status, _, body) = get(&w.addr, "/api/workflows/direct", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["name"], "direct");
    assert_eq!(v["source"], "catalog");
    assert!(v["text"].as_str().unwrap().contains("direct"), "{body}");
    assert_eq!(v["steps"][0]["name"], "setup");
    assert_eq!(v["measured"]["current"]["n"], 12);

    // ...and with ?project=, a repository workflow.
    let (status, _, body) = get(&w.addr, "/api/workflows/repo-flow?project=demo", &cookie);
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["source"], "repo");
    assert_eq!(v["steps"][0]["name"], "setup");

    // forge workflows lint --stdin, through POST /api/workflows/<name>/lint:
    // the body is the candidate text, plain.
    let (status, _, body) = post_body(
        &w.addr,
        "/api/workflows/ok/lint",
        &cookie,
        "name = \"ok\"\n",
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["problems"].as_array().unwrap().len(), 0, "{body}");

    let (status, _, body) = post_body(&w.addr, "/api/workflows/bad/lint", &cookie, "garbage");
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["problems"][0]["line"], 3);
    assert!(
        v["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("nope"),
        "{body}"
    );
    let (status, _, _) = get(&w.addr, "/api/workflows/ok/lint", &cookie);
    assert_eq!(status, 405);

    // forge workflows put NAME --stdin --message MSG, through
    // POST /api/workflows/<name>: a catalog save commits and returns the
    // hash.
    let (status, _, body) = post_body(
        &w.addr,
        "/api/workflows/put-me",
        &cookie,
        r#"{"text":"name = \"put-me\"\n","message":"add put-me","project":null}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["result"], "committed");
    assert_eq!(v["hash"], "abc123abc123abc123abc123abc123abc123abcd");

    // ...and with a project, "file as a task" — resolves the project's
    // first repo and calls put --repo, filing a task instead.
    let (status, _, body) = post_body(
        &w.addr,
        "/api/workflows/repo-flow",
        &cookie,
        r#"{"text":"name = \"repo-flow\"\n","message":"m","project":"demo"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["result"], "filed");
    assert_eq!(v["task_id"], 42);
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

/// One raw HTTP/1.0 POST with a body; returns (status, headers, body).
fn post_body(addr: &str, path: &str, extra: &str, body: &str) -> (u16, String, String) {
    post(
        addr,
        path,
        &format!("{extra}Content-Length: {}\r\n\r\n{body}", body.len()),
    )
}

fn fire_log(w: &Web) -> String {
    std::fs::read_to_string(w.home.path().join("fire.log")).unwrap_or_default()
}

#[test]
fn a_webhook_post_hands_its_body_and_bearer_token_to_forge_job_fire() {
    let w = start();
    // Not behind the web token: the hook's own bearer token is the credential.
    let (status, _, body) = post_body(
        &w.addr,
        "/hooks/demo/orders?ref=delivery%2042",
        "Authorization: Bearer good\r\n",
        r#"{"order":"17"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["job"], 7);
    let log = fire_log(&w);
    assert!(
        log.contains("fire demo orders ref=delivery 42 mode=600"),
        "{log}"
    );
    assert!(log.contains(r#"{"order":"17"}"#), "{log}");

    // Without a ref the kernel keys the delivery on the body; an
    // Idempotency-Key header is a ref too.
    let (status, _, _) = post_body(
        &w.addr,
        "/hooks/demo/orders",
        "Authorization: bearer good\r\nIdempotency-Key: k1\r\n",
        "{}",
    );
    assert_eq!(status, 200);
    let (status, _, _) = post_body(
        &w.addr,
        "/hooks/demo/orders",
        "Authorization: Bearer good\r\n",
        "{}",
    );
    assert_eq!(status, 200);
    let log = fire_log(&w);
    assert!(log.contains("fire demo orders ref=k1 "), "{log}");
    assert!(log.contains("fire demo orders ref=none "), "{log}");
}

#[test]
fn a_webhook_with_a_wrong_or_missing_token_is_401_and_never_echoes_the_token() {
    let w = start();
    let (status, _, body) = post_body(
        &w.addr,
        "/hooks/demo/orders",
        "Authorization: Bearer sekrit-wrong\r\n",
        "{}",
    );
    assert_eq!(status, 401, "{body}");
    assert!(!body.contains("sekrit-wrong"), "{body}");
    for extra in [
        "",
        "Authorization: Basic Zm9v\r\n",
        "Authorization: Bearer \r\n",
    ] {
        let (status, _, _) = post_body(&w.addr, "/hooks/demo/orders", extra, "{}");
        assert_eq!(status, 401, "{extra:?}");
    }
    // The token is a header credential only, and the web token is not one.
    let (status, _, _) = post_body(&w.addr, "/hooks/demo/orders?token=good", "", "{}");
    assert_eq!(status, 401);
    let cookie = format!("Cookie: forge_token={}\r\n", w.token);
    let (status, _, _) = post_body(&w.addr, "/hooks/demo/orders", &cookie, "{}");
    assert_eq!(status, 401);
}

#[test]
fn a_webhook_route_takes_only_a_post_to_a_plain_project_and_name() {
    let w = start();
    let auth = "Authorization: Bearer good\r\n";
    let (status, _, _) = get(&w.addr, "/hooks/demo/orders", auth);
    assert_eq!(status, 405);
    for path in [
        "/hooks/demo",
        "/hooks/demo/",
        "/hooks//orders",
        "/hooks/demo/a/b",
        "/hooks/demo/a%2Fb",
    ] {
        let (status, _, _) = post_body(&w.addr, path, auth, "{}");
        assert_eq!(status, 404, "{path}");
    }
    assert_eq!(fire_log(&w), "", "nothing was fired");
}

#[test]
fn a_webhook_body_over_the_limit_is_refused_before_it_reaches_forge() {
    let w = start();
    let big = "x".repeat(1024 * 1024 + 1);
    let (status, _, _) = post_body(
        &w.addr,
        "/hooks/demo/orders",
        "Authorization: Bearer good\r\n",
        &big,
    );
    assert_eq!(status, 413);
    assert_eq!(fire_log(&w), "");
}
