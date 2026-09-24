//! Environment needs: a missing tool is a policy decision, never a human
//! question. `recognize` is the deterministic half, pure code over the tail
//! of a failed setup or check, or over a `needs_input` question: it names
//! the host the egress proxy refused, the binary missing from PATH, the
//! toolchain or browser cache that is not there. `Policy` is the operator's
//! `[environment]` table, what may be granted without asking; the engine
//! applies a covered need (`ctx::Forge::grant_environment`), records it as
//! a decision row by `forge` (`record`) and re-runs without spending a
//! retry. A host or cache need the table does not cover goes to the
//! supervisor (`env_supervisor`), which may approve only what
//! `within_ceiling` allows; a denial reaches the operator as a yes/no
//! question. Anything else is left as it always was.

use crate::store::Store;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Who allowed a grant: the operator's table, or the supervisor within the
/// ceiling (`within_ceiling`) with its one-line reason.
pub enum Approval {
    Policy,
    Supervisor(String),
}

impl Approval {
    fn answered_by(&self) -> &'static str {
        match self {
            Approval::Policy => "forge",
            Approval::Supervisor(_) => "supervisor",
        }
    }
}

/// The `decisions.kind` of a grant, which `forge doctor` lists.
pub const DECISION_KIND: &str = "environment-grant";

const DEFAULT_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "index.crates.io",
    "static.crates.io",
    "nodejs.org",
    "cdn.playwright.dev",
    "playwright.azureedge.net",
    "playwright-akamai.azureedge.net",
    "playwright-verizon.azureedge.net",
];

const DEFAULT_CACHE_PATHS: &[&str] = &["~/.cache/node-gyp", "~/.cache/ms-playwright"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedKind {
    /// The egress proxy refused a host.
    Host,
    /// A binary is missing from PATH.
    Binary,
    /// A toolchain is not installed.
    Toolchain,
    /// A cache directory (browser build, node headers) is not there.
    Cache,
}

impl NeedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedKind::Host => "host",
            NeedKind::Binary => "binary",
            NeedKind::Toolchain => "toolchain",
            NeedKind::Cache => "cache",
        }
    }
}

/// What a failure said it was missing: the kind, the host, binary, toolchain
/// or path, and the line of output that said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    pub kind: NeedKind,
    pub target: String,
    pub evidence: String,
}

/// The first environment need in `text`, or nothing. Lines are read in
/// order; a proxy refusal outranks the rest because it is unambiguous.
pub fn recognize(text: &str) -> Option<Need> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find_map(|l| refused_host(l, false))
        .or_else(|| lines.iter().find_map(|l| refused_host(l, true)))
        .or_else(|| lines.iter().find_map(|l| missing_cache(l)))
        .or_else(|| lines.iter().find_map(|l| missing_toolchain(l)))
        .or_else(|| lines.iter().find_map(|l| missing_binary(l)))
}

fn need(kind: NeedKind, target: &str, line: &str) -> Need {
    Need {
        kind,
        target: target.to_string(),
        evidence: line.chars().take(300).collect(),
    }
}

/// The proxy's own body (`forge egress: HOST:PORT is not allowed.`), or a
/// tool's line naming a 403 and a URL (`403 Forbidden - GET https://HOST/..`).
fn refused_host(line: &str, by_url: bool) -> Option<Need> {
    if !by_url {
        if let Some(rest) = line.split("forge egress: ").nth(1)
            && let Some(authority) = rest.split(" is not allowed").next()
            && authority.len() < rest.len()
        {
            let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
            if valid_host(host) || valid_wildcard(host) {
                return Some(need(NeedKind::Host, &host.to_ascii_lowercase(), line));
            }
        }
        return None;
    }
    if line.contains("403") {
        for scheme in ["https://", "http://"] {
            if let Some(rest) = line.split(scheme).nth(1) {
                let authority = rest
                    .split(['/', ' ', '"', '\'', ')', '>'])
                    .next()
                    .unwrap_or("");
                let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
                if valid_host(host) {
                    return Some(need(NeedKind::Host, &host.to_ascii_lowercase(), line));
                }
            }
        }
    }
    None
}

