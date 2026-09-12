//! The result contract. The CLI is given this JSON schema, so the agent's
//! final result is structured, never parsed out of prose: what it changed,
//! which checks it ran and how they went, what it claims with evidence, and
//! whether it needs the operator. Everything in it is a claim; verification
//! compares it with what Forge measured.

use serde::{Deserialize, Serialize};

/// Ported verbatim from Forge 1 (VERIFICATION.md, worker/verify.go).
pub const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["schema_version","summary","needs_input","changes","checks_run","claims"],"properties":{"schema_version":{"type":"integer"},"summary":{"type":"string"},"needs_input":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"required":["question","tried"],"properties":{"question":{"type":"string"},"tried":{"type":"string","description":"what you did before stopping, and where you stopped"},"kind":{"type":"string","enum":["question","workflow","review"]},"options":{"type":"array","items":{"type":"string"}},"context":{"type":"string"},"checkpoint":{"type":["string","null"]}}}]},"changes":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["path","kind"],"properties":{"path":{"type":"string"},"kind":{"type":"string","enum":["added","modified","deleted"]},"summary":{"type":"string"}}}},"checks_run":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["check","passed"],"properties":{"check":{"type":"string"},"passed":{"type":"boolean"},"notes":{"type":"string"}}}},"claims":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["claim","evidence"],"properties":{"claim":{"type":"string"},"evidence":{"type":"string"}}}}}}"#;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Envelope {
    pub schema_version: i64,
    pub summary: String,
    pub needs_input: Option<NeedsInput>,
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default)]
    pub checks_run: Vec<CheckRun>,
    #[serde(default)]
    pub claims: Vec<Claim>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NeedsInput {
    pub question: String,
    /// What the agent did before stopping and where it stopped: the
    /// anti-stop-early guard, and the first thing a human reads.
    #[serde(default)]
    pub tried: String,
    /// "question" (default): the operator must answer. "workflow": the
    /// workflow given is wrong for the task or a needed step does not exist.
    /// "review": a reviewer demotes the task to human review.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub context: String,
    pub checkpoint: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Change {
    pub path: String,
    pub kind: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CheckRun {
    pub check: String,
    pub passed: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Claim {
    pub claim: String,
    #[serde(default)]
    pub evidence: String,
}

/// `Ok(None)` when there is no structured result at all; `Err` when there is
/// one and it does not fit the contract.
pub fn parse(structured: Option<&str>, result_text: &str) -> Result<Option<Envelope>, String> {
    let raw = match structured {
        Some(s) if s != "null" && !s.trim().is_empty() => s,
        _ if result_text.trim_start().starts_with('{') => result_text,
        _ => return Ok(None),
    };
    serde_json::from_str::<Envelope>(raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_valid_json_and_parses_its_own_example() {
        serde_json::from_str::<serde_json::Value>(SCHEMA).unwrap();
        let env = parse(
            Some(r#"{"schema_version":1,"summary":"did it","needs_input":null,"changes":[{"path":"a.txt","kind":"added"}],"checks_run":[{"check":"test","passed":true}],"claims":[{"claim":"x","evidence":"y"}]}"#),
            "",
        )
        .unwrap()
        .unwrap();
        assert_eq!(env.changes[0].path, "a.txt");
        assert!(env.checks_run[0].passed);
    }

    #[test]
    fn falls_back_to_json_result_text_and_reports_absence() {
        assert!(parse(None, "all done").unwrap().is_none());
        assert!(parse(Some("null"), "all done").unwrap().is_none());
        let env = parse(None, r#"{"schema_version":1,"summary":"s","needs_input":null,"changes":[],"checks_run":[],"claims":[]}"#).unwrap();
        assert!(env.is_some());
        assert!(parse(Some(r#"{"nope":1}"#), "").is_err());
    }
}
