//! No secrets in what the model sees or the record keeps. Every tool
//! result passes through here before it reaches the model, the session's
//! record, or the operator's screen: the values the operator has
//! configured as secrets are removed wherever they appear, and so is
//! anything shaped like a credential, the way an attempt's log is kept
//! clean of the environment it ran under.

pub const MASK: &str = "[redacted]";

/// Prefixes of the credentials that turn up in logs and command lines.
const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "glpat-",
];

/// A key that names a secret's value, matched inside the key.
const SECRET_KEYS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "private_key",
];

/// Secrets shorter than this are not masked by value: they would mangle
/// ordinary words.
const MIN_SECRET_LEN: usize = 6;

/// What to mask: the secret values Forge holds.
#[derive(Default, Clone)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Redactor {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .filter(|s| s.len() >= MIN_SECRET_LEN)
            .collect();
        // Longest first, so a secret that contains another is masked whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Redactor { secrets }
    }

    /// `text` with every secret value and credential-shaped word masked.
    pub fn text(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            out = out.replace(s.as_str(), MASK);
        }
        mask_words(&out)
    }

    /// `v` with every string masked as `text` does, and the value under a
    /// key that names a secret masked whole.
    pub fn value(&self, v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(items) => Value::Array(items.iter().map(|i| self.value(i)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let masked = names_a_secret(k) && !v.is_null() && !v.is_boolean();
                        let v = if masked {
                            Value::String(MASK.into())
                        } else {
                            self.value(v)
                        };
                        (k.clone(), v)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// Whether a key names a secret: `password`, `api_key`, `access_token`,
/// but not `input_tokens`.
fn names_a_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|s| k.contains(s)) || k == "token" || k.ends_with("_token")
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=' | ':' | '@' | '%')
}

/// Mask credential-shaped words: known token prefixes, JWTs, the word
/// after `Bearer`/`Basic`, `key=value` and `"key": "value"` pairs whose
/// key names a secret, and the password of a URL's userinfo.
fn mask_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut after_scheme = false;
    let flush = |word: &mut String, out: &mut String, after_scheme: &mut bool| {
        if word.is_empty() {
            return;
        }
        let masked = mask_word(word, *after_scheme);
        *after_scheme = matches!(word.as_str(), "Bearer" | "Basic" | "bearer" | "basic");
        out.push_str(&masked);
        word.clear();
    };
    for c in text.chars() {
        if is_word_char(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out, &mut after_scheme);
            out.push(c);
            // A quote or colon between a `"key"` and its value keeps the
            // pair together for `mask_pair`; whitespace does not end it.
        }
    }
    flush(&mut word, &mut out, &mut after_scheme);
    mask_json_pairs(&out)
}

fn mask_word(word: &str, after_scheme: bool) -> String {
    if after_scheme && word.len() >= 8 {
        return MASK.into();
    }
    if let Some((k, v)) = word.split_once(['=', ':'])
        && !v.is_empty()
        && !v.starts_with("//")
        && names_a_secret(k)
    {
        return format!("{k}{}{MASK}", &word[k.len()..k.len() + 1]);
    }
    if word.contains("://")
        && let Some(masked) = mask_userinfo(word)
    {
        return masked;
    }
    let token = word.trim_start_matches(['=', ':', '/']);
    let token = token.rsplit(['=', ':']).next().unwrap_or(token);
    let credential = (token.len() >= 12 && TOKEN_PREFIXES.iter().any(|p| token.starts_with(p)))
        || (token.starts_with("eyJ") && token.len() >= 30 && token.contains('.'));
    if credential {
        return word.replace(token, MASK);
    }
    word.to_string()
}

/// `scheme://user:password@host/...` with the password masked.
fn mask_userinfo(word: &str) -> Option<String> {
    let (scheme, rest) = word.split_once("://")?;
    let (userinfo, tail) = rest.split_once('@')?;
    let (user, password) = userinfo.split_once(':')?;
    if password.is_empty() || user.contains('/') {
        return None;
    }
    Some(format!("{scheme}://{user}:{MASK}@{tail}"))
}

