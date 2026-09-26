//! Bounded network egress for an attempt. The sandbox gives an attempt no
//! network of its own; the only route out is a proxy on the host that
//! answers for a list of rules, and this module is both the list (`Rule`)
//! and the proxy. A repository declares its rules in forge.toml
//! (`[sandbox] egress = ["registry.npmjs.org", "*.crates.io:443"]`), the
//! model endpoint is always allowed on top, and everything else is refused
//! with a 403 that names the host.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};

/// Where the proxy's socket is bound inside the sandbox.
pub const SANDBOX_SOCKET: &str = "/run/forge/egress.sock";
/// Where the in-sandbox relay listens, and what HTTP_PROXY names.
pub const RELAY_ADDR: &str = "127.0.0.1:3128";
/// A request to this name is answered by the proxy itself with the policy:
/// the one host that always resolves, needing no network, so a probe can
/// tell "the route to the proxy works" from "the route is missing".
pub const POLICY_HOST: &str = "forge-egress.invalid";
/// The header on the proxy's own 403, naming the refused `host:port`, so
/// the relay can tell a refusal from a 403 an upstream server sent.
pub const REFUSED_HEADER: &str = "X-Forge-Egress-Refused";
/// The file, in a clone's `.git`, the relay records refusals in.
pub const REFUSED_FILE: &str = "forge-egress-refused.jsonl";

/// The host a request must name for a rule to apply.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Host {
    /// `registry.npmjs.org`, or a literal IPv4 address.
    Exact(String),
    /// `*.github.com`: any host below `github.com`, not `github.com`
    /// itself. Stored without the `*.`.
    Suffix(String),
}

/// One allowlist entry: `host`, `host:port`, `*.suffix` or `*.suffix:port`.
/// Without a port, the ports a web request uses (443 and 80) are allowed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rule {
    host: Host,
    port: Option<u16>,
}

/// How a rule matched, which decides what the address it resolves to may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matched {
    /// The operator wrote this host down, so where it resolves is theirs to
    /// vouch for (a model on the LAN is a private address on purpose).
    Exact,
    /// Any name below a suffix: a name the operator never saw, so it must
    /// not resolve to loopback or a private range (DNS rebinding).
    Suffix,
}

impl Rule {
    pub fn parse(s: &str) -> Result<Rule> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            bail!("an egress entry is empty");
        }
        if s.contains("://")
            || s.contains('/')
            || s.contains('@')
            || s.contains(char::is_whitespace)
        {
            bail!("egress entry {s:?}: write a host, not a URL (host, host:port, *.suffix)");
        }
        let (host, port) = match s.matches(':').count() {
            0 => (s.as_str(), None),
            1 => {
                let (h, p) = s.split_once(':').expect("one colon");
                match p.parse::<u16>() {
                    Ok(p) if p != 0 => (h, Some(p)),
                    _ => bail!("egress entry {s:?}: {p:?} is not a port"),
                }
            }
            _ => bail!("egress entry {s:?}: IPv6 literals are not supported; name a host"),
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        let valid_label = |l: &str| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        let host = match host.strip_prefix("*.") {
            Some(rest) => {
                if !rest.contains('.') || !rest.split('.').all(valid_label) {
                    bail!(
                        "egress entry {s:?}: a wildcard must be *.suffix with a real domain below it (a bare * or *.com would allow the world)"
                    );
                }
                Host::Suffix(rest.to_string())
            }
            None => {
                if !host.split('.').all(valid_label) {
                    bail!("egress entry {s:?}: {host:?} is not a host name");
                }
                Host::Exact(host.to_string())
            }
        };
        Ok(Rule { host, port })
    }

    /// Whether this rule lets a connection to `host:port` through.
    pub fn matches(&self, host: &str, port: u16) -> Option<Matched> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let port_ok = match self.port {
            Some(p) => p == port,
            None => port == 443 || port == 80,
        };
        if !port_ok {
            return None;
        }
        match &self.host {
            Host::Exact(h) if *h == host => Some(Matched::Exact),
            Host::Suffix(s) => host
                .strip_suffix(s.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.'))
                .then_some(Matched::Suffix),
            _ => None,
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Host::Exact(h) => write!(f, "{h}")?,
            Host::Suffix(s) => write!(f, "*.{s}")?,
        }
        if let Some(p) = self.port {
            write!(f, ":{p}")?;
        }
        Ok(())
    }
}

