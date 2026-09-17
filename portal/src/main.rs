//! forge-portal: the customer portal (see docs/PORTAL.md), a read-only
//! page at `/p/<token>` showing one project's work in plain words. Like
//! `forge-web` it never touches the kernel: every read is a `forge`
//! verb's JSON. `GET /p/<token>` resolves the token to a project through
//! `forge project resolve-token`, then renders `forge project view
//! --json` as four sections: Running for you, Being built, Done, Your
//! plan. An unknown or revoked token is a plain 404 page, with no hint
//! of why. `GET /p/<token>/shot/<target>` streams that deploy target's
//! last-look screenshot file. This is the read-only step (docs/PORTAL.md,
//! "Build order" 2); Needs you and the Ask box are step 3.

use anyhow::{Context, Result};
use forge_client::{
    Forge, PortalBacklogItem, PortalBrief, PortalDeployTarget, PortalDoc, PortalInitiative,
    PortalLanded,
};
use std::io::Cursor;
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const STYLE: &str = include_str!("style.css");

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

fn human_date(ts: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = civil_from_days(ts.div_euclid(86400));
    format!("{} {d}, {y}", MONTHS[(m - 1) as usize])
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
    if targets.is_empty() {
        return r#"<p class="empty">Nothing runs for you yet.</p>"#.to_string();
    }
    let mut out = String::new();
    for t in targets {
        let (class, phrase) = deploy_status(t);
        let when = match t.last_deployed_at {
            Some(ts) => format!("Last updated {}", human_date(ts)),
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

fn render_initiatives(items: &[PortalInitiative]) -> String {
    if items.is_empty() {
        return r#"<p class="empty">Nothing being built right now.</p>"#.to_string();
    }
    let mut out = String::from(r#"<ul class="plain">"#);
    for i in items {
        out.push_str(&format!(
            r#"<li><div>{}</div><div class="date">{}</div></li>"#,
            esc(&i.outcome),
            esc(&cap_first(&i.state)),
        ));
    }
    out.push_str("</ul>");
    out
}

fn render_landed(items: &[PortalLanded]) -> String {
    if items.is_empty() {
        return r#"<p class="empty">Nothing has shipped yet.</p>"#.to_string();
    }
    let mut out = String::from(r#"<ul class="plain">"#);
    for l in items {
        out.push_str(&format!(
            r#"<li><div>{}</div><div class="date">Shipped {}</div></li>"#,
            esc(&l.text),
            human_date(l.landed_at),
        ));
    }
    out.push_str("</ul>");
    out
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

fn render_page(doc: &PortalDoc, token: &str) -> String {
    let header = format!(
        r#"<header><h1>{}</h1><p>{}</p></header>"#,
        esc(&doc.project),
        esc(&doc.purpose)
    );
    let main = format!(
        r#"<main>
<section><h2>Running for you</h2>{targets}</section>
<section><h2>Being built</h2>{initiatives}</section>
<section><h2>Done</h2>{landed}</section>
<section><h2>Your plan</h2>{plan}</section>
</main>"#,
        targets = render_targets(&doc.deploy_targets, token),
        initiatives = render_initiatives(&doc.initiatives),
        landed = render_landed(&doc.landed),
        plan = render_plan(&doc.brief, &doc.backlog),
    );
    page(&doc.project, &format!("{header}{main}"))
}

fn screenshot_path<'a>(doc: &'a PortalDoc, target: &str) -> Option<&'a str> {
    doc.deploy_targets
        .iter()
        .find(|t| t.name == target)
        .and_then(|t| t.screenshot.as_deref())
}

/// One request: resolve the token, then either render the page or stream
/// a deploy target's last-look screenshot.
fn handle(req: Request, forge: &Forge) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();
    if req.method() != &Method::Get {
        let _ = req.respond(html(405, &page("Read-only", "<p>Read-only for now.</p>")));
        return;
    }
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
    let project = match forge.resolve_portal_token(token) {
        Ok(p) => p,
        Err(_) => {
            let _ = req.respond(not_found());
            return;
        }
    };
    let doc = match forge.project_view(&project) {
        Ok(d) => d,
        Err(_) => {
            let _ = req.respond(not_found());
            return;
        }
    };
    match sub {
        None => {
            let _ = req.respond(html(200, &render_page(&doc, token)));
        }
        Some(sub) => {
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
    let server = Server::http(&bind).map_err(|e| anyhow::anyhow!("binding {bind}: {e}"))?;
    let addr = server.server_addr();
    eprintln!("forge-portal listening on {addr}");
    println!("http://{addr}");
    let server = Arc::new(server);
    for req in server.incoming_requests() {
        let forge = forge.clone();
        std::thread::spawn(move || handle(req, &forge));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_plain_and_correct() {
        assert_eq!(human_date(0), "Jan 1, 1970");
        assert_eq!(human_date(1_726_531_200), "Sep 17, 2024");
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