/// A refusal can name a pattern (`*.example.org`); it is typed as a host
/// need so the ceiling can refuse it by name rather than the line going
/// unrecognized.
fn valid_wildcard(h: &str) -> bool {
    h.strip_prefix("*.").is_some_and(valid_host)
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.contains('.')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// A path under a well-known host cache that is not there: Playwright's
/// `Executable doesn't exist at /home/u/.cache/ms-playwright/...`, node-gyp
/// missing its headers directory.
fn missing_cache(line: &str) -> Option<Need> {
    let missing = [
        "doesn't exist",
        "does not exist",
        "ENOENT",
        "No such file",
        "not found",
    ]
    .iter()
    .any(|m| line.contains(m));
    if !missing {
        return None;
    }
    for word in line.split(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '`') {
        if word.starts_with('/') && word.contains("/.cache/") {
            let path = word.trim_end_matches([',', ':', ';', '.']);
            return Some(need(NeedKind::Cache, path, line));
        }
    }
    None
}

/// `toolchain 'stable-x86_64-unknown-linux-gnu' is not installed`.
fn missing_toolchain(line: &str) -> Option<Need> {
    let rest = line.split("toolchain '").nth(1)?;
    let (name, after) = rest.split_once('\'')?;
    (after.contains("not installed") || after.contains("is not installed"))
        .then(|| need(NeedKind::Toolchain, name, line))
}

/// `bash: cargo-nextest: command not found`, `sh: 1: tsc: not found`,
/// `exec: "x": executable file not found in $PATH`, `x not found in PATH`.
fn missing_binary(line: &str) -> Option<Need> {
    let clean = |s: &str| {
        s.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ':' || c.is_whitespace())
            .to_string()
    };
    let name = if let Some(before) = line.strip_suffix("command not found") {
        clean(before.rsplit(':').nth(1).unwrap_or(before))
    } else if let Some(before) = line.strip_suffix(": not found") {
        clean(before.rsplit(':').next().unwrap_or(before))
    } else if let Some(before) = line.split(": executable file not found").next()
        && before.len() < line.len()
    {
        clean(before.rsplit(':').next().unwrap_or(before))
    } else if let Some(before) = line.strip_suffix(" not found in PATH") {
        clean(before.rsplit(' ').next().unwrap_or(before))
    } else {
        return None;
    };
    (!name.is_empty() && !name.contains(char::is_whitespace))
        .then(|| need(NeedKind::Binary, &name, line))
}

/// What the kernel applies for a covered need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grant {
    /// The host, added to the worktree's egress.
    Host(String),
    /// The host path, bound read-only into the sandbox.
    ReadOnly(PathBuf),
}

impl Grant {
    pub fn describe(&self) -> String {
        match self {
            Grant::Host(h) => format!("egress to {h}"),
            Grant::ReadOnly(p) => format!("{} read-only", p.display()),
        }
    }
}

/// `[environment]`: hosts a repository may be granted automatically and
/// host cache paths that may be mounted read-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub hosts: Vec<String>,
    pub cache_paths: Vec<PathBuf>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            hosts: DEFAULT_HOSTS.iter().map(|h| h.to_string()).collect(),
            cache_paths: DEFAULT_CACHE_PATHS.iter().map(|p| expand_home(p)).collect(),
        }
    }
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

impl Policy {
    /// The policy from the operator's `[environment]` keys; a key left out
    /// keeps its default, an empty list grants nothing of that kind.
    pub fn build(hosts: Option<Vec<String>>, cache_paths: Option<Vec<PathBuf>>) -> Result<Policy> {
        let mut p = Policy::default();
        if let Some(hosts) = hosts {
            for h in &hosts {
                crate::egress::Rule::parse(h)
                    .map_err(|e| e.context(format!("environment.hosts: {h}")))?;
            }
            p.hosts = hosts
                .iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .collect();
        }
        if let Some(paths) = cache_paths {
            if let Some(bad) = paths.iter().find(|p| !p.is_absolute()) {
                anyhow::bail!("environment.cache_paths: {} is not absolute", bad.display());
            }
            p.cache_paths = paths;
        }
        Ok(p)
    }