/// What a URL in a provider's configuration lets through: its host, and its
/// port when it names one (else the scheme's: 80 for http, 443 for https).
fn rule_for_url(url: &str) -> Option<Rule> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let entry = match (scheme, authority.contains(':')) {
        ("http", false) => format!("{authority}:80"),
        ("http" | "https", _) => authority.to_string(),
        _ => return None,
    };
    Rule::parse(&entry).ok()
}

/// The model endpoints every attempt may reach: for each configured
/// provider, its runner's own hosts, its `base_url`, and any URL in its
/// `env` (a local model's OLLAMA_HOST, codex's OSS base URL). These carry
/// the token an attempt runs with, so they are the one thing always allowed.
pub fn model_rules(providers: &BTreeMap<String, crate::agent::Provider>) -> Vec<Rule> {
    use crate::agent::Runner;
    let mut rules = Vec::new();
    for p in providers.values() {
        let own: &[&str] = match p.runner {
            // The API, and the sign-in the CLI refreshes its token against.
            Runner::ClaudeCli => &["*.anthropic.com", "*.claude.com", "claude.ai"],
            Runner::CodexCli => &["*.openai.com", "chatgpt.com"],
            // The Copilot API and the GitHub API its token is checked
            // against; not github.com itself, which is not a model
            // endpoint and would be a route out for anything.
            Runner::CopilotCli => &["*.githubcopilot.com", "api.github.com"],
            Runner::Chat => &[],
        };
        rules.extend(own.iter().filter_map(|h| Rule::parse(h).ok()));
        rules.extend(p.base_url.as_deref().and_then(rule_for_url));
        rules.extend(p.env.iter().filter_map(|(_, v)| rule_for_url(v)));
    }
    Policy::new(rules).rules
}

/// The rules one proxy enforces: sorted and de-duplicated, so two attempts
/// declaring the same hosts in a different order share one proxy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    pub fn new(rules: impl IntoIterator<Item = Rule>) -> Policy {
        let mut rules: Vec<Rule> = rules.into_iter().collect();
        rules.sort();
        rules.dedup();
        Policy { rules }
    }

    #[cfg(test)]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    fn allows(&self, host: &str, port: u16) -> Option<Matched> {
        // An exact rule outranks a suffix one: it is the stronger statement.
        let mut found = None;
        for r in &self.rules {
            match r.matches(host, port) {
                Some(Matched::Exact) => return Some(Matched::Exact),
                Some(m) => found = Some(m),
                None => {}
            }
        }
        found
    }

    /// What `GET http://forge-egress.invalid/` answers.
    fn describe(&self) -> String {
        let mut out = String::from("forge-egress: ok\n");
        for r in &self.rules {
            out.push_str(&format!("allow {r}\n"));
        }
        out.push_str("everything else is refused with 403\n");
        out
    }
}

/// Whether `ip` is somewhere on the public internet: not loopback, not a
/// private, link-local, shared or reserved range.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_documentation()
                || v.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || o[0] >= 240)
        }
        IpAddr::V6(v) => match v.to_ipv4_mapped() {
            Some(m) => is_public(IpAddr::V4(m)),
            None => {
                let s = v.segments();
                !(v.is_loopback()
                    || v.is_unspecified()
                    || v.is_multicast()
                    || (s[0] & 0xfe00) == 0xfc00
                    || (s[0] & 0xffc0) == 0xfe80)
            }
        },
    }
}

const HEAD_LIMIT: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Bind the proxy's socket, removing a stale file left by a crashed run.
pub fn bind(path: &Path) -> Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
}

/// Answer connections on `listener` under `policy` until the task is aborted.
pub async fn serve(listener: UnixListener, policy: Arc<Policy>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let policy = policy.clone();
        tokio::spawn(async move {
            let _ = handle(stream, &policy).await;
        });
    }
}

async fn respond(s: &mut UnixStream, status: &str, body: &str) -> Result<()> {
    respond_with(s, status, "", body).await
}

