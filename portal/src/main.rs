//! forge-portal: the customer portal (see docs/PORTAL.md), a page at
//! `/p/<token>` showing one project's work in plain words. Like
//! `forge-web` it never touches the kernel: every read is a `forge`
//! verb's JSON and every write is a `forge` verb's exit status. `GET
//! /p/<token>` resolves the token to a project through `forge project
//! resolve-token`, then renders `forge project view --json` as six
//! sections: Running for you, Being built, Needs you, Done, Ask, Your
//! plan. An unknown or revoked token is a plain 404 page, with no hint
//! of why. `GET /p/<token>/shot/<target>` streams that deploy target's
//! last-look screenshot file.
//!
//! Ask is a conversation, not a box on its own: every message on the
//! project's record (`forge message list --json`, both directions) and
//! every concierge reply (`forge decisions --json`, narrowed to the
//! rows addressed to this portal's own contact) render as one thread,
//! oldest first, with any question still open at the point it was
//! asked rendered inline as the same answerable form "Needs you"
//! carries (see `render_conversation`).
//!
//! `POST /p/<token>/answer` (`id`, `text`) runs `forge answer <id> <text>
//! --by customer`, re-queuing the blocked task; `POST /p/<token>/ask`
//! (`message`) runs `forge ask <project> <message> --from customer` and
//! appends the exchange to the same thread. Both are token-scoped (the
//! same 404 an unknown token gets elsewhere) and rate-limited to ten
//! writes a minute per token; over that, and any failure from `forge`
//! itself, is the same fixed error page, so nothing about why leaks.

use anyhow::{Context, Result};
use forge_client::{
    Forge, PortalBacklogItem, PortalBrief, PortalDeployTarget, PortalDoc, PortalInitiative,
    PortalJobRun, PortalLanded, PortalQuestion, PortalWorkflow,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// The identity the portal writes under: there is one link per project
/// and no accounts yet (see docs/PORTAL.md, "What it is"), so every
/// answer and every ask comes from the same named contact.
const CONTACT: &str = "customer";

/// Per-token write rate limit: ten writes a minute (see docs/PORTAL.md).
const RATE_LIMIT: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Tracks write timestamps per token, in memory, for the lifetime of the
/// server process. A token that outgrows this needs a real account
/// system, which is a later build-order step.
#[derive(Default)]
struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    /// Records one write attempt for `token` and reports whether it is
    /// within the limit: prunes hits older than the window, then allows
    /// the write only if fewer than `RATE_LIMIT` remain.
    fn allow(&self, token: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().expect("rate limiter mutex poisoned");
        let entry = hits.entry(token.to_string()).or_default();
        entry.retain(|&t| now.duration_since(t) < RATE_WINDOW);
        if entry.len() >= RATE_LIMIT {
            return false;
        }
        entry.push(now);
        true
    }
}

const STYLE: &str = include_str!("style.css");
/// Rewrites every `<time data-ts>` into the viewer's own zone (see
/// `time_tag`); the page carries it inline so it is one request.
const LOCAL_TIME_JS: &str = include_str!("local-time.js");

fn h(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("static header")
}

fn html(status: u16, body: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(StatusCode(status))
        .with_header(h("Content-Type", "text/html; charset=utf-8"))
        .with_header(h("Cache-Control", "no-store"))
}

/// Escapes text for HTML: every field a project's own people wrote
/// (purpose, outcomes, backlog text, ...) lands in the page verbatim
/// otherwise.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Unix days since the epoch to a proleptic Gregorian (year, month, day),
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// A moment as the server's fallback text: "Nov 14, 2023, 22:13 UTC".
/// `local-time.js` renders the same shape in the viewer's zone.
fn utc_text(ts: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = civil_from_days(ts.div_euclid(86400));
    let secs = ts.rem_euclid(86400);
    format!(
        "{} {d}, {y}, {:02}:{:02} UTC",
        MONTHS[(m - 1) as usize],
        secs / 3600,
        secs % 3600 / 60
    )
}

