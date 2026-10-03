//! Regression coverage for prompt transport beyond Linux's per-argument limit.
use super::*;
use std::os::unix::fs::PermissionsExt;

async fn large_prompt(runner: Runner, resume: Option<&str>, step: &str) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    let fake = dir.path().join("fake.py");
    std::fs::write(
        &fake,
        r#"#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
assert sum(len(a.encode()) + 1 for a in sys.argv) < 1024, 'prompt leaked to argv'
assert '-p' not in args
codex = args[0] == 'exec'
if codex:
    assert args[-1] == '-'
prompt = sys.stdin.buffer.read()
print(json.dumps({'type': 'stdin_length', 'length': len(prompt)}))
if prompt == b'x' * (200 * 1024):
    if codex:
        print(json.dumps({'type': 'thread.started', 'thread_id': 'session'}))
    else:
        print(json.dumps({'type': 'result', 'sessionId': 'session'}))
else:
    assert prompt.startswith(b'Do no further work.'), 'wrong report prompt'
    text = '{"summary":"whole prompt received"}'
    if codex:
        print(json.dumps({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': text}}))
    else:
        print(json.dumps({'type': 'assistant.message', 'data': {'content': text}}))
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prefix = if runner == Runner::CodexCli {
        "CODEX"
    } else {
        "COPILOT"
    };
    let key = format!("FORGE_{prefix}_BIN_{}", step.to_uppercase());
    // Each test owns a unique step-specific environment key.
    unsafe { std::env::set_var(&key, &fake) };
    let provider = Provider {
        runner,
        nudges: 0,
        ..Provider::default()
    };
    let report = Reporter::new(false, None);
    let log_path = dir.path().join("log.jsonl");
    let prompt = "x".repeat(200 * 1024);
    let mut launch = tests::test_launch(
        dir.path(),
        &report,
        &provider,
        &log_path,
        "{}",
        false,
        resume,
    );
    launch.prompt = &prompt;
    launch.step = step;
    let outcome = match runner {
        Runner::CodexCli => run_codex(launch).await.unwrap(),
        Runner::CopilotCli => run_copilot(launch).await.unwrap(),
        _ => unreachable!(),
    };
    unsafe { std::env::remove_var(key) };
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    assert!(!outcome.timed_out, "{outcome:?}");
    assert_eq!(
        outcome.structured.as_deref(),
        Some("{\"summary\":\"whole prompt received\"}")
    );
    let log = std::fs::read_to_string(log_path).unwrap();
    let lengths: Vec<usize> = log
        .lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            (value["type"] == "stdin_length").then(|| value["length"].as_u64().unwrap() as usize)
        })
        .collect();
    let report_len = if runner == Runner::CodexCli {
        codex::CODEX_REPORT_PROMPT.len()
    } else {
        copilot::COPILOT_REPORT_PROMPT.len() + 2
    };
    assert_eq!(lengths, [prompt.len(), report_len]);
}

#[tokio::test]
async fn codex_receives_200_kib_on_stdin_fresh_and_resumed() {
    large_prompt(Runner::CodexCli, None, "big_codex_fresh").await;
    large_prompt(Runner::CodexCli, Some("session"), "big_codex_resume").await;
}

#[tokio::test]
async fn copilot_receives_200_kib_on_stdin_fresh_and_resumed() {
    large_prompt(Runner::CopilotCli, None, "big_copilot_fresh").await;
    large_prompt(Runner::CopilotCli, Some("session"), "big_copilot_resume").await;
}