async fn respond_with(s: &mut UnixStream, status: &str, extra: &str, body: &str) -> Result<()> {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(msg.as_bytes()).await?;
    s.shutdown().await.ok();
    Ok(())
}

/// Refuse `what` (`CONNECT host:port`, `GET http://host:port`): a 403 whose
/// body names the host and the policy, and whose `REFUSED_HEADER` lets the
/// relay in the sandbox record the refusal for the attempt.
async fn refuse(
    s: &mut UnixStream,
    what: &str,
    host: &str,
    port: u16,
    policy: &Policy,
) -> Result<()> {
    eprintln!("egress: refused {what}");
    let allowed: Vec<String> = policy.rules.iter().map(|r| r.to_string()).collect();
    let body = format!(
        "forge egress: {host}:{port} is not allowed. This attempt may reach only: {}.\nA repository declares more in forge.toml under [sandbox] egress.\n",
        allowed.join(", ")
    );
    let header = format!("{REFUSED_HEADER}: {}\r\n", authority(host, port));
    respond_with(s, "403 Forbidden", &header, &body).await
}

/// `host:port`, bracketing a v6 address so `split_authority` reads it back.
fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Split `host:port`, `host` (with `default`) or `[v6]:port`.
fn split_authority(a: &str, default: u16) -> Option<(String, u16)> {
    if a.is_empty() || a.contains(['/', '@', ' ']) {
        return None;
    }
    if let Some(rest) = a.strip_prefix('[') {
        let (h, p) = rest.split_once(']')?;
        let port = match p.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if p.is_empty() => default,
            None => return None,
        };
        return Some((h.to_string(), port));
    }
    match a.split_once(':') {
        Some((h, p)) if !h.is_empty() && !p.contains(':') => Some((h.to_string(), p.parse().ok()?)),
        Some(_) => None,
        None => Some((a.to_string(), default)),
    }
}

/// Resolve `host` and connect. A name a suffix rule matched must not lead to
/// a loopback or private address: the name is not one the operator wrote.
async fn dial(host: &str, port: u16, matched: Matched) -> Result<TcpStream> {
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("resolving {host}"))?
            .collect(),
    };
    let mut last = None;
    for a in addrs {
        if matched == Matched::Suffix && !is_public(a.ip()) {
            last = Some(anyhow::anyhow!(
                "{host} resolves to {}, which is not a public address",
                a.ip()
            ));
            continue;
        }
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = Some(e.into()),
            Err(_) => last = Some(anyhow::anyhow!("connecting to {a} timed out")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("{host} did not resolve")))
}

