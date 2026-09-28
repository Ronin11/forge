//! What a turn records about the tools it used (`chat_turns.tool_calls`):
//! each call with its arguments and what it returned, and for a write
//! verb the proposal the operator has yet to decide.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A write verb's proposal and where the operator has left it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Proposal {
    /// What confirming would do, in a sentence the operator reads.
    pub summary: String,
    pub status: Status,
    /// What happened once confirmed: the task filed, the answer taken, or
    /// why it failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Proposed,
    /// Confirmed and running or done; `Proposal::outcome` says which.
    Confirmed,
    Rejected,
    /// Confirmed, and the action itself failed.
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Confirmed => "confirmed",
            Status::Rejected => "rejected",
            Status::Failed => "failed",
        }
    }
}

/// One tool call in a turn.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub tool: String,
    pub arguments: Value,
    /// The model's own sentence about why it made the call.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// What a read tool returned, after redaction and bounding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why the call failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Proposal>,
}

impl ToolCall {
    pub fn read(tool: &str, arguments: Value, note: &str, result: Value) -> ToolCall {
        ToolCall {
            tool: tool.into(),
            arguments,
            note: note.into(),
            result: Some(result),
            error: None,
            proposal: None,
        }
    }

    pub fn failed(tool: &str, arguments: Value, note: &str, error: String) -> ToolCall {
        ToolCall {
            tool: tool.into(),
            arguments,
            note: note.into(),
            result: None,
            error: Some(error),
            proposal: None,
        }
    }

    pub fn proposed(tool: &str, arguments: Value, note: &str, summary: String) -> ToolCall {
        ToolCall {
            tool: tool.into(),
            arguments,
            note: note.into(),
            result: None,
            error: None,
            proposal: Some(Proposal {
                summary,
                status: Status::Proposed,
                outcome: String::new(),
                decided_at: None,
            }),
        }
    }
}

/// A turn's `tool_calls` column as calls; an unreadable column is none.
pub fn parse_calls(json: &str) -> Vec<ToolCall> {
    serde_json::from_str(json).unwrap_or_default()
}

pub fn calls_json(calls: &[ToolCall]) -> String {
    serde_json::to_string(calls).unwrap_or_else(|_| "[]".into())
}

/// The id the operator confirms an action by: its turn, then its position
/// among that turn's tool calls.
pub fn action_id(turn: i64, index: usize) -> String {
    format!("{turn}.{index}")
}

pub fn parse_action_id(id: &str) -> Option<(i64, usize)> {
    let (turn, index) = id.split_once('.')?;
    Some((turn.parse().ok()?, index.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_id_names_a_turn_and_a_position() {
        assert_eq!(action_id(12, 3), "12.3");
        assert_eq!(parse_action_id("12.3"), Some((12, 3)));
        for bad in ["12", "12.", ".3", "a.b", "12.3.4", "-1.x"] {
            assert_eq!(parse_action_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn calls_round_trip_and_an_unreadable_column_is_no_calls() {
        let calls = vec![
            ToolCall::read(
                "task",
                serde_json::json!({"id": 1}),
                "look",
                serde_json::json!({}),
            ),
            ToolCall::proposed(
                "retry_task",
                serde_json::json!({"task": 1}),
                "",
                "retry 1".into(),
            ),
        ];
        let back = parse_calls(&calls_json(&calls));
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].proposal.as_ref().unwrap().status, Status::Proposed);
        assert!(parse_calls("not json").is_empty());
    }
}
