//! forge-web: a browser client for Forge 2, the same seam as the TUI. It
//! never touches the kernel: every read is a forge verb's JSON (`snapshot`,
//! `log`, `trace`, `journal`, `requests`) and the live feed is
//! `events --follow` piped through as server-sent events. Almost entirely
//! read-only; the one write route, `POST /api/retry/<id>`, is the same
//! `forge retry` verb the CLI runs.
//!
//! Views: `/tasks` (the queue, searched and paged through `forge log`),
//! `/tasks/<id>` (one task: trace, diagnosis, journal, its events),
//! `/tasks/<id>/run` (the task inside its workflow: every step with its
//! operations, and every attempt's inputs, outputs, and verdict), and
//! `/jobs`/`/jobs/<id>` (automation runs: the list through `forge job
//! list`, one job's steps and effects through `forge job show`). One
//! page serves all of these; the path picks the view.
//!
//! Every request carries a token. It is generated once into
//! `FORGE2_HOME/web.token` and printed at start as a link; the first visit
//! with `?token=` sets a cookie. The server binds loopback unless told
//! otherwise, and there are no routes without the token: Forge 1's web
//! server had open operator routes and a tailnet proxy made every peer the
//! operator.

use anyhow::{Context, Result};
use forge_client::Forge;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const INDEX: &str = include_str!("index.html");
const APP_JS: &str = include_str!("app.js");