async fn handle(mut client: UnixStream, policy: &Policy) -> Result<()> {
    // The head: request line and headers, up to the blank line. Whatever
    // the client sent after it is body (or the start of a tunnel) and goes
    // upstream untouched.
    let mut buf = Vec::with_capacity(1024);
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > HEAD_LIMIT {
            return respond(
                &mut client,
                "431 Request Header Fields Too Large",
                "too large\n",
            )
            .await;
        }
        let mut chunk = [0u8; 2048];
        let n = tokio::time::timeout(HEAD_TIMEOUT, client.read(&mut chunk))
            .await
            .context("reading the request head")??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let (head, rest) = buf.split_at(end);
    let head = String::from_utf8_lossy(head).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return respond(&mut client, "400 Bad Request", "bad request line\n").await;
    };
    let headers: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_authority(target, 443) else {
            return respond(&mut client, "400 Bad Request", "bad CONNECT target\n").await;
        };
        let Some(matched) = policy.allows(&host, port) else {
            let what = format!("CONNECT {host}:{port}");
            return refuse(&mut client, &what, &host, port, policy).await;
        };
        let mut upstream = match dial(&host, port, matched).await {
            Ok(u) => u,
            Err(e) => {
                return respond(
                    &mut client,
                    "502 Bad Gateway",
                    &format!("forge egress: {e:#}\n"),
                )
                .await;
            }
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if !rest.is_empty() {
            upstream.write_all(rest).await?;
        }
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .ok();
        return Ok(());
    }

    // A plain HTTP request. Through a proxy its target is absolute; a
    // request for the policy may also come origin-form with a Host header.
    let host_header = headers.iter().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("host").then(|| v.trim().to_string())
    });
    let (authority, path) = match target.strip_prefix("http://") {
        Some(rest) => match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), format!("/{p}")),
            None => (rest.to_string(), "/".to_string()),
        },
        None if target.starts_with('/') => match host_header {
            Some(h) => (h, target.to_string()),
            None => return respond(&mut client, "400 Bad Request", "no host\n").await,
        },
        None => {
            return respond(&mut client, "400 Bad Request", "use CONNECT for https\n").await;
        }
    };
    let Some((host, port)) = split_authority(&authority, 80) else {
        return respond(&mut client, "400 Bad Request", "bad host\n").await;
    };
    if host.eq_ignore_ascii_case(POLICY_HOST) {
        return respond(&mut client, "200 OK", &policy.describe()).await;
    }
    let Some(matched) = policy.allows(&host, port) else {
        let what = format!("{method} http://{host}:{port}");
        return refuse(&mut client, &what, &host, port, policy).await;
    };
    let mut upstream = match dial(&host, port, matched).await {
        Ok(u) => u,
        Err(e) => {
            return respond(
                &mut client,
                "502 Bad Gateway",
                &format!("forge egress: {e:#}\n"),
            )
            .await;
        }
    };
    // Origin-form, and `Connection: close`: this connection carries one
    // request to one host, so a second request cannot ride it to a host the
    // policy never saw.
    let mut out = format!("{method} {path} {version}\r\n");
    for h in headers {
        let name = h
            .split_once(':')
            .map(|(k, _)| k.trim().to_ascii_lowercase());
        if matches!(
            name.as_deref(),
            Some("connection" | "proxy-connection" | "proxy-authorization" | "keep-alive")
        ) {
            continue;
        }
        out.push_str(h);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    upstream.write_all(out.as_bytes()).await?;
    upstream.write_all(rest).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .ok();
    Ok(())
}

/// The relay `forge egress-relay` runs inside the sandbox: listen on
/// loopback (the only network the namespace has) and pipe every connection
/// to the proxy's unix socket. `ready` is created once listening, so the
/// wrapper that started it can wait for the route before running the agent.
/// Each refusal the proxy answers is appended to `refused` (see
/// `refused_path`), so a refusal a tool swallowed is still on the record.
pub async fn relay(
    socket: &Path,
    listen: &str,
    ready: Option<&Path>,
    refused: Option<&Path>,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    if let Some(r) = ready {
        std::fs::write(r, b"").with_context(|| format!("writing {}", r.display()))?;
    }
    loop {
        let (tcp, _) = listener.accept().await?;
        let socket = socket.to_path_buf();
        let refused = refused.map(Path::to_path_buf);
        tokio::spawn(async move {
            if let Ok(unix) = UnixStream::connect(&socket).await {
                pipe(tcp, unix, refused.as_deref()).await;
            }
        });
    }
}

/// Read from `r` until the end of an HTTP head (or EOF, or the limit).
async fn read_head(r: &mut (impl AsyncReadExt + Unpin)) -> Vec<u8> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() <= HEAD_LIMIT {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
    }
    head
}

/// Pipe one connection both ways, reading the request head and the answer's
/// head on the way through: a 403 from the proxy naming the host the request
/// asked for is a refusal, recorded in `refused`.
async fn pipe(tcp: TcpStream, unix: UnixStream, refused: Option<&Path>) {
    let (mut tr, mut tw) = tcp.into_split();
    let (mut ur, mut uw) = unix.into_split();
    let request = read_head(&mut tr).await;
    if uw.write_all(&request).await.is_err() {
        return;
    }
    let asked = requested(&request);
    let up = async {
        tokio::io::copy(&mut tr, &mut uw).await.ok();
        uw.shutdown().await.ok();
    };
    let down = async {
        let answer = read_head(&mut ur).await;
        if let (Some(path), Some(asked)) = (refused, &asked)
            && refused_in(&answer).as_ref() == Some(asked)
        {
            note_refused(path, &asked.0, asked.1);
        }
        if tw.write_all(&answer).await.is_ok() {
            tokio::io::copy(&mut ur, &mut tw).await.ok();
        }
        tw.shutdown().await.ok();
    };
    tokio::join!(up, down);
}

