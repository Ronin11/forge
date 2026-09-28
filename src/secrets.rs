//! What a run workflow's operation step may be given beyond the cleared
//! environment (docs/JOBS.md, "Secrets and egress on a step"): named
//! secrets and declared egress hosts, and the redaction that keeps a
//! secret's value out of everything the step leaves behind.
//!
//! `[secrets]` in the operator's config maps a name to the environment
//! variable the worker already has, never to a value. A step declares
//! `secrets = [...]` and `egress = [...]`; `step_grant` turns that into the
//! environment and egress policy of that step alone, and only at operator
//! trust: a contact or public job that reaches a step asking for either is
//! refused, so a payload nobody vouched for can never spend the operator's
//! credentials. Values are read from the worker's environment when the step
//! starts and never stored; a `Redactor` built from them scrubs the step's
//! output, effect log and error text before any of it is recorded.

use crate::config::TrustEgress;
use crate::egress::{Policy, Rule};
use crate::store::Trust;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

/// What a redacted value is replaced with, naming the secret and never
/// its value.
fn placeholder(name: &str) -> String {
    format!("[redacted:{name}]")
}

/// A `[secrets]` name: lowercase letters, digits, `_` and `-`.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !ok {
        bail!("{name:?} is not a secret name (lowercase letters, digits, `_` and `-`)");
    }
    Ok(())
}

/// An environment variable name a secret may map to. Names that begin
/// `FORGE` are the kernel's own and cannot be handed to a step under a
/// secret's name.
pub fn check_env_var(var: &str) -> Result<()> {
    let mut chars = var.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!("{var:?} is not an environment variable name");
    }
    if var.to_ascii_uppercase().starts_with("FORGE") {
        bail!("{var:?} is one of the kernel's own variables; a secret cannot map to it");
    }
    Ok(())
}

/// One `[secrets]` entry as written: `{ env = "CLOUDFLARE_API_TOKEN" }`. A
/// bare string is read as the attempt to write the value down, and refused
/// by `build` with a message that says so.
#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Env { env: String },
    Value(#[allow(dead_code)] String),
}

/// `[secrets]`, validated: each name mapped to an environment variable name.
pub fn build(raw: BTreeMap<String, Entry>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (name, entry) in raw {
        let Entry::Env { env } = entry else {
            bail!(
                "secrets.{name}: write {{ env = \"VARIABLE\" }}, the environment variable the \
                 worker already has; a secret is never written into config.toml"
            );
        };
        check_name(&name).with_context(|| format!("secrets.{name}"))?;
        check_env_var(&env).with_context(|| format!("secrets.{name}.env"))?;
        out.insert(name, env);
    }
    Ok(out)
}

/// A secret resolved for one step: the name it is declared under, the
/// variable the step sees it as, and the value.
#[derive(Clone)]
pub struct Secret {
    pub name: String,
    pub env_var: String,
    value: String,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("name", &self.name)
            .field("env_var", &self.env_var)
            .finish_non_exhaustive()
    }
}

impl Secret {
    #[cfg(test)]
    pub fn new(name: &str, env_var: &str, value: &str) -> Self {
        Secret {
            name: name.into(),
            env_var: env_var.into(),
            value: value.into(),
        }
    }
}

/// Resolve the secrets a step declares. `table` is `[secrets]` (name to
/// variable) and `lookup` reads a variable from the worker's environment.
/// A name `[secrets]` does not define, or a variable the worker does not
/// have (or has empty), is an error naming the secret, never a step that
/// runs without what it asked for.
pub fn resolve(
    declared: &[String],
    table: &BTreeMap<String, String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Secret>> {
    let mut out: Vec<Secret> = Vec::new();
    for name in declared {
        if out.iter().any(|s| &s.name == name) {
            continue;
        }
        let var = table.get(name).with_context(|| {
            format!(
                "the step asks for secret {name:?}, which [secrets] in config.toml does not define"
            )
        })?;
        let value = lookup(var).filter(|v| !v.is_empty()).with_context(|| {
            format!("secret {name:?} maps to {var}, which is not set in the worker's environment")
        })?;
        out.push(Secret {
            name: name.clone(),
            env_var: var.clone(),
            value,
        });
    }
    Ok(out)
}

/// Replaces every resolved value with a placeholder naming its secret. The
/// one redaction every place a step's output lands goes through: the step
/// row's tail, the output file, the verdict, the effect log, the error
/// text.
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    /// `(needle, secret name)`, longest needle first so a value that
    /// contains another is replaced whole.
    needles: Vec<(String, String)>,
}