    /// The table as the supervisor reads it, one line.
    pub fn describe(&self) -> String {
        format!(
            "hosts: {}; cache paths (read-only): {}",
            self.hosts.join(", "),
            self.cache_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// What may be granted for `need`, or nothing when the table does not
    /// cover it: a host the table lists (`*.suffix` entries match below the
    /// suffix), or a missing path under a listed cache. Binaries and
    /// toolchains are never granted here.
    pub fn covers(&self, need: &Need) -> Option<Grant> {
        match need.kind {
            NeedKind::Host => (!need.target.contains('*'))
                .then(|| {
                    self.hosts
                        .iter()
                        .any(|h| host_matches(h, &need.target))
                })
                .unwrap_or(false)
                .then(|| Grant::Host(need.target.clone())),
            NeedKind::Cache => {
                let path = Path::new(&need.target);
                self.cache_paths
                    .iter()
                    .find(|c| path.starts_with(c))
                    .map(|c| Grant::ReadOnly(c.clone()))
            }
            NeedKind::Binary | NeedKind::Toolchain => None,
        }
    }
}

fn host_matches(entry: &str, host: &str) -> bool {
    match entry.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() && host.ends_with(&format!(".{suffix}")),
        None => entry == host,
    }
}

/// The most the supervisor may approve for `need`, or why nothing: one
/// named host (never a wildcard, never github.com, never a model endpoint,
/// never what the repository's `[environment] deny` lists), or the one
/// directory under `~/.cache` the need's path is in, read-only. Pure code;
/// the supervisor's ruling is checked against this, never the other way.
pub fn within_ceiling(
    need: &Need,
    deny: &[String],
    models: &[crate::egress::Rule],
) -> Result<Grant, String> {
    match need.kind {
        NeedKind::Host => host_ceiling(&need.target, deny, models),
        NeedKind::Cache => cache_ceiling(&need.target, deny),
        NeedKind::Binary | NeedKind::Toolchain => Err(format!(
            "a {} is never granted here",
            need.kind.as_str()
        )),
    }
}

fn host_ceiling(host: &str, deny: &[String], models: &[crate::egress::Rule]) -> Result<Grant, String> {
    if host.contains('*') {
        return Err(format!(
            "{host} is a wildcard; the most that may be granted is one named host"
        ));
    }
    if !valid_host(host) {
        return Err(format!("{host} is not a host name"));
    }
    if host == "github.com" || host.ends_with(".github.com") {
        return Err("github.com is never granted".into());
    }
    if models.iter().any(|r| r.matches(host, 443).is_some()) {
        return Err(format!("{host} is a model endpoint"));
    }
    if deny.iter().any(|d| host_matches(&d.to_ascii_lowercase(), host)) {
        return Err(format!("the repository's forge.toml [environment] deny lists {host}"));
    }
    Ok(Grant::Host(host.to_string()))
}

fn cache_ceiling(target: &str, deny: &[String]) -> Result<Grant, String> {
    let path = Path::new(target);
    let cache = expand_home("~/.cache");
    if !cache.is_absolute() {
        return Err("no home directory to find ~/.cache in".into());
    }
    let Some(first) = path
        .strip_prefix(&cache)
        .ok()
        .filter(|_| !target.contains(".."))
        .and_then(|rest| rest.components().next())
    else {
        return Err(format!("{target} is not under {}", cache.display()));
    };
    let dir = cache.join(first);
    if let Some(d) = deny
        .iter()
        .map(|d| expand_home(d))
        .find(|d| dir.starts_with(d) || d.starts_with(&dir))
    {
        return Err(format!(
            "the repository's forge.toml [environment] deny lists {}",
            d.display()
        ));
    }
    Ok(Grant::ReadOnly(dir))
}

/// Record an applied grant as a decision row by `by` on `task_id`, naming
/// the need and its evidence; `who` says what allowed it.
pub fn record(
    store: &Store,
    task_id: i64,
    repo: &str,
    need: &Need,
    grant: &Grant,
    by: &Approval,
) -> Result<i64> {
    let question = format!(
        "Environment need: {} {}. Evidence: {}",
        need.kind.as_str(),
        need.target,
        need.evidence
    );
    let answer = match by {
        Approval::Policy => format!(
            "Granted {} for this worktree by the [environment] policy; re-ran without counting a retry.",
            grant.describe()
        ),
        Approval::Supervisor(why) => format!(
            "Granted {} for this worktree on the supervisor's approval ({why}); re-ran without counting a retry.",
            grant.describe()
        ),
    };
    let id = store.insert_decision_by(
        task_id,
        repo,
        &question,
        &answer,
        by.answered_by(),
        &need.target,
        None,
    )?;
    store.set_decision_kind(id, DECISION_KIND)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY: &str = "npm error code E403\nnpm error 403 403 Forbidden - GET https://registry.npmjs.org/left-pad\nforge egress: nodejs.org:443 is not allowed. This attempt may reach only: api.anthropic.com.\n";

    #[test]
    fn a_proxy_refusal_names_the_host() {
        let n = recognize(PROXY).unwrap();
        assert_eq!(n.kind, NeedKind::Host);
        assert_eq!(n.target, "nodejs.org");
        assert!(n.evidence.starts_with("forge egress: nodejs.org:443"));
    }

    #[test]
    fn a_tools_403_line_with_a_url_names_the_host() {
        let n = recognize("gyp http GET https://nodejs.org/dist/v20/node-headers.tar.gz\ngyp ERR! 403 Forbidden https://nodejs.org/dist/v20/x.tar.gz").unwrap();
        assert_eq!((n.kind, n.target.as_str()), (NeedKind::Host, "nodejs.org"));
        assert!(recognize("GET https://example.com/ returned 200").is_none());
    }

    #[test]
    fn a_missing_browser_cache_names_the_path() {
        let n = recognize("browserType.launch: Executable doesn't exist at /home/u/.cache/ms-playwright/chromium-1140/chrome-linux/chrome").unwrap();
        assert_eq!(n.kind, NeedKind::Cache);
        assert_eq!(
            n.target,
            "/home/u/.cache/ms-playwright/chromium-1140/chrome-linux/chrome"
        );
    }

    #[test]
    fn a_missing_binary_or_toolchain_is_typed() {
        let n = recognize("bash: line 1: cargo-nextest: command not found").unwrap();
        assert_eq!(
            (n.kind, n.target.as_str()),
            (NeedKind::Binary, "cargo-nextest")
        );
        let n = recognize("sh: 1: tsc: not found").unwrap();
        assert_eq!((n.kind, n.target.as_str()), (NeedKind::Binary, "tsc"));
        let n = recognize("error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed")
            .unwrap();
        assert_eq!(n.kind, NeedKind::Toolchain);
        assert_eq!(n.target, "nightly-x86_64-unknown-linux-gnu");
        let n = recognize("error: no such file: forge-repomap not found in PATH").unwrap();
        assert_eq!(
            (n.kind, n.target.as_str()),
            (NeedKind::Binary, "forge-repomap")
        );
    }

    #[test]
    fn ordinary_failures_and_questions_are_nothing() {
        assert!(recognize("error[E0308]: mismatched types\n  --> src/lib.rs:3:5").is_none());
        assert!(recognize("Which of the two endpoints should the client call?").is_none());
        assert!(recognize("").is_none());
    }

    #[test]
    fn a_question_is_read_the_same_way() {
        let q = "The build fails: forge egress: static.crates.io:443 is not allowed. Should I add it to forge.toml?";
        assert_eq!(recognize(q).unwrap().target, "static.crates.io");
    }

    fn policy() -> Policy {
        Policy::build(None, Some(vec![PathBuf::from("/h/.cache/ms-playwright")])).unwrap()
    }

    #[test]
    fn the_default_table_covers_the_registries_and_the_playwright_cdn() {
        let p = policy();
        for h in [
            "registry.npmjs.org",
            "index.crates.io",
            "static.crates.io",
            "nodejs.org",
            "cdn.playwright.dev",
        ] {
            let n = Need {
                kind: NeedKind::Host,
                target: h.into(),
                evidence: String::new(),
            };
            assert_eq!(p.covers(&n), Some(Grant::Host(h.into())), "{h}");
        }
        let n = Need {
            kind: NeedKind::Host,
            target: "evil.example".into(),
            evidence: String::new(),
        };
        assert_eq!(p.covers(&n), None);
    }

    #[test]
    fn a_cache_is_covered_only_under_a_listed_path() {
        let p = policy();
        let n = |t: &str| Need {
            kind: NeedKind::Cache,
            target: t.into(),
            evidence: String::new(),
        };
        assert_eq!(
            p.covers(&n("/h/.cache/ms-playwright/chromium-1/chrome")),
            Some(Grant::ReadOnly(PathBuf::from("/h/.cache/ms-playwright")))
        );
        assert_eq!(p.covers(&n("/h/.cache/other/x")), None);
        assert_eq!(p.covers(&n("/h/.cache/ms-playwright-evil/x")), None);
    }

    #[test]
    fn a_binary_is_never_granted_and_the_operator_can_narrow_the_table() {
        let b = Need {
            kind: NeedKind::Binary,
            target: "tsc".into(),
            evidence: String::new(),
        };
        assert_eq!(policy().covers(&b), None);
        let p = Policy::build(Some(vec!["*.example.org".into()]), Some(vec![])).unwrap();
        let h = |t: &str| Need {
            kind: NeedKind::Host,
            target: t.into(),
            evidence: String::new(),
        };
        assert!(p.covers(&h("a.example.org")).is_some());
        assert!(p.covers(&h("example.org")).is_none());
        assert!(p.covers(&h("nodejs.org")).is_none());
        assert!(Policy::build(Some(vec!["not a host".into()]), None).is_err());
    }
}