/// The one way the page shows a moment: a `<time>` carrying the Unix
/// seconds in `data-ts` for the script to render in the viewer's zone,
/// and the UTC text inside for a viewer without JavaScript. `prefix` is
/// static wording ("Shipped ") that stays in front of the moment either
/// way.
fn time_tag(prefix: &str, ts: i64) -> String {
    let prefix_attr = if prefix.is_empty() {
        String::new()
    } else {
        format!(r#" data-prefix="{}""#, esc(prefix))
    };
    format!(
        r#"<time data-ts="{ts}"{prefix_attr}>{}{}</time>"#,
        esc(prefix),
        utc_text(ts)
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>{STYLE}</style>
</head>
<body>
{body}
<script>{LOCAL_TIME_JS}</script>
</body>
</html>
"#,
        esc(title)
    )
}

fn not_found() -> Response<Cursor<Vec<u8>>> {
    html(
        404,
        &page(
            "Not found",
            r#"<div class="notfound"><h1>This link doesn't work anymore.</h1><p>Check the link you were sent, or ask for a fresh one.</p></div>"#,
        ),
    )
}

/// The one error page every write failure shows, whatever the cause
/// (rate limit, bad input, `forge` itself failing): fixed wording so
/// nothing about why leaks (see docs/PORTAL.md).
fn write_error(status: u16) -> Response<Cursor<Vec<u8>>> {
    html(
        status,
        &page(
            "Couldn't send that",
            r#"<div class="notfound"><h1>That didn't go through.</h1><p>Wait a moment, then try again.</p></div>"#,
        ),
    )
}

/// Percent-decodes one `application/x-www-form-urlencoded` value (plus
/// as space).
fn urldecode(v: &str) -> String {
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

/// One value out of a `application/x-www-form-urlencoded` body.
fn form_value(body: &str, key: &str) -> Option<String> {
    body.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| urldecode(v))
}

/// The request body, read to completion (tiny_http's reader is already
/// bounded by `Content-Length`).
fn read_body(req: &mut Request) -> String {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    body
}

/// The class and plain-word phrase for one deploy target's current state:
/// bad beats a bad look, a bad look beats "never checked", "never
/// checked" beats "never deployed".
fn deploy_status(t: &PortalDeployTarget) -> (&'static str, &'static str) {
    if t.last_deployed_at.is_none() {
        return ("", "Hasn't shipped yet.");
    }
    match (t.check_ok, t.look_ok) {
        (Some(false), _) => ("bad", "Something's wrong right now \u{2014} we're on it."),
        (_, Some(false)) => ("warn", "It's up, but something looks off."),
        (Some(true), Some(true)) => ("ok", "Up and running, and looking right."),
        (Some(true), _) => ("ok", "Up and running."),
        (None, _) => ("", "Deployed; we haven't checked it yet."),
    }
}

fn render_targets(targets: &[PortalDeployTarget], token: &str) -> String {
    let mut out = String::new();
    for t in targets {
        let (class, phrase) = deploy_status(t);
        let when = match t.last_deployed_at {
            Some(ts) => time_tag("Last updated ", ts),
            None => String::new(),
        };
        let img = if t.screenshot.is_some() {
            format!(
                r#"<img src="/p/{}/shot/{}" alt="the last look at {}">"#,
                esc(token),
                esc(&t.name),
                esc(&t.name)
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            r#"<div class="card"><div class="name">{name}</div><div class="where">{where_it_runs}</div><div class="status {class}">{phrase}</div><div class="when">{when}</div>{img}</div>"#,
            name = esc(&t.name),
            where_it_runs = esc(&t.where_it_runs),
        ));
    }
    out
}

/// The class and plain-word phrase for one job run: bad for a failure,
/// warn for one that needs a person, ok for a clean run or a skip (a
/// `[skip_if]` deciding there was nothing to do is not a failure), plain
/// for anything still in flight. A failure, a needs-you, or a skip carries
/// its one-line reason, when there is one.
fn job_run_status(j: &PortalJobRun) -> (&'static str, String) {
    match j.state.as_str() {
        "ok" => ("ok", "Ran fine.".to_string()),
        "skipped" => (
            "ok",
            match &j.reason {
                Some(r) => format!("Skipped \u{2014} {r}."),
                None => "Skipped.".to_string(),
            },
        ),
        "failed" => (
            "bad",
            match &j.reason {
                Some(r) => format!("Failed \u{2014} {r}."),
                None => "Failed.".to_string(),
            },
        ),
        "needs_human" => (
            "warn",
            match &j.reason {
                Some(r) => format!("Needs you \u{2014} {r}."),
                None => "Needs you.".to_string(),
            },
        ),
        "running" => ("", "Running now.".to_string()),
        "scheduled" => ("", "Scheduled.".to_string()),
        "dropped" => ("", "Dropped.".to_string()),
        _ => ("", "Queued.".to_string()),
    }
}

fn render_run_workflows(workflows: &[PortalWorkflow]) -> String {
    let mut out = String::new();
    for w in workflows {
        out.push_str(&format!(
            r#"<div class="card"><div class="name">{}</div><ul class="plain">"#,
            esc(&w.name)
        ));
        for j in &w.jobs {
            let (class, phrase) = job_run_status(j);
            out.push_str(&format!(
                r#"<li><div class="status {class}">{phrase}</div><div class="date">{when}</div></li>"#,
                phrase = esc(&phrase),
                when = time_tag("", j.started_at),
            ));
        }
        out.push_str("</ul></div>");
    }
    out
}

/// "1 piece of work" / "3 pieces of work" beside an initiative's outcome.
fn pieces_phrase(n: i64) -> String {
    if n == 1 {
        "1 piece of work".to_string()
    } else {
        format!("{n} pieces of work")
    }
}

/// "and n more" for a list `PortalDoc` already capped at ten; empty when
/// nothing was cut.
fn render_more(more: i64) -> String {
    if more <= 0 {
        return String::new();
    }
    format!(r#"<li class="more">and {more} more</li>"#)
}

fn render_initiatives(items: &[PortalInitiative], more: i64) -> String {
    if items.is_empty() {
        return r#"<p class="empty">Nothing being built right now.</p>"#.to_string();
    }
    let mut out = String::from(r#"<ul class="plain">"#);
    for i in items {
        out.push_str(&format!(
            r#"<li><div>{outcome}</div><div class="pieces">{pieces}</div><div class="date">{state}</div></li>"#,
            outcome = esc(&i.outcome),
            pieces = esc(&pieces_phrase(i.pieces)),
            state = esc(&cap_first(&i.state)),
        ));
    }
    out.push_str(&render_more(more));
    out.push_str("</ul>");
    out
}

fn render_landed(items: &[PortalLanded], more: i64) -> String {
    if items.is_empty() {
        return r#"<p class="empty">Nothing has shipped yet.</p>"#.to_string();
    }
    let mut out = String::from(r#"<ul class="plain">"#);
    for l in items {
        let pieces = match l.pieces {
            Some(n) => format!(r#"<div class="pieces">{}</div>"#, esc(&pieces_phrase(n))),
            None => String::new(),
        };
        out.push_str(&format!(
            r#"<li><div>{text}</div>{pieces}<div class="date">{date}</div></li>"#,
            text = esc(&l.text),
            date = time_tag("Shipped ", l.landed_at),
        ));
    }
    out.push_str(&render_more(more));
    out.push_str("</ul>");
    out
}

fn render_questions(items: &[PortalQuestion], token: &str) -> String {
    if items.is_empty() {
        return r#"<p class="empty">Nothing needs you right now.</p>"#.to_string();
    }
    let mut out = String::new();
    for q in items {
        let asked = match q.asked_at {
            Some(ts) => format!(r#"<p class="date">{}</p>"#, time_tag("Asked ", ts)),
            None => String::new(),
        };
        out.push_str(&format!(
            r#"<form class="ask" method="post" action="/p/{token}/answer"><p>{text}</p>{asked}<input type="hidden" name="id" value="{id}"><input type="text" name="text" placeholder="Your answer" required><button type="submit">Send</button></form>"#,
            token = esc(token),
            text = esc(&q.text),
            id = q.task_id,
        ));
    }
    out
}

/// One row of `forge message list --json`, trimmed to what the
/// conversation thread renders. The portal keeps its own copy of this
/// shape (see docs/CLIENT.md, `MessageRow`) rather than growing
/// `forge-client`, whose `PortalDoc` carries no messages of its own.
#[derive(Deserialize, Default)]
struct ConvMessage {
    #[serde(default)]
    direction: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    at: i64,
}

/// One row of `forge decisions --json`, trimmed the same way: a reply
/// the concierge (or an operator, answering in its place) gave to a
/// question the customer asked. `answered_for` is who the question was
/// addressed to; only a row addressed to this portal's own contact
/// (`CONTACT`) belongs in the customer's own thread.
#[derive(Deserialize, Default)]
struct ConvReply {
    #[serde(default)]
    question: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    answered_for: Option<String>,
}

/// `forge message list <project> --json`, both directions: the customer's
/// side of every channel exchange on record for this project.
fn conversation_messages(forge: &Forge, project: &str) -> Result<Vec<ConvMessage>> {
    let v = forge.json(&["message", "list", project, "--json"])?;
    Ok(serde_json::from_value(v)?)
}

/// `forge decisions --project <project> --json`, narrowed to the rows
/// addressed to this portal's own contact: every concierge (or operator)
/// reply that belongs in the customer's own thread, not an internal
/// decision on some other task of theirs.
fn conversation_replies(forge: &Forge, project: &str) -> Result<Vec<ConvReply>> {
    let v = forge.json(&["decisions", "--project", project, "--json"])?;
    let rows: Vec<ConvReply> = serde_json::from_value(v)?;
    Ok(rows
        .into_iter()
        .filter(|d| d.answered_for.as_deref() == Some(CONTACT))
        .collect())
}

/// The conversation thread: every message on the record (both
/// directions), every concierge reply, and every open question still
/// addressed to the customer, merged in the order they happened. A
/// question renders as the same answerable form "Needs you" carries, so
/// answering it in either place settles the same task.
fn render_conversation(
    doc: &PortalDoc,
    token: &str,
    messages: &[ConvMessage],
    replies: &[ConvReply],
) -> String {
    let mut entries: Vec<(i64, String)> = Vec::new();
    for m in messages {
        let class = if m.direction == "in" { "in" } else { "out" };
        entries.push((
            m.at,
            format!(
                r#"<div class="msg {class}"><p>{}</p><p class="date">{}</p></div>"#,
                esc(&m.text),
                time_tag("", m.at),
            ),
        ));
    }
    for d in replies {
        entries.push((
            d.created_at,
            format!(
                r#"<div class="msg in"><p>{}</p></div><div class="msg out"><p>{}</p><p class="date">{}</p></div>"#,
                esc(&d.question),
                esc(&d.answer),
                time_tag("", d.created_at),
            ),
        ));
    }
    for q in &doc.questions {
        let at = q.asked_at.unwrap_or(0);
        entries.push((
            at,
            format!(
                r#"<form class="ask msg question" method="post" action="/p/{token}/answer"><p>{text}</p><p class="date">{when}</p><input type="hidden" name="id" value="{id}"><input type="text" name="text" placeholder="Your answer" required><button type="submit">Send</button></form>"#,
                token = esc(token),
                text = esc(&q.text),
                when = time_tag("", at),
                id = q.task_id,
            ),
        ));
    }
    entries.sort_by_key(|(at, _)| *at);
    if entries.is_empty() {
        return r#"<p class="empty">No conversation yet.</p>"#.to_string();
    }
    entries.into_iter().map(|(_, html)| html).collect()
}

/// The conversation thread, then the Ask box at its foot: what the
/// customer types posts into this same thread (`/p/<token>/ask`), and
/// `pending`, when set (the message just submitted and the reply it
/// got back), is appended after the persisted thread so the exchange
/// shows immediately, before the next read of the record would carry it
/// forward on its own.
fn render_ask(
    doc: &PortalDoc,
    token: &str,
    messages: &[ConvMessage],
    replies: &[ConvReply],
    pending: Option<(&str, &str)>,
) -> String {
    let thread = render_conversation(doc, token, messages, replies);
    let banner = match pending {
        Some((message, reply)) => format!(
            r#"<div class="msg in"><p>{}</p></div><div class="msg out"><p>{}</p></div>"#,
            esc(message),
            esc(reply),
        ),
        None => String::new(),
    };
    format!(
        r#"{thread}{banner}<form class="ask" method="post" action="/p/{token}/ask"><textarea name="message" placeholder="Ask us anything" required></textarea><button type="submit">Send</button></form>"#,
        token = esc(token),
    )
}

fn render_plan(brief: &Option<PortalBrief>, backlog: &[PortalBacklogItem]) -> String {
    let mut out = String::new();
    match brief {
        Some(b) => {
            out.push_str(&format!(r#"<p>It runs on {}.</p>"#, esc(&b.where_it_runs)));
            if !b.workflows.is_empty() {
                out.push_str(r#"<ul class="plain">"#);
                for w in &b.workflows {
                    out.push_str(&format!("<li>{}</li>", esc(w)));
                }
                out.push_str("</ul>");
            }
        }
        None => out.push_str(r#"<p class="empty">No plan on file yet.</p>"#),
    }
    if !backlog.is_empty() {
        out.push_str(r#"<ul class="plain">"#);
        for b in backlog {
            out.push_str(&format!("<li>{}</li>", esc(&b.text)));
        }
        out.push_str("</ul>");
    }
    out
}

/// "Running for you": deploy targets and run workflows together, since
/// either alone is what is running for the customer (see docs/PORTAL.md).
/// Empty only when both are.
fn render_running(
    targets: &[PortalDeployTarget],
    workflows: &[PortalWorkflow],
    token: &str,
) -> String {
    if targets.is_empty() && workflows.is_empty() {
        return r#"<p class="empty">Nothing runs for you yet.</p>"#.to_string();
    }
    format!(
        "{}{}",
        render_targets(targets, token),
        render_run_workflows(workflows)
    )
}

fn render_page(
    doc: &PortalDoc,
    token: &str,
    messages: &[ConvMessage],
    replies: &[ConvReply],
    pending: Option<(&str, &str)>,
) -> String {
    // No purpose paragraph: a project's purpose is the operator's own
    // words, never the customer's (see docs/PORTAL.md).
    let header = format!(r#"<header><h1>{}</h1></header>"#, esc(&doc.project));
    let main = format!(
        r#"<main>
<section><h2>Running for you</h2>{running}</section>
<section><h2>Being built</h2>{initiatives}</section>
<section><h2>Needs you</h2>{questions}</section>
<section><h2>Done</h2>{landed}</section>
<section><h2>Ask</h2>{ask}</section>
<section><h2>Your plan</h2>{plan}</section>
</main>"#,
        running = render_running(&doc.deploy_targets, &doc.run_workflows, token),
        initiatives = render_initiatives(&doc.initiatives, doc.initiatives_more),
        questions = render_questions(&doc.questions, token),
        landed = render_landed(&doc.landed, doc.landed_more),
        ask = render_ask(doc, token, messages, replies, pending),
        plan = render_plan(&doc.brief, &doc.backlog),
    );
    page(&doc.project, &format!("{header}{main}"))
}

/// Reads the project's whole record — the view document plus its
/// conversation thread — and renders the page. The one place `handle`'s
/// three routes (the GET page, `answer`, `ask`) share, so all three fail
/// the same way when any one read does.
fn render_full(
    forge: &Forge,
    project: &str,
    token: &str,
    pending: Option<(&str, &str)>,
) -> Result<String> {
    let doc = forge.project_view(project)?;
    let messages = conversation_messages(forge, project)?;
    let replies = conversation_replies(forge, project)?;
    Ok(render_page(&doc, token, &messages, &replies, pending))
}

fn screenshot_path<'a>(doc: &'a PortalDoc, target: &str) -> Option<&'a str> {
    doc.deploy_targets
        .iter()
        .find(|t| t.name == target)
        .and_then(|t| t.screenshot.as_deref())
}

/// Answers a blocked task's question: `forge answer <id> <text> --by
/// customer`, then the freshly re-read page. A missing or malformed
/// `id`/`text`, or `forge` itself failing, is the fixed write-error page
/// — never a hint of which.
fn handle_answer(mut req: Request, forge: &Forge, project: &str, token: &str) {
    let body = read_body(&mut req);
    let id = form_value(&body, "id").and_then(|v| v.parse::<i64>().ok());
    let text = form_value(&body, "text").filter(|t| !t.trim().is_empty());
    let (Some(id), Some(text)) = (id, text) else {
        let _ = req.respond(write_error(400));
        return;
    };
    let id = id.to_string();
    if forge.run(&["answer", &id, &text, "--by", CONTACT]).is_err() {
        let _ = req.respond(write_error(502));
        return;
    }
    match render_full(forge, project, token, None) {
        Ok(page) => {
            let _ = req.respond(html(200, &page));
        }
        Err(_) => {
            let _ = req.respond(write_error(502));
        }
    }
}

/// Sends a message through the concierge: `forge ask <project> <message>
/// --from customer`, then the freshly re-read page with the command's
/// stdout shown back as the reply line.
fn handle_ask(mut req: Request, forge: &Forge, project: &str, token: &str) {
    let body = read_body(&mut req);
    let Some(message) = form_value(&body, "message").filter(|m| !m.trim().is_empty()) else {
        let _ = req.respond(write_error(400));
        return;
    };
    let reply = match forge.run(&["ask", project, &message, "--from", CONTACT]) {
        Ok(out) => out.trim().to_string(),
        Err(_) => {
            let _ = req.respond(write_error(502));
            return;
        }
    };
    match render_full(forge, project, token, Some((&message, &reply))) {
        Ok(page) => {
            let _ = req.respond(html(200, &page));
        }
        Err(_) => {
            let _ = req.respond(write_error(502));
        }
    }
}

/// One request: resolve the token, then render the page, stream a
/// deploy target's last-look screenshot, or run a write.
fn handle(req: Request, forge: &Forge, limiter: &RateLimiter) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();
    let Some(rest) = path.strip_prefix("/p/") else {
        let _ = req.respond(not_found());
        return;
    };
    let (token, sub) = match rest.split_once('/') {
        Some((t, s)) => (t, Some(s)),
        None => (rest, None),
    };
    if token.is_empty() {
        let _ = req.respond(not_found());
        return;
    }
    let write_route = matches!(sub, Some("answer") | Some("ask"));
    match (req.method(), write_route) {
        (&Method::Get, false) | (&Method::Post, true) => {}
        _ => {
            let _ = req.respond(html(405, &page("Read-only", "<p>Read-only for now.</p>")));
            return;
        }
    }
    let project = match forge.resolve_portal_token(token) {
        Ok(p) => p,
        Err(_) => {
            let _ = req.respond(not_found());
            return;
        }
    };
    if write_route {
        if !limiter.allow(token) {
            let _ = req.respond(write_error(429));
            return;
        }
        match sub {
            Some("answer") => handle_answer(req, forge, &project, token),
            Some("ask") => handle_ask(req, forge, &project, token),
            _ => unreachable!(),
        }
        return;
    }
    match sub {
        None => match render_full(forge, &project, token, None) {
            Ok(page) => {
                let _ = req.respond(html(200, &page));
            }
            Err(_) => {
                let _ = req.respond(not_found());
            }
        },
        Some(sub) => {
            let doc = match forge.project_view(&project) {
                Ok(d) => d,
                Err(_) => {
                    let _ = req.respond(not_found());
                    return;
                }
            };
            let target = sub
                .strip_prefix("shot/")
                .filter(|t| !t.is_empty() && !t.contains('/'));
            let file = target.and_then(|t| screenshot_path(&doc, t));
            match file.and_then(|p| std::fs::File::open(p).ok()) {
                Some(f) => {
                    let _ = req.respond(
                        Response::from_file(f)
                            .with_header(h("Content-Type", "image/png"))
                            .with_header(h("Cache-Control", "no-store")),
                    );
                }
                None => {
                    let _ = req.respond(not_found());
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let mut bind = "127.0.0.1:7799".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" => bind = args.next().context("--bind needs an address")?,
            "-h" | "--help" => {
                println!(
                    "usage: forge-portal [--bind ADDR]   (default 127.0.0.1:7799; FORGE_BIN honoured)"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let forge = Forge::new();
    let limiter = Arc::new(RateLimiter::default());
    let server = Server::http(&bind).map_err(|e| anyhow::anyhow!("binding {bind}: {e}"))?;
    let addr = server.server_addr();
    eprintln!("forge-portal listening on {addr}");
    println!("http://{addr}");
    let server = Arc::new(server);
    for req in server.incoming_requests() {
        let forge = forge.clone();
        let limiter = limiter.clone();
        std::thread::spawn(move || handle(req, &forge, &limiter));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_utc_fallback_is_plain_and_correct() {
        assert_eq!(utc_text(0), "Jan 1, 1970, 00:00 UTC");
        assert_eq!(utc_text(1_726_531_200), "Sep 17, 2024, 00:00 UTC");
        assert_eq!(utc_text(1_699_999_999), "Nov 14, 2023, 22:13 UTC");
        assert_eq!(utc_text(-1), "Dec 31, 1969, 23:59 UTC");
    }

    #[test]
    fn a_moment_carries_its_seconds_and_a_utc_fallback() {
        assert_eq!(
            time_tag("Shipped ", 1_699_999_999),
            r#"<time data-ts="1699999999" data-prefix="Shipped ">Shipped Nov 14, 2023, 22:13 UTC</time>"#
        );
        assert_eq!(
            time_tag("", 0),
            r#"<time data-ts="0">Jan 1, 1970, 00:00 UTC</time>"#
        );
    }

    #[test]
    fn names_capitalize_only_the_first_letter() {
        assert_eq!(cap_first("waiting on you"), "Waiting on you");
        assert_eq!(cap_first(""), "");
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            esc("<b>Tom & Jerry's</b>"),
            "&lt;b&gt;Tom &amp; Jerry&#39;s&lt;/b&gt;"
        );
    }
}