impl Redactor {
    pub fn new(secrets: &[Secret]) -> Self {
        let mut needles: Vec<(String, String)> = Vec::new();
        for s in secrets.iter().filter(|s| !s.value.is_empty()) {
            needles.push((s.value.clone(), s.name.clone()));
            // The same value as JSON escapes it, for output that is JSON.
            let json = serde_json::to_string(&s.value).unwrap_or_default();
            let json = json.trim_matches('"');
            if !json.is_empty() && json != s.value {
                needles.push((json.to_string(), s.name.clone()));
            }
        }
        needles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.cmp(b)));
        needles.dedup();
        Redactor { needles }
    }

    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, name) in &self.needles {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), &placeholder(name));
            }
        }
        out
    }

    /// A check result with every text field redacted.
    pub fn result(&self, mut r: crate::checks::CheckResult) -> crate::checks::CheckResult {
        if self.is_empty() {
            return r;
        }
        r.tail = self.redact(&r.tail);
        r.stdout = self.redact(&r.stdout);
        r.failing_tests = r.failing_tests.iter().map(|t| self.redact(t)).collect();
        r
    }
}

/// What a step declared: the `secrets` and `egress` of its workflow entry.
#[derive(Clone, Copy, Debug, Default)]
pub struct Declared<'a> {
    pub secrets: &'a [String],
    pub egress: &'a [String],
}

impl Declared<'_> {
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty() && self.egress.is_empty()
    }
}

/// What one step is given on top of the cleared environment.
#[derive(Debug, Default)]
pub struct StepGrant {
    /// The declared secrets, as `(variable, value)`: added to the step's
    /// environment and to nothing else.
    pub env: Vec<(String, String)>,
    /// The step's egress policy: the declared hosts alone. Nothing a
    /// neighbouring step, the repository or the model allowlist opens is in
    /// it, and it exists for this step only.
    pub egress: Policy,
    pub redactor: Redactor,
}