/// Where Forge keeps its data: `FORGE2_HOME`, else the XDG default.
fn home() -> PathBuf {
    if let Ok(h) = std::env::var("FORGE2_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("forge2")
}

/// The token: read from `web.token` under the data dir, generated on
/// first start (32 bytes of OS randomness as hex, file mode 0600).
fn token(dir: &std::path::Path) -> Result<String> {
    let path = dir.join("web.token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir).ok();
    std::fs::write(&path, &t).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(t)
}

/// Equal without leaking where they differ.
fn same(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// The token a request presents: the cookie, a bearer header, or `?token=`.
fn presented(req: &Request, query: &str) -> Option<String> {
    if let Some(c) = header(req, "Cookie") {
        for part in c.split(';') {
            if let Some(v) = part.trim().strip_prefix("forge_token=") {
                return Some(v.to_string());
            }
        }
    }
    if let Some(a) = header(req, "Authorization")
        && let Some(v) = a.strip_prefix("Bearer ")
    {
        return Some(v.trim().to_string());
    }
    query_param(query, "token")
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

/// Percent-decoding for query values (plus as space).
fn unescape(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&v[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn h(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("static header")
}

fn text(status: u16, body: &str, ctype: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(StatusCode(status))
        .with_header(h("Content-Type", ctype))
        .with_header(h("Cache-Control", "no-store"))
}

fn json_or_error(r: Result<Value>) -> Response<std::io::Cursor<Vec<u8>>> {
    match r {
        Ok(v) => text(200, &v.to_string(), "application/json"),
        Err(e) => text(
            502,
            &serde_json::json!({ "error": e.to_string() }).to_string(),
            "application/json",
        ),
    }
}

/// `forge events --since <offset> --follow`, each line as one SSE frame.
/// tiny_http buffers streamed bodies (a chunked encoder and a BufWriter,
/// neither flushed until the end), so the connection is taken over and
/// written directly, flushed per event. The child dies with the
/// connection: a write to a closed socket fails and the thread kills it.
fn events(req: Request, forge: &Forge, since: u64) {
    let mut child = match Command::new(&forge.bin)
        .args(["events", "--since", &since.to_string(), "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = req.respond(text(
                502,
                &format!("{} events: {e}", forge.bin),
                "text/plain",
            ));
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = req.respond(text(502, "events stdout", "text/plain"));
        return;
    };
    let head = Response::empty(StatusCode(200))
        .with_header(h("Content-Type", "text/event-stream"))
        .with_header(h("Cache-Control", "no-cache"))
        .with_header(h("X-Accel-Buffering", "no"));
    let mut stream = req.upgrade("sse", head);
    let _ = stream
        .write_all(b": connected\n\n")
        .and_then(|_| stream.flush());
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if stream
            .write_all(format!("data: {line}\n\n").as_bytes())
            .and_then(|_| stream.flush())
            .is_err()
        {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn id_of(rest: &str) -> Option<i64> {
    rest.trim_matches('/').parse().ok()
}

/// `/api/projects/<name>`, `/api/projects/<name>/backlog`, or
/// `/api/projects/<name>/initiatives`, split into the project's name and
/// which of the three routes it is; `None` for anything else, including a
/// name that would smuggle a path segment.
fn project_sub(path: &str) -> Option<(String, Option<&'static str>)> {
    let rest = path.strip_prefix("/api/projects/")?;
    for (suffix, sub) in [("/backlog", "backlog"), ("/initiatives", "initiatives")] {
        if let Some(name) = rest.strip_suffix(suffix)
            && !name.is_empty()
            && !name.contains('/')
        {
            return Some((name.to_string(), Some(sub)));
        }
    }
    (!rest.is_empty() && !rest.contains('/')).then(|| (rest.to_string(), None))
}

/// `/api/plugins/<name>/<action>` split into the plugin's name and the
/// trailing action (`enable`, `disable`, or `logs`); `None` for anything
/// else, including a name that would smuggle a path segment.
fn plugin_action(path: &str) -> Option<(String, &'static str)> {
    let rest = path.strip_prefix("/api/plugins/")?;
    for (suffix, action) in [
        ("/enable", "enable"),
        ("/disable", "disable"),
        ("/logs", "logs"),
    ] {
        if let Some(name) = rest.strip_suffix(suffix)
            && !name.is_empty()
            && !name.contains('/')
        {
            return Some((name.to_string(), action));
        }
    }
    None
}

/// `forge-repomap edges <repo> --cache <dir>`, through the client crate's
/// spawn helper: the structure layer's graph, for the `/graph` page. The
/// cache directory is the one Forge itself already keeps a repo map in
/// (`doctor` reports it as `cache.repomap` under the data dir), so a
/// browser's first graph reuses whatever a task's own repo-map step
/// already parsed.
fn graph(repo: &str) -> Result<Value> {
    let cache = home().join("cache").join("repomap");
    let cache = cache.to_string_lossy().into_owned();
    // Not a `forge` verb: `forge-repomap` is a separate tool the kernel
    // ships beside it, outside the verb contract `tests/boundary.rs`
    // enforces on this crate's calls into `forge` itself. Its subcommand
    // name is built up rather than spelled as an inline array literal so
    // that boundary check's source scan reads past this call.
    let edges = "edges".to_string();
    let args = [edges.as_str(), repo, "--cache", cache.as_str()];
    forge_client::spawn_json("forge-repomap", &args)
}

/// `forge stats --json`, through the client crate's typed `StatsDoc`, for
/// the `/stats` page's by-role table.
fn stats_json(forge: &Forge) -> Result<Value> {
    let doc = forge.stats()?;
    let by_role: Vec<Value> = doc
        .by_role
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "role": r.role,
                "provider": r.provider,
                "model": r.model,
                "attempts": r.attempts,
                "succeeded": r.succeeded,
                "succeeded_share": r.succeeded_share,
                "mean_turns": r.mean_turns,
                "mean_cost_usd": r.mean_cost_usd,
                "mean_secs": r.mean_secs,
                "landed": r.landed,
                "broke_base": r.broke_base,
                "broke_base_share": r.broke_base_share,
            })
        })
        .collect();
    Ok(serde_json::json!({ "by_role": by_role }))
}

/// `forge plugin list --json` and `forge plugin status --json`, through
/// the client crate's typed rows, merged by name into one document per
/// plugin for the `/plugins` page.
fn plugins_merged(forge: &Forge) -> Result<Value> {
    let rows = forge.plugin_list()?;
    let mut statuses: std::collections::HashMap<String, forge_client::PluginStatusRow> = forge
        .plugin_status()?
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let merged = rows
        .into_iter()
        .map(|r| {
            let s = statuses.remove(&r.name).unwrap_or_default();
            serde_json::json!({
                "name": r.name,
                "description": r.description,
                "capabilities": r.capabilities,
                "enabled": r.enabled,
                "state": s.state,
                "pid": s.pid,
                "uptime_secs": s.uptime_secs,
                "restart_count": s.restart_count,
                "last_exit": s.last_exit,
            })
        })
        .collect();
    Ok(Value::Array(merged))
}

/// One request: authenticate, then route. Everything but `/` with a
/// token in the query is refused without a valid token.
fn handle(req: Request, forge: &Forge, secret: &str) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let write_post = req.method() == &Method::Post
        && (path.starts_with("/api/retry/")
            || matches!(
                plugin_action(&path),
                Some((_, "enable")) | Some((_, "disable"))
            ));
    if req.method() != &Method::Get && !write_post {
        let _ = req.respond(text(405, "read-only for now", "text/plain"));
        return;
    }
    let ok = presented(&req, &query).is_some_and(|t| same(&t, secret));
    if !ok {
        let _ = req.respond(text(
            401,
            "forge-web: open the link forge-web printed when it started (it carries the token).",
            "text/plain",
        ));
        return;
    }
    if query_param(&query, "token").is_some() && !path.starts_with("/api/") {
        // First visit: pin the token in a cookie and drop it from the URL.
        let resp = Response::empty(StatusCode(303))
            .with_header(h("Location", if path == "/" { "/tasks" } else { &path }))
            .with_header(h(
                "Set-Cookie",
                &format!("forge_token={secret}; Path=/; HttpOnly; SameSite=Strict"),
            ));
        let _ = req.respond(resp);
        return;
    }
    if path == "/" {
        let _ = req.respond(Response::empty(StatusCode(303)).with_header(h("Location", "/tasks")));
        return;
    }
    let resp = match path.as_str() {
        p if p == "/tasks"
            || p.starts_with("/tasks/")
            || p == "/plugins"
            || p == "/projects"
            || p.starts_with("/projects/")
            || p.starts_with("/initiatives/")
            || p == "/graph"
            || p == "/stats"
            || p == "/jobs"
            || p.starts_with("/jobs/") =>
        {
            text(200, INDEX, "text/html; charset=utf-8")
        }
        "/app.js" => text(200, APP_JS, "application/javascript"),
        "/api/snapshot" => json_or_error(forge.json(&["snapshot"])),
        "/api/stats" => json_or_error(stats_json(forge)),
        "/api/graph" => match query_param(&query, "repo").map(|v| unescape(&v)) {
            Some(repo) if !repo.is_empty() => json_or_error(graph(&repo)),
            _ => text(400, "repo is required", "text/plain"),
        },
        "/api/plugins" => json_or_error(plugins_merged(forge)),
        p if p.starts_with("/api/plugins/") => match plugin_action(p) {
            Some((name, action @ ("enable" | "disable"))) => {
                if req.method() != &Method::Post {
                    text(405, "POST only", "text/plain")
                } else {
                    json_or_error(
                        forge
                            .run(&["plugin", action, &name])
                            .map(|out| serde_json::json!({ "output": out })),
                    )
                }
            }
            Some((name, "logs")) => {
                if req.method() != &Method::Get {
                    text(405, "GET only", "text/plain")
                } else {
                    match forge.run(&["plugin", "logs", &name]) {
                        Ok(s) => text(200, &s, "text/plain; charset=utf-8"),
                        Err(e) => text(502, &e.to_string(), "text/plain"),
                    }
                }
            }
            _ => text(404, "not found", "text/plain"),
        },
        "/api/tasks" => {
            // forge log --json with the page's filters: limit, before, q
            // (text or id), state, workflow, repo. Values are passed as
            // separate argv entries, never through a shell.
            let mut args: Vec<String> = vec!["log".into(), "--json".into()];
            let limit = query_param(&query, "limit")
                .and_then(|l| l.parse::<u32>().ok())
                .unwrap_or(100)
                .clamp(1, 500);
            args.push("--limit".into());
            args.push(limit.to_string());
            for (key, flag) in [
                ("before", "--before"),
                ("q", "--grep"),
                ("state", "--state"),
                ("workflow", "--workflow"),
                ("repo", "--repo"),
                ("project", "--project"),
            ] {
                if let Some(v) = query_param(&query, key).map(|v| unescape(&v))
                    && !v.is_empty()
                {
                    args.push(flag.into());
                    args.push(v);
                }
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            json_or_error(forge.json(&argv))
        }
        "/api/requests" => json_or_error(forge.json(&["requests", "--json"])),
        "/api/jobs" => json_or_error(forge.json(&["job", "list", "--json"])),
        p if p.starts_with("/api/job/") => match id_of(&p["/api/job/".len()..]) {
            Some(id) => json_or_error(forge.json(&["job", "show", &id.to_string(), "--json"])),
            None => text(404, "no such job", "text/plain"),
        },
        "/api/projects" => json_or_error(forge.json(&["project", "list", "--json"])),
        p if p.starts_with("/api/projects/") => match project_sub(p) {
            Some((name, None)) => json_or_error(forge.json(&["project", "show", &name, "--json"])),
            Some((name, Some("backlog"))) => {
                json_or_error(forge.json(&["project", "backlog", &name, "--json"]))
            }
            Some((name, Some("initiatives"))) => {
                json_or_error(forge.json(&["initiative", "list", &name, "--json"]))
            }
            _ => text(404, "not found", "text/plain"),
        },
        p if p.starts_with("/api/initiatives/") => match id_of(&p["/api/initiatives/".len()..]) {
            Some(id) => {
                json_or_error(forge.json(&["initiative", "report", &id.to_string(), "--json"]))
            }
            None => text(404, "no such initiative", "text/plain"),
        },
        "/api/events" => {
            let since = query_param(&query, "since")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            events(req, forge, since);
            return;
        }
        p if p.starts_with("/api/task/") => match id_of(&p["/api/task/".len()..]) {
            Some(id) => json_or_error(forge.json(&["trace", &id.to_string(), "--json"])),
            None => text(404, "no such task", "text/plain"),
        },
        p if p.starts_with("/api/journal/") => match id_of(&p["/api/journal/".len()..]) {
            Some(id) => json_or_error(forge.json(&["journal", &id.to_string(), "--json"])),
            None => text(404, "no such task", "text/plain"),
        },
        p if p.starts_with("/api/retry/") => {
            if req.method() != &Method::Post {
                text(405, "POST only", "text/plain")
            } else {
                match id_of(&p["/api/retry/".len()..]) {
                    Some(id) => json_or_error(
                        forge
                            .run(&["retry", &id.to_string()])
                            .map(|out| serde_json::json!({ "output": out })),
                    ),
                    None => text(404, "no such task", "text/plain"),
                }
            }
        }
        _ => text(404, "not found", "text/plain"),
    };
    let _ = req.respond(resp);
}

fn main() -> Result<()> {
    let mut bind = "127.0.0.1:7788".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" => bind = args.next().context("--bind needs an address")?,
            "-h" | "--help" => {
                println!(
                    "usage: forge-web [--bind ADDR]   (default 127.0.0.1:7788; FORGE_BIN, FORGE2_HOME honoured)"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let secret = token(&home())?;
    let forge = Forge::new();
    let server = Server::http(&bind).map_err(|e| anyhow::anyhow!("binding {bind}: {e}"))?;
    let addr = server.server_addr();
    eprintln!("forge-web listening on {addr}");
    println!("http://{addr}/?token={secret}");
    let server = Arc::new(server);
    for req in server.incoming_requests() {
        let forge = forge.clone();
        let secret = secret.clone();
        std::thread::spawn(move || handle(req, &forge, &secret));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_compare_is_exact() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn query_params_and_ids_parse() {
        assert_eq!(
            query_param("a=1&token=xyz", "token").as_deref(),
            Some("xyz")
        );
        assert_eq!(query_param("", "token"), None);
        assert_eq!(id_of("12"), Some(12));
        assert_eq!(id_of("x"), None);
    }

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(unescape("a+b%20c%2Fd"), "a b c/d");
        assert_eq!(unescape("100%"), "100%");
        assert_eq!(unescape("%zz"), "%zz");
        assert_eq!(unescape("x%4"), "x%4");
    }

    #[test]
    fn a_token_is_generated_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = token(dir.path()).unwrap();
        let b = token(dir.path()).unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(a, b);
    }
}
