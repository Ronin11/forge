//! The Chat page's routes (`/api/chat...`, docs/CLIENT.md, "Ask Forge"):
//! each is one `forge chat` verb. A message is `forge chat --stream`, its
//! JSON lines relayed as server-sent events while the model works; the
//! sessions and one session's turns are `chat sessions` and `chat show`;
//! confirming or rejecting a proposed action is `chat confirm` and `chat
//! reject`. The web layer keeps no conversation and runs no tool of its
//! own.

use super::*;

type Reply = Response<std::io::Cursor<Vec<u8>>>;

/// The longest message the page may send.
const MESSAGE_LIMIT: usize = 8000;

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

/// An action id is a turn and a position: digits, a dot, digits.
fn action_id(s: &str) -> Option<&str> {
    let (turn, index) = s.split_once('.')?;
    let digits = |d: &str| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit());
    (digits(turn) && digits(index)).then_some(s)
}

/// `POST /api/chat/message`: `{message, session?, provider?}`. The answer
/// is a `text/event-stream` of the events `forge chat --stream` prints,
/// each as one `data:` line; it ends when the turn does. A browser that
/// goes away does not abort the turn — its reply and cost are recorded
/// like any other — so the stream is read to the end either way.
fn message(mut req: Request, forge: &Forge) {
    let raw = match read_body(&mut req, 64 * 1024) {
        Ok(t) => t,
        Err(e) => return respond(req, error(400, &e.to_string())),
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return respond(req, error(400, &format!("bad JSON body: {e}"))),
    };
    let message = v["message"].as_str().unwrap_or_default().trim().to_string();
    if message.is_empty() || message.chars().count() > MESSAGE_LIMIT {
        return respond(
            req,
            error(
                422,
                &format!("a message is 1 to {MESSAGE_LIMIT} characters"),
            ),
        );
    }
    let mut args: Vec<String> = vec!["chat".into(), "--stream".into()];
    if let Some(id) = v["session"].as_i64() {
        args.extend(["--session".into(), id.to_string()]);
    }
    if let Some(p) = v["provider"]
        .as_str()
        .filter(|p| !p.is_empty() && !p.starts_with('-'))
    {
        args.extend(["--provider".into(), p.to_string()]);
    }
    // After `--` the message is never read as an option.
    args.extend(["--".into(), message]);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut lines = match forge.follow(&argv) {
        Ok(l) => l,
        Err(e) => return respond(req, error(502, &e.to_string())),
    };
    let head = Response::empty(StatusCode(200))
        .with_header(h("Content-Type", "text/event-stream"))
        .with_header(h("Cache-Control", "no-cache"))
        .with_header(h("X-Accel-Buffering", "no"));
    let mut stream = req.upgrade("sse", head);
    let mut open = true;
    while let Some(Ok(line)) = lines.next_line() {
        if open
            && stream
                .write_all(format!("data: {line}\n\n").as_bytes())
                .and_then(|_| stream.flush())
                .is_err()
        {
            open = false;
        }
    }
}

fn respond(req: Request, resp: Reply) {
    let _ = req.respond(resp);
}

/// Route one `/api/chat...` request and respond to it.
pub(crate) fn route(req: Request, forge: &Forge, path: &str) {
    let rest = path
        .strip_prefix("/api/chat")
        .unwrap_or("")
        .trim_matches('/');
    let get = req.method() == &Method::Get;
    let resp = match (rest, get) {
        ("", true) => answer(forge.json(&["chat", "sessions", "--json"]), 502),
        ("message", false) => return message(req, forge),
        (id, true) if id.parse::<i64>().is_ok() => {
            answer(forge.json(&["chat", "show", id, "--json"]), 404)
        }
        (r, false) => match r.split_once('/') {
            Some((verb @ ("confirm" | "reject"), a)) if action_id(a).is_some() => {
                answer(forge.json(&["chat", verb, a, "--json"]), 422)
            }
            _ => error(404, "not found"),
        },
        _ => error(405, "not a chat route"),
    };
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_turn_and_a_position_is_an_action_id() {
        assert_eq!(action_id("12.0"), Some("12.0"));
        for bad in [
            "", "12", "12.", ".0", "a.b", "12.0.1", "-1.0", "1.0 --x", "1.0;",
        ] {
            assert_eq!(action_id(bad), None, "{bad:?}");
        }
    }
}