/// The grant for one operation step, or an error saying why the step may
/// not run. Nothing is granted at any level but the operator's, and hosts
/// only when that level's own `[trust.<level>] egress` is `declared` (the
/// same gate `Forge::apply_grant` applies to an environment grant).
pub fn step_grant(
    step: &str,
    declared: Declared<'_>,
    trust: Trust,
    trust_egress: TrustEgress,
    table: &BTreeMap<String, String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<StepGrant> {
    if declared.is_empty() {
        return Ok(StepGrant::default());
    }
    if trust != Trust::Operator {
        bail!(
            "step {step:?} declares secrets or egress hosts, which only an operator-trust job may \
             be given; this job is {}",
            trust.as_str()
        );
    }
    if !declared.egress.is_empty() && trust_egress != TrustEgress::Declared {
        bail!(
            "step {step:?} declares egress hosts, but [trust.operator] egress is {:?}, which \
             opens none",
            trust_egress.as_str()
        );
    }
    let rules = declared
        .egress
        .iter()
        .map(|h| Rule::parse(h).with_context(|| format!("step {step:?}: egress")))
        .collect::<Result<Vec<_>>>()?;
    let secrets =
        resolve(declared.secrets, table, lookup).with_context(|| format!("step {step:?}"))?;
    Ok(StepGrant {
        env: secrets
            .iter()
            .map(|s| (s.env_var.clone(), s.value.clone()))
            .collect(),
        egress: Policy::new(rules),
        redactor: Redactor::new(&secrets),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> BTreeMap<String, String> {
        [
            ("cloudflare_token", "CLOUDFLARE_API_TOKEN"),
            ("other", "OTHER_KEY"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn env(name: &str) -> Option<String> {
        match name {
            "CLOUDFLARE_API_TOKEN" => Some("cf-s3cret-value".into()),
            "OTHER_KEY" => Some("other-value".into()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_declared_secret_resolves_to_the_workers_variable() {
        let got = resolve(&strings(&["cloudflare_token"]), &table(), env).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "cloudflare_token");
        assert_eq!(got[0].env_var, "CLOUDFLARE_API_TOKEN");
        assert_eq!(got[0].value, "cf-s3cret-value");
    }

    #[test]
    fn only_what_a_step_declares_is_resolved_and_a_repeat_counts_once() {
        let got = resolve(&strings(&["other", "other"]), &table(), env).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "other");
    }

    #[test]
    fn an_undefined_secret_or_an_unset_variable_is_an_error_naming_the_secret() {
        let e = resolve(&strings(&["nope"]), &table(), env).unwrap_err();
        assert!(format!("{e:#}").contains("\"nope\""), "{e:#}");
        let e = resolve(&strings(&["cloudflare_token"]), &table(), |_| None).unwrap_err();
        let text = format!("{e:#}");
        assert!(text.contains("cloudflare_token") && text.contains("CLOUDFLARE_API_TOKEN"));
        let mut t = table();
        t.insert("blank".into(), "EMPTY".into());
        assert!(resolve(&strings(&["blank"]), &t, env).is_err());
    }

    #[test]
    fn a_debug_print_of_a_secret_never_shows_the_value() {
        let s = Secret::new("t", "T", "hunter2-hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
    }

    #[test]
    fn names_and_variables_are_checked() {
        assert!(check_name("cloudflare_token").is_ok());
        assert!(check_name("Cloudflare").is_err());
        assert!(check_name("").is_err());
        assert!(check_env_var("CLOUDFLARE_API_TOKEN").is_ok());
        assert!(check_env_var("1BAD").is_err());
        assert!(check_env_var("has space").is_err());
        assert!(check_env_var("FORGE_HOME").is_err());
    }

    #[test]
    fn redaction_replaces_every_value_and_names_the_secret() {
        let r = Redactor::new(&[Secret::new("t", "T", "abc-123-xyz")]);
        assert_eq!(
            r.redact("auth abc-123-xyz and again abc-123-xyz."),
            "auth [redacted:t] and again [redacted:t]."
        );
        assert_eq!(r.redact("nothing here"), "nothing here");
    }

    #[test]
    fn redaction_catches_the_json_escaped_form_and_the_longer_value_first() {
        let r = Redactor::new(&[
            Secret::new("short", "S", "abcd"),
            Secret::new("long", "L", "abcd\"efgh"),
        ]);
        assert_eq!(r.redact("x abcd\"efgh y"), "x [redacted:long] y");
        assert_eq!(
            r.redact(r#"{"k":"abcd\"efgh"}"#),
            r#"{"k":"[redacted:long]"}"#
        );
    }

    #[test]
    fn redaction_scrubs_every_text_field_of_a_result() {
        let r = Redactor::new(&[Secret::new("t", "T", "s3cr3t-value")]);
        let got = r.result(crate::checks::CheckResult {
            tail: "tail s3cr3t-value".into(),
            stdout: "out s3cr3t-value".into(),
            failing_tests: vec!["test_s3cr3t-value".into()],
            ..Default::default()
        });
        assert_eq!(got.tail, "tail [redacted:t]");
        assert_eq!(got.stdout, "out [redacted:t]");
        assert_eq!(got.failing_tests, vec!["test_[redacted:t]".to_string()]);
    }

    #[test]
    fn an_empty_redactor_leaves_text_alone() {
        let r = Redactor::default();
        assert!(r.is_empty());
        assert_eq!(r.redact("anything"), "anything");
    }

    fn grant(trust: Trust, egress: TrustEgress, d: Declared<'_>) -> Result<StepGrant> {
        step_grant("play", d, trust, egress, &table(), env)
    }

    #[test]
    fn a_step_declaring_nothing_is_granted_nothing_at_any_trust() {
        for trust in [Trust::Operator, Trust::Contact, Trust::Public] {
            let g = grant(trust, TrustEgress::Model, Declared::default()).unwrap();
            assert!(g.env.is_empty() && g.egress.rules().is_empty() && g.redactor.is_empty());
        }
    }

    #[test]
    fn an_operator_step_gets_exactly_its_declared_secrets_and_hosts() {
        let secrets = strings(&["cloudflare_token"]);
        let hosts = strings(&["api.cloudflare.com"]);
        let g = grant(
            Trust::Operator,
            TrustEgress::Declared,
            Declared {
                secrets: &secrets,
                egress: &hosts,
            },
        )
        .unwrap();
        assert_eq!(
            g.env,
            vec![(
                "CLOUDFLARE_API_TOKEN".to_string(),
                "cf-s3cret-value".to_string()
            )]
        );
        let rules: Vec<String> = g.egress.rules().iter().map(|r| r.to_string()).collect();
        assert_eq!(rules, vec!["api.cloudflare.com"]);
        assert_eq!(
            g.redactor.redact("cf-s3cret-value"),
            "[redacted:cloudflare_token]"
        );
    }

    #[test]
    fn a_contact_or_public_step_asking_for_either_is_refused() {
        let secrets = strings(&["cloudflare_token"]);
        let hosts = strings(&["api.cloudflare.com"]);
        for trust in [Trust::Contact, Trust::Public] {
            for d in [
                Declared {
                    secrets: &secrets,
                    egress: &[],
                },
                Declared {
                    secrets: &[],
                    egress: &hosts,
                },
            ] {
                let e = grant(trust, TrustEgress::Declared, d).unwrap_err();
                assert!(format!("{e:#}").contains("operator-trust"), "{e:#}");
            }
        }
    }

    #[test]
    fn hosts_are_refused_where_the_operator_level_opens_none() {
        let hosts = strings(&["api.cloudflare.com"]);
        let d = Declared {
            secrets: &[],
            egress: &hosts,
        };
        assert!(grant(Trust::Operator, TrustEgress::Model, d).is_err());
    }

    #[test]
    fn a_bad_host_or_an_unknown_secret_refuses_the_step() {
        let bad = strings(&["https://api.cloudflare.com/"]);
        let d = Declared {
            secrets: &[],
            egress: &bad,
        };
        assert!(grant(Trust::Operator, TrustEgress::Declared, d).is_err());
        let unknown = strings(&["nope"]);
        let d = Declared {
            secrets: &unknown,
            egress: &[],
        };
        assert!(grant(Trust::Operator, TrustEgress::Declared, d).is_err());
    }
}
