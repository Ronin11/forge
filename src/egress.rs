//! Bounded network egress for an attempt. The sandbox gives an attempt no
//! network of its own; the only route out is a proxy on the host that
//! answers for a list of rules, and this module is both the list (`Rule`)
//! and the proxy. A repository declares its rules in forge.toml
//! (`[sandbox] egress = ["registry.npmjs.org", "*.crates.io:443"]`), the
//! model endpoint is always allowed on top, and everything else is refused
//! with a 403 that names the host.

use anyhow::{Result, bail};
use std::fmt;

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
}
