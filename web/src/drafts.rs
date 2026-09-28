//! The draft editor's routes (`/api/drafts/...`, docs/CLIENT.md): each is
//! one `forge workflows draft` verb. The draft is a JSON document the page
//! holds and sends whole — a step list with edges and placeholders — and
//! every answer is that document annotated with the file it renders to and
//! the catalog linter's problems, so the editor lints on every change with
//! the linter `forge workflows lint` runs and keeps no logic of its own.

use super::*;

type Reply = Response<std::io::Cursor<Vec<u8>>>;

fn error(status: u16, message: &str) -> Reply {
    text(
        status,
        &serde_json::json!({ "error": message }).to_string(),
        "application/json",
    )
}

fn answer(r: Result<Value>, status_on_error: u16) -> Reply {
    match r {
        Ok(v) => text(200, &v.to_string(), "application/json"),
        Err(e) => error(status_on_error, &e.to_string()),
    }
}

/// The body's `draft` (or, for a verb with no other field, the whole body),
/// as the text the verb reads on stdin.
fn draft_of(v: &Value) -> String {
    v.get("draft").unwrap_or(v).to_string()
}

/// A draft's name, the one thing the web layer checks itself: it becomes
/// an argument and a file name.
fn named(v: &Value) -> Option<&str> {
    v.get("draft")
        .unwrap_or(v)
        .get("name")?
        .as_str()
        .filter(|n| !n.is_empty() && !n.starts_with('-'))
}

fn post(req: &mut Request, forge: &Forge, verb: &str, name: Option<&str>) -> Reply {
    let raw = match read_body(req, WORKFLOW_BODY_LIMIT) {
        Ok(t) => t,
        Err(e) => return error(400, &e.to_string()),
    };
    if verb == "import" {
        return answer(
            forge.json_with_stdin(&["workflows", "draft", "import"], &raw),
            422,
        );
    }
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return error(400, &format!("bad JSON body: {e}")),
    };
    let doc = draft_of(&v);
    match verb {
        "check" | "save" => answer(
            forge.json_with_stdin(&["workflows", "draft", verb], &doc),
            422,
        ),
        "put" => put(forge, &v, &doc, name),
        _ => error(404, "not found"),
    }
}

/// `POST /api/drafts/<name>/put`: `{draft, message, to_repo}`. `to_repo`
/// files a repository task on the draft's project (as `forge workflows put
/// --repo`) instead of writing the catalog. A draft with placeholders is
/// saved incomplete and files its build tasks.
fn put(forge: &Forge, v: &Value, doc: &str, name: Option<&str>) -> Reply {
    let Some(name) = name.or_else(|| named(v)) else {
        return error(422, "the draft needs a name");
    };
    let message = v["message"].as_str().unwrap_or_default();
    let mut args = vec!["workflows", "draft", "put", name, "--message", message];
    let repo;
    if v["to_repo"].as_bool() == Some(true) {
        let project = v["draft"]["project"].as_str().unwrap_or_default();
        repo = match project_first_repo(forge, project) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return error(
                    422,
                    &format!("project {project:?} has no registered repository"),
                );
            }
            Err(e) => return error(502, &e.to_string()),
        };
        args.extend(["--repo", &repo]);
    }
    answer(forge.json_with_stdin(&args, doc), 422)
}

/// Route one `/api/drafts...` request and respond to it.
pub(crate) fn route(mut req: Request, forge: &Forge, path: &str) {
    let rest = path
        .strip_prefix("/api/drafts")
        .unwrap_or("")
        .trim_matches('/');
    let get = req.method() == &Method::Get;
    let resp = match (rest, get) {
        ("", true) => answer(forge.json(&["workflows", "draft", "list", "--json"]), 502),
        ("actions", true) => answer(
            forge.json(&["workflows", "draft", "actions", "--json"]),
            502,
        ),
        ("check" | "save" | "import", false) => post(&mut req, forge, rest, None),
        (name, true) if !name.contains('/') && !name.starts_with('-') => answer(
            forge.json(&["workflows", "draft", "show", &unescape(name), "--json"]),
            502,
        ),
        (name, false) => match name.strip_suffix("/put") {
            Some(n) if !n.contains('/') && !n.starts_with('-') => {
                post(&mut req, forge, "put", Some(&unescape(n)))
            }
            _ => error(404, "not found"),
        },
        _ => error(405, "not a draft route"),
    };
    let _ = req.respond(resp);
}