/// The host and port a proxy request asks for.
fn requested(head: &[u8]) -> Option<(String, u16)> {
    let head = String::from_utf8_lossy(head);
    let mut parts = head.lines().next()?.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    if method.eq_ignore_ascii_case("CONNECT") {
        return split_authority(target, 443);
    }
    let rest = target.strip_prefix("http://")?;
    split_authority(rest.split('/').next()?, 80)
}

/// The host and port a 403 from the proxy names in its `REFUSED_HEADER`.
fn refused_in(head: &[u8]) -> Option<(String, u16)> {
    let head = String::from_utf8_lossy(head);
    let mut lines = head.lines();
    if lines.next()?.split(' ').nth(1) != Some("403") {
        return None;
    }
    lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(REFUSED_HEADER)
            .then(|| split_authority(v.trim(), 443))
            .flatten()
    })
}

/// One host the proxy refused an attempt, and how many times.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Refused {
    pub host: String,
    pub port: u16,
    pub count: u64,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} x{}", authority(&self.host, self.port), self.count)
    }
}

/// Where the relays of attempts in `dir` record refusals: beside the
/// attempt's own state in the clone's `.git`, which the kernel owns and
/// no commit carries. `None` when `dir` is not a clone.
pub fn refused_path(dir: &Path) -> Option<PathBuf> {
    let git = dir.join(".git");
    git.is_dir().then(|| git.join(REFUSED_FILE))
}

/// Append one refusal. Best effort: a record that cannot be written must
/// never break the connection it describes.
fn note_refused(path: &Path, host: &str, port: u16) {
    use std::io::Write;
    let line = serde_json::json!({ "host": host, "port": port, "count": 1 });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The refusals recorded for `dir`, one per host and port with the counts
/// summed, most refused first.
pub fn read_refused(dir: &Path) -> Vec<Refused> {
    let Some(text) = refused_path(dir).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let mut counts: BTreeMap<(String, u16), u64> = BTreeMap::new();
    for r in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Refused>(l).ok())
    {
        *counts.entry((r.host, r.port)).or_default() += r.count;
    }
    let mut out: Vec<Refused> = counts
        .into_iter()
        .map(|((host, port), count)| Refused { host, port, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count));
    out
}

/// Write `refused` back into `dir`'s record: what the agent was refused,
/// carried across the verification checkout that replaces its `.git`, so
/// the checks' own refusals add to it.
pub fn restore_refused(dir: &Path, refused: &[Refused]) {
    use std::io::Write;
    let Some(path) = refused_path(dir) else {
        return;
    };
    if refused.is_empty() {
        return;
    }
    let text: String = refused
        .iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .map(|l| format!("{l}\n"))
        .collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Forget what earlier attempts in `dir` were refused, so what is read at
/// the end of this one is its own.
pub fn clear_refused(dir: &Path) {
    if let Some(p) = refused_path(dir) {
        let _ = std::fs::remove_file(p);
    }
}

/// The proxies a Forge process runs, one per distinct policy, each on its
/// own socket in a private directory that goes away with the process.
#[derive(Default)]
pub struct Proxies {
    dir: Mutex<Option<PathBuf>>,
    running: Mutex<BTreeMap<Policy, (PathBuf, tokio::task::JoinHandle<()>)>>,
}

impl Proxies {
    /// The socket of a proxy enforcing `policy`, started if it is not
    /// running. Needs a tokio runtime; without one there is no proxy, and
    /// so no route out.
    pub fn socket_for(&self, policy: &Policy) -> Result<PathBuf> {
        let handle = tokio::runtime::Handle::try_current()
            .context("no runtime to run the egress proxy on")?;
        let mut running = self.running.lock().unwrap();
        if let Some((path, task)) = running.get(policy)
            && !task.is_finished()
        {
            return Ok(path.clone());
        }
        let dir = {
            let mut d = self.dir.lock().unwrap();
            match &*d {
                Some(d) => d.clone(),
                None => {
                    let path =
                        std::env::temp_dir().join(format!("forge-egress-{}", std::process::id()));
                    let _ = std::fs::remove_dir_all(&path);
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&path)
                        .with_context(|| format!("creating {}", path.display()))?;
                    *d = Some(path.clone());
                    path
                }
            }
        };
        let path = dir.join(format!("p{}.sock", running.len()));
        let listener = {
            let _guard = handle.enter();
            bind(&path)?
        };
        let task = handle.spawn(serve(listener, Arc::new(policy.clone())));
        running.insert(policy.clone(), (path.clone(), task));
        Ok(path)
    }
}

