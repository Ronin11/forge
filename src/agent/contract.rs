//! The adapter contracts under `contracts/` read as data, and each runner's
//! golden transcript replayed through its real parser and held to the
//! numbers its contract records. The argv tests in each runner's module
//! take their always-flags from `always_args`, so a flag cannot change in
//! one place only.

use super::*;
use crate::config::EarlyEnding;
use std::path::PathBuf;

pub(super) const RUNNERS: [Runner; 5] = [
    Runner::ClaudeCli,
    Runner::CodexCli,
    Runner::CopilotCli,
    Runner::Chat,
    Runner::Jev,
];

fn text(runner: Runner) -> &'static str {
    match runner {
        Runner::ClaudeCli => include_str!("contracts/claude-cli.toml"),
        Runner::CodexCli => include_str!("contracts/codex-cli.toml"),
        Runner::CopilotCli => include_str!("contracts/copilot-cli.toml"),
        Runner::Chat => include_str!("contracts/chat.toml"),
        Runner::Jev => include_str!("contracts/jev.toml"),
    }
}

pub(super) fn load(runner: Runner) -> toml::Table {
    text(runner)
        .parse()
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", runner.contract_path()))
}

/// The argument groups the runner always passes, each in order: `["--model"]`
/// or `["--output-format", "json"]`.
pub(super) fn always_args(runner: Runner) -> Vec<Vec<String>> {
    load(runner)["always"]
        .as_array()
        .expect("always is an array of tables")
        .iter()
        .map(|a| {
            a["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

/// Asserts `argv` carries every always-group of the contract, its
/// arguments adjacent and in order, and none of the contract's `never` flags.
pub(super) fn assert_always(runner: Runner, argv: &[String]) {
    for group in always_args(runner) {
        assert!(
            argv.windows(group.len()).any(|w| w == group.as_slice()),
            "{}: {group:?} missing from {argv:?}",
            runner.as_str()
        );
    }
    if let Some(never) = load(runner).get("never") {
        for flag in never["flags"].as_array().unwrap() {
            let flag = flag.as_str().unwrap();
            assert!(
                !argv.iter().any(|a| a == flag),
                "{}: {flag} is never passed: {argv:?}",
                runner.as_str()
            );
        }
    }
}

/// The transcript's lines but its `forge_fixture` header, parsed.
fn transcript(runner: Runner) -> Vec<Value> {
    let c = load(runner);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/agent")
        .join(c["transcript"].as_str().unwrap());
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut lines = body
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap());
    let header = lines.next().unwrap();
    assert_eq!(
        header["type"], "forge_fixture",
        "{path:?} starts with its header"
    );
    assert_eq!(header["runner"], runner.as_str());
    assert_eq!(
        header["synthetic"].as_bool(),
        c["synthetic"].as_bool(),
        "{path:?}: header and contract agree on whether the transcript is synthetic"
    );
    if header["synthetic"] == true {
        assert!(
            header["note"].as_str().unwrap().starts_with("SYNTHETIC"),
            "{path:?}: a synthetic transcript says so in its header"
        );
    }
    lines.collect()
}

fn golden(runner: Runner) -> toml::Table {
    load(runner)["golden"].as_table().unwrap().clone()
}

fn thresholds() -> EarlyEnding {
    super::tests::thresholds(1000, 1000, 1000, 2)
}

/// What every contract states, whatever the runner.
#[test]
fn every_contract_states_its_flags_events_session_exit_codes_and_limits() {
    for runner in RUNNERS {
        let c = load(runner);
        let name = runner.as_str();
        assert_eq!(c["runner"].as_str(), Some(name));
        assert!(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(runner.contract_path())
                .exists(),
            "{name}: contract_path names a real file"
        );
        for key in ["kind", "binary", "parser", "transcript", "provenance"] {
            assert!(
                c[key].as_str().is_some_and(|s| !s.is_empty()),
                "{name}: {key}"
            );
        }
        assert!(c["synthetic"].as_bool().is_some(), "{name}: synthetic");
        for a in c["always"].as_array().unwrap() {
            assert!(!a["args"].as_array().unwrap().is_empty(), "{name}");
            assert!(
                a["why"]
                    .as_str()
                    .is_some_and(|w| !w.is_empty() && !w.contains('\n')),
                "{name}: every always-flag says why in one line: {a:?}"
            );
        }
        for kind in ["init", "assistant", "tool_use", "result", "usage"] {
            assert!(
                c["events"][kind].as_str().is_some_and(|s| !s.is_empty()),
                "{name}: events.{kind}"
            );
        }
        for key in ["found_in", "resume"] {
            assert!(
                c["session"][key].as_str().is_some(),
                "{name}: session.{key}"
            );
        }
        for key in ["usage_limit", "refusal", "crash"] {
            assert!(
                c["exit_codes"][key].as_str().is_some_and(|s| !s.is_empty()),
                "{name}: exit_codes.{key}"
            );
        }
        assert!(
            !c["cannot"].as_table().unwrap().is_empty(),
            "{name}: states what it cannot do"
        );
        assert_eq!(
            runner.contract_path(),
            format!("src/agent/contracts/{name}.toml")
        );
    }
}

#[test]
fn the_claude_transcript_replays_into_the_recorded_outcome() {
    let g = golden(Runner::ClaudeCli);
    let mut out = Outcome::default();
    let mut seen = HashSet::new();
    let mut watch = Watch::new(thresholds());
    let mut tools = Vec::new();
    for v in transcript(Runner::ClaudeCli) {
        let early =
            claude::apply_claude_frame(&v, &mut out, &mut seen, &mut watch, true, &mut |n| {
                tools.push(n.to_string())
            });
        assert!(early.is_none());
    }
    assert_eq!(out.session_id.as_deref(), g["session_id"].as_str());
    assert_eq!(out.num_turns, g["turns"].as_integer().unwrap());
    assert_eq!(out.tool_calls, g["tool_calls"].as_integer().unwrap());
    assert_eq!(tools, ["Bash", "StructuredOutput"], "each tool_use id once");
    assert_eq!(out.cost_usd, g["cost_usd"].as_float());
    assert_eq!(out.input_tokens, g["input_tokens"].as_integer());
    assert_eq!(out.output_tokens, g["output_tokens"].as_integer());
    assert_eq!(
        out.cache_read_input_tokens,
        g["cache_read_input_tokens"].as_integer()
    );
    assert_eq!(
        out.cache_creation_input_tokens,
        g["cache_creation_input_tokens"].as_integer()
    );
    assert_eq!(
        out.rate_limits.five_hour.map(|w| w.0),
        g["five_hour_utilization"].as_float()
    );
    assert!(out.got_result && !out.is_error && !out.rate_limited);
    assert_envelope(&out, &g);
}

#[test]
fn the_codex_transcript_replays_into_the_recorded_outcome() {
    let g = golden(Runner::CodexCli);
    let mut out = Outcome::default();
    let mut watch = Watch::new(thresholds());
    for v in transcript(Runner::CodexCli) {
        assert!(apply_codex_event(&v, &mut out, &mut watch, true).is_none());
    }
    assert_eq!(out.session_id.as_deref(), g["session_id"].as_str());
    assert_eq!(out.num_turns, g["turns"].as_integer().unwrap());
    assert_eq!(out.tool_calls, g["tool_calls"].as_integer().unwrap());
    assert_eq!(out.input_tokens, g["input_tokens"].as_integer());
    assert_eq!(out.output_tokens, g["output_tokens"].as_integer());
    assert_eq!(
        out.cache_read_input_tokens,
        g["cache_read_input_tokens"].as_integer()
    );
    assert_eq!(out.cost_usd, None, "codex reports tokens, never dollars");
    assert!(
        out.got_result && !out.is_error,
        "the warning item is cleared"
    );
    assert_envelope(&out, &g);
}

#[test]
fn the_copilot_transcript_replays_into_the_recorded_outcome() {
    let g = golden(Runner::CopilotCli);
    let c = load(Runner::CopilotCli);
    let mut out = Outcome::default();
    let mut watch = Watch::new(thresholds());
    let mut tally = CopilotTally::default();
    for v in transcript(Runner::CopilotCli) {
        assert!(apply_copilot_event(&v, &mut out, &mut watch, true, &mut tally).is_none());
    }
    assert_eq!(out.session_id.as_deref(), g["session_id"].as_str());
    assert_eq!(out.num_turns, g["turns"].as_integer().unwrap());
    assert_eq!(out.tool_calls, g["tool_calls"].as_integer().unwrap());
    assert_eq!(
        tally.premium_requests,
        g["premium_requests"].as_integer().unwrap()
    );
    assert_eq!(
        out.input_tokens, None,
        "copilot meters requests, not tokens"
    );
    // run_copilot prices the requests at the provider's per-request price.
    let price = c["price_usd_per_premium_request"].as_float().unwrap();
    let cost = tally.premium_requests as f64 * price;
    assert!(
        (cost - g["cost_usd"].as_float().unwrap()).abs() < 1e-12,
        "{cost}"
    );
    assert!(out.got_result && !out.is_error);
    assert_envelope(&out, &g);
}

#[test]
fn the_chat_transcript_replays_into_the_recorded_outcome() {
    let g = golden(Runner::Chat);
    let c = load(Runner::Chat);
    let schema: Value = serde_json::from_str(c["schema"].as_str().unwrap()).unwrap();
    let provider = Provider {
        runner: Runner::Chat,
        price_input_per_million: c["price_input_per_million"].as_float().unwrap(),
        price_output_per_million: c["price_output_per_million"].as_float().unwrap(),
        ..Provider::default()
    };
    let frames = transcript(Runner::Chat);
    assert_eq!(frames.len(), 1);
    let mut out = Outcome::default();
    chat::read_response(&mut out, &provider, &frames[0]["body"], &schema);
    assert_eq!(out.session_id, None);
    assert_eq!(out.num_turns, g["turns"].as_integer().unwrap());
    assert_eq!(out.tool_calls, g["tool_calls"].as_integer().unwrap());
    assert_eq!(out.input_tokens, g["input_tokens"].as_integer());
    assert_eq!(out.output_tokens, g["output_tokens"].as_integer());
    assert!((out.cost_usd.unwrap() - g["cost_usd"].as_float().unwrap()).abs() < 1e-12);
    assert!(out.got_result && !out.is_error);
    let structured: Value = serde_json::from_str(out.structured.as_deref().unwrap()).unwrap();
    assert_eq!(structured["summary"].as_str(), g["summary"].as_str());
}

#[test]
fn the_jev_transcript_replays_into_the_recorded_outcome() {
    let g = golden(Runner::Jev);
    let c = load(Runner::Jev);
    let text = "name = \"triage\"\nkind = \"directive\"\ncontract = \"plan\"\ndescription = \"Route the email.\"\n[outcomes]\nreply = \"a person needs an answer\"\nignore = \"newsletters and noise\"\n";
    let action =
        crate::workflows::parse_action(std::path::Path::new("triage.toml"), text, "h".into())
            .unwrap();
    let provider = Provider {
        runner: Runner::Jev,
        price_input_per_million: c["price_input_per_million"].as_float().unwrap(),
        ..Provider::default()
    };
    let frames = transcript(Runner::Jev);
    assert_eq!(frames.len(), 1);
    let mut out = Outcome::default();
    jev::read_result(&mut out, &provider, &action, &frames[0]["body"]);
    assert_eq!(out.session_id, None);
    assert_eq!(out.num_turns, g["turns"].as_integer().unwrap());
    assert_eq!(out.tool_calls, g["tool_calls"].as_integer().unwrap());
    assert_eq!(out.input_tokens, g["input_tokens"].as_integer());
    assert_eq!(out.output_tokens, g["output_tokens"].as_integer());
    assert!((out.cost_usd.unwrap() - g["cost_usd"].as_float().unwrap()).abs() < 1e-12);
    assert!(out.got_result);
    let env: Value = serde_json::from_str(out.structured.as_deref().unwrap()).unwrap();
    assert_eq!(env["outcome"].as_str(), g["outcome"].as_str());
    assert_eq!(env["confidence"].as_f64(), g["confidence"].as_float());
}

/// The structured envelope's summary and the number of changes it names:
/// the stream carries the changes, git carries the commits.
fn assert_envelope(out: &Outcome, g: &toml::Table) {
    let env: Value = serde_json::from_str(out.structured.as_deref().expect("structured")).unwrap();
    assert_eq!(env["summary"].as_str(), g["summary"].as_str());
    assert_eq!(
        env["changes"].as_array().map(Vec::len).map(|n| n as i64),
        g["changes"].as_integer()
    );
}