/// `"password": "hunter2"` in JSON text (a log line): the value is masked
/// when the key names a secret.
fn mask_json_pairs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(q) = rest.find('"') {
        out.push_str(&rest[..=q]);
        rest = &rest[q + 1..];
        let Some(end) = rest.find('"') else { break };
        let key = &rest[..end];
        out.push_str(key);
        out.push('"');
        rest = &rest[end + 1..];
        let after = rest.trim_start();
        let Some(value) = after.strip_prefix(':').map(str::trim_start) else {
            continue;
        };
        if !names_a_secret(key) || !value.starts_with('"') {
            continue;
        }
        let skipped = rest.len() - value.len();
        out.push_str(&rest[..skipped]);
        // The value's closing quote, honoring backslash escapes.
        let body = &value[1..];
        let mut close = None;
        let mut escaped = false;
        for (i, c) in body.char_indices() {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => {
                    close = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(close) = close else { break };
        out.push('"');
        out.push_str(MASK);
        out.push('"');
        rest = &body[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r() -> Redactor {
        Redactor::new(["hunter2-the-secret".to_string(), "abc".to_string()])
    }

    #[test]
    fn a_configured_secret_is_masked_wherever_it_appears() {
        let out = r().text("curl -H x=hunter2-the-secret and hunter2-the-secret again");
        assert!(!out.contains("hunter2"), "{out}");
        assert_eq!(out.matches(MASK).count(), 2, "{out}");
    }

    #[test]
    fn a_secret_too_short_to_tell_from_a_word_is_left_alone() {
        assert_eq!(r().text("the abc of it"), "the abc of it");
    }

    #[test]
    fn credential_shaped_words_are_masked() {
        for leaked in [
            "sk-ant-api03-abcdefghijklmnop",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijk",
        ] {
            let out = Redactor::default().text(&format!("env: KEY={leaked} ok"));
            assert!(!out.contains(leaked), "{out}");
            assert!(out.contains("ok"), "{out}");
        }
    }

    #[test]
    fn a_bearer_credential_and_a_url_password_are_masked() {
        let out = Redactor::default()
            .text("Authorization: Bearer abcdef0123456789 via https://bob:swordfish@example.com/x");
        assert!(!out.contains("abcdef0123456789"), "{out}");
        assert!(!out.contains("swordfish"), "{out}");
        assert!(out.contains("bob"), "{out}");
        assert!(out.contains("@example.com/x"), "{out}");
    }

    #[test]
    fn key_value_pairs_and_json_pairs_naming_a_secret_are_masked() {
        let out = Redactor::default()
            .text(r#"password=hunter22 {"api_key": "zzzzzzzz", "name": "keep"} token: keepme"#);
        assert!(!out.contains("hunter22"), "{out}");
        assert!(!out.contains("zzzzzzzz"), "{out}");
        assert!(out.contains("keep"), "{out}");
    }

    #[test]
    fn a_json_value_under_a_secret_key_is_masked_and_token_counts_are_not() {
        let v = json!({
            "password": "p",
            "api_key": "k",
            "access_token": "t",
            "input_tokens": 1200,
            "nested": [{"secret": {"a": 1}}],
            "note": "fine",
        });
        let out = Redactor::default().value(&v);
        assert_eq!(out["password"], MASK);
        assert_eq!(out["api_key"], MASK);
        assert_eq!(out["access_token"], MASK);
        assert_eq!(out["nested"][0]["secret"], MASK);
        assert_eq!(out["input_tokens"], 1200);
        assert_eq!(out["note"], "fine");
    }

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        let t = "task 903 failed: test — cargo test exited 101 (see src/store/chat.rs:12)";
        assert_eq!(Redactor::default().text(t), t);
    }
}