impl Drop for Proxies {
    fn drop(&mut self) {
        for (_, task) in self.running.lock().unwrap().values() {
            task.abort();
        }
        if let Some(d) = self.dir.lock().unwrap().as_ref() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(s: &str) -> Rule {
        Rule::parse(s).unwrap()
    }

    #[test]
    fn a_host_matches_itself_on_the_web_ports_only() {
        let r = rule("registry.npmjs.org");
        assert_eq!(r.matches("registry.npmjs.org", 443), Some(Matched::Exact));
        assert_eq!(r.matches("Registry.NPMJS.org.", 80), Some(Matched::Exact));
        assert_eq!(r.matches("registry.npmjs.org", 22), None);
        assert_eq!(r.matches("evil-registry.npmjs.org", 443), None);
        assert_eq!(r.matches("registry.npmjs.org.evil.com", 443), None);
    }

    #[test]
    fn a_port_pins_the_port() {
        let r = rule("dev.home:11434");
        assert_eq!(r.matches("dev.home", 11434), Some(Matched::Exact));
        assert_eq!(r.matches("dev.home", 443), None);
    }

    #[test]
    fn a_suffix_matches_below_the_domain_and_not_the_domain() {
        let r = rule("*.crates.io");
        assert_eq!(r.matches("static.crates.io", 443), Some(Matched::Suffix));
        assert_eq!(r.matches("a.b.crates.io", 443), Some(Matched::Suffix));
        assert_eq!(r.matches("crates.io", 443), None);
        assert_eq!(r.matches("evilcrates.io", 443), None);
        assert_eq!(r.matches("static.crates.io.evil.com", 443), None);
    }

    #[test]
    fn what_is_not_a_host_is_refused() {
        for bad in [
            "",
            "*",
            "*.com",
            "*.",
            "https://x.io",
            "x.io/path",
            "user@x.io",
            "x.io:0",
            "x.io:http",
            "x .io",
            "-x.io",
            "a..b",
            "::1",
            "*x.io",
            "x.*.io",
        ] {
            assert!(Rule::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "registry.npmjs.org",
            "*.github.com",
            "dev.home:11434",
            "*.x.io:8443",
            "10.0.0.5",
        ] {
            assert_eq!(rule(s).to_string(), s);
        }
        assert_eq!(rule("  Example.COM ").to_string(), "example.com");
    }

    /// A proxy on a socket in a temp dir, allowing `rules`.
    fn start(rules: &[&str]) -> (tempfile::TempDir, PathBuf, tokio::task::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("p.sock");
        let policy = Policy::new(rules.iter().map(|r| rule(r)));
        let task = tokio::spawn(serve(bind(&sock).unwrap(), Arc::new(policy)));
        (dir, sock, task)
    }

    /// Send `req`, read until the proxy closes, return everything it said.
    async fn ask(sock: &Path, req: &str) -> String {
        let mut s = UnixStream::connect(sock).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
            .await
            .expect("the proxy answers")
            .ok();
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A local server that answers one connection with what `reply` makes
    /// of the bytes it received first.
    async fn upstream(reply: fn(&str) -> String) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut c, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = c.read(&mut buf).await.unwrap_or(0);
                    let got = String::from_utf8_lossy(&buf[..n]).into_owned();
                    c.write_all(reply(&got).as_bytes()).await.ok();
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_host_off_the_list_gets_a_403_that_names_it_for_connect_and_for_http() {
        let (_d, sock, task) = start(&["registry.npmjs.org"]);
        let r = ask(
            &sock,
            "CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n",
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        assert!(r.contains("evil.example:443"), "{r}");
        assert!(
            r.contains("registry.npmjs.org"),
            "the refusal lists what is allowed: {r}"
        );
        let r = ask(
            &sock,
            "GET http://evil.example/x HTTP/1.1\r\nHost: evil.example\r\n\r\n",
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        // An allowed host on a port the rule does not cover is refused too.
        let r = ask(&sock, "CONNECT registry.npmjs.org:22 HTTP/1.1\r\n\r\n").await;
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        task.abort();
    }

    #[tokio::test]
    async fn the_policy_host_answers_with_the_rules_and_needs_no_network() {
        let (_d, sock, task) = start(&["registry.npmjs.org", "*.crates.io"]);
        let r = ask(
            &sock,
            &format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\nHost: {POLICY_HOST}\r\n\r\n"),
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(r.contains("forge-egress: ok"), "{r}");
        assert!(r.contains("allow registry.npmjs.org"), "{r}");
        assert!(r.contains("allow *.crates.io"), "{r}");
        task.abort();
    }

    #[tokio::test]
    async fn connect_to_an_allowed_host_tunnels_bytes_both_ways() {
        let port = upstream(|got| format!("echo:{got}")).await;
        let (_d, sock, task) = start(&[&format!("127.0.0.1:{port}")]);
        let mut s = UnixStream::connect(&sock).await.unwrap();
        s.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut head = [0u8; 39];
        s.read_exact(&mut head).await.unwrap();
        assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200 Connection Established"));
        s.write_all(b"hello").await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
            .await
            .unwrap()
            .ok();
        assert_eq!(String::from_utf8_lossy(&out), "echo:hello");
        task.abort();
    }

    #[tokio::test]
    async fn an_allowed_http_request_is_forwarded_origin_form_and_closed() {
        let port = upstream(|got| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{got}",
                got.len()
            )
        })
        .await;
        let (_d, sock, task) = start(&[&format!("127.0.0.1:{port}")]);
        let r = ask(
            &sock,
            &format!("GET http://127.0.0.1:{port}/a/b?c=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n"),
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
        assert!(
            r.contains("GET /a/b?c=1 HTTP/1.1\r\n"),
            "the target is origin-form: {r}"
        );
        assert!(r.contains("Accept: */*"), "{r}");
        assert!(r.contains("Connection: close"), "{r}");
        assert!(!r.contains("Proxy-Connection"), "{r}");
        task.abort();
    }

    #[tokio::test]
    async fn the_relay_pipes_loopback_to_the_proxy_socket() {
        let (dir, sock, task) = start(&["registry.npmjs.org"]);
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap().to_string();
        drop(free);
        let ready = dir.path().join("ready");
        let (a, r) = (addr.clone(), ready.clone());
        let relay_task = tokio::spawn(async move { relay(&sock, &a, Some(&r), None).await });
        for _ in 0..100 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ready.exists(), "the relay signals it is listening");
        let mut s = TcpStream::connect(&addr).await.unwrap();
        s.write_all(format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
            .await
            .unwrap()
            .ok();
        assert!(String::from_utf8_lossy(&out).contains("allow registry.npmjs.org"));
        relay_task.abort();
        task.abort();
    }

    #[tokio::test]
    async fn the_relay_records_each_refusal_beside_the_attempt() {
        let (dir, sock, task) = start(&["registry.npmjs.org"]);
        let clone = dir.path().join("clone");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        let record = refused_path(&clone).unwrap();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap().to_string();
        drop(free);
        let ready = dir.path().join("ready");
        let (a, r, rec) = (addr.clone(), ready.clone(), record.clone());
        let relay_task = tokio::spawn(async move { relay(&sock, &a, Some(&r), Some(&rec)).await });
        for _ in 0..100 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let requests = [
            "CONNECT http-intake.logs.example.com:443 HTTP/1.1\r\n\r\n".to_string(),
            "CONNECT http-intake.logs.example.com:443 HTTP/1.1\r\n\r\n".to_string(),
            "GET http://evil.example/x HTTP/1.1\r\nHost: evil.example\r\n\r\n".to_string(),
            // The policy host answers 200: not a refusal.
            format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\n\r\n"),
        ];
        for req in requests {
            let mut s = TcpStream::connect(&addr).await.unwrap();
            s.write_all(req.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
                .await
                .unwrap()
                .ok();
            assert!(!out.is_empty(), "the answer still reaches the client");
        }
        let got: Vec<String> = read_refused(&clone).iter().map(|r| r.to_string()).collect();
        assert_eq!(
            got,
            ["http-intake.logs.example.com:443 x2", "evil.example:80 x1"]
        );
        // Carried across a new `.git`, and forgotten at the next attempt.
        let refused = read_refused(&clone);
        clear_refused(&clone);
        assert!(read_refused(&clone).is_empty());
        restore_refused(&clone, &refused);
        assert_eq!(read_refused(&clone), refused);
        relay_task.abort();
        task.abort();
    }

    #[test]
    fn a_403_counts_as_a_refusal_only_when_the_proxy_names_the_host_asked_for() {
        let ask = b"CONNECT a.example:443 HTTP/1.1\r\n\r\n";
        assert_eq!(requested(ask), Some(("a.example".into(), 443)));
        assert_eq!(
            requested(b"GET http://b.example:8080/x HTTP/1.1\r\n\r\n"),
            Some(("b.example".into(), 8080))
        );
        assert_eq!(requested(b""), None);
        let refused = format!("HTTP/1.1 403 Forbidden\r\n{REFUSED_HEADER}: a.example:443\r\n\r\n");
        assert_eq!(
            refused_in(refused.as_bytes()),
            Some(("a.example".into(), 443))
        );
        // An upstream server's own 403 carries no header.
        assert_eq!(refused_in(b"HTTP/1.1 403 Forbidden\r\n\r\n"), None);
        let ok = format!("HTTP/1.1 200 OK\r\n{REFUSED_HEADER}: a.example:443\r\n\r\n");
        assert_eq!(refused_in(ok.as_bytes()), None);
    }

    #[test]
    fn a_directory_that_is_not_a_clone_records_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(refused_path(d.path()).is_none());
        assert!(read_refused(d.path()).is_empty());
    }

    #[test]
    fn only_public_addresses_pass_for_a_suffix_match() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "93.184.216.34", "2606:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_url_becomes_the_rule_for_its_host_and_port() {
        let r = |u: &str| rule_for_url(u).map(|r| r.to_string());
        assert_eq!(
            r("https://api.openai.com/v1"),
            Some("api.openai.com".into())
        );
        assert_eq!(r("http://dev.home:11434/v1"), Some("dev.home:11434".into()));
        assert_eq!(r("http://dev.home/v1"), Some("dev.home:80".into()));
        assert_eq!(r("https://u:p@x.io:8443/a?b"), Some("x.io:8443".into()));
        assert_eq!(r("file:///etc"), None);
        assert_eq!(r("not a url"), None);
    }

    #[test]
    fn the_model_endpoint_is_always_in_the_rules() {
        let mut providers = BTreeMap::new();
        providers.insert("anthropic".to_string(), crate::agent::Provider::default());
        let rules: Vec<String> = model_rules(&providers)
            .iter()
            .map(|r| r.to_string())
            .collect();
        assert!(rules.iter().any(|r| r == "*.anthropic.com"), "{rules:?}");
        let p = crate::agent::Provider {
            name: "devhome".into(),
            runner: crate::agent::Runner::CodexCli,
            env: vec![("OLLAMA_HOST".into(), "http://dev.home:11434".into())],
            ..crate::agent::Provider::default()
        };
        providers.insert("devhome".to_string(), p);
        let rules: Vec<String> = model_rules(&providers)
            .iter()
            .map(|r| r.to_string())
            .collect();
        assert!(rules.iter().any(|r| r == "dev.home:11434"), "{rules:?}");
    }

    #[test]
    fn a_policy_is_the_same_whatever_order_its_rules_came_in() {
        let a = Policy::new([rule("b.io"), rule("a.io"), rule("a.io")]);
        let b = Policy::new([rule("a.io"), rule("b.io")]);
        assert_eq!(a, b);
        assert_eq!(a.rules().len(), 2);
    }
}
