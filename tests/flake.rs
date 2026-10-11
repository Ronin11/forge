//! Flakiness, made visible: reads the records the e2e harness appends to
//! `target/tmp/e2e-timing.jsonl` (one line per finished test: name, wall
//! seconds, pass/fail, load average at start) and lists the tests whose
//! duration varied more than 5x across the recorded runs. A test that takes
//! 0.8 s on one run and 9 s on another is waiting on a clock, not an event
//! (docs/CHECKS.md, "Timing in tests").
//!
//! The report is printed (`cargo test --test flake -- --nocapture`), never a
//! failure: a check that failed on the very thing it measures would be one
//! more flake.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// A test is listed when its slowest recorded run took more than this many
/// times its fastest.
const SPREAD: f64 = 5.0;
/// Runs below this many seconds are scheduler noise, not a timing
/// assumption, however large the ratio.
const FLOOR_SECS: f64 = 0.5;
/// How many of the most recent records per test are compared.
const RUNS: usize = 20;

#[derive(Debug, PartialEq)]
struct Varies {
    test: String,
    fastest: f64,
    slowest: f64,
    load_at_slowest: f64,
    runs: usize,
}

/// The tests in `jsonl` whose durations vary more than `SPREAD`x, widest
/// spread first. Unparseable lines are skipped.
fn varying(jsonl: &str) -> Vec<Varies> {
    let mut by_test: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(test), Some(secs)) = (v["test"].as_str(), v["secs"].as_f64()) else {
            continue;
        };
        let load = v["load"].as_f64().unwrap_or(0.0);
        by_test
            .entry(test.to_string())
            .or_default()
            .push((secs, load));
    }
    let mut out = Vec::new();
    for (test, mut runs) in by_test {
        if runs.len() > RUNS {
            runs.drain(..runs.len() - RUNS);
        }
        let fastest = runs.iter().map(|r| r.0).fold(f64::MAX, f64::min);
        let (slowest, load_at_slowest) = runs
            .iter()
            .copied()
            .fold((0.0, 0.0), |a, r| if r.0 > a.0 { r } else { a });
        if runs.len() > 1 && slowest >= FLOOR_SECS && slowest > fastest.max(0.001) * SPREAD {
            out.push(Varies {
                test,
                fastest,
                slowest,
                load_at_slowest,
                runs: runs.len(),
            });
        }
    }
    out.sort_by(|a, b| (b.slowest / b.fastest).total_cmp(&(a.slowest / a.fastest)));
    out
}

#[test]
fn a_test_whose_duration_varies_more_than_five_fold_is_listed() {
    let jsonl = [
        r#"{"test":"steady","secs":1.0,"load":1.0}"#,
        r#"{"test":"steady","secs":1.4,"load":9.0}"#,
        r#"{"test":"flaky","secs":0.8,"load":1.0}"#,
        r#"{"test":"flaky","secs":9.0,"load":40.0}"#,
        r#"{"test":"tiny","secs":0.001,"load":1.0}"#,
        r#"{"test":"tiny","secs":0.2,"load":1.0}"#,
        "not json",
    ]
    .join("\n");
    let v = varying(&jsonl);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].test, "flaky");
    assert_eq!(v[0].load_at_slowest, 40.0);
    assert_eq!(v[0].runs, 2);
}

#[test]
fn report_tests_whose_duration_varies_more_than_five_fold() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e-timing.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("no timing records at {}; run the e2e suite", path.display());
        return;
    };
    let v = varying(&text);
    if v.is_empty() {
        eprintln!("no test varies more than {SPREAD}x across its last {RUNS} runs");
    }
    for t in &v {
        eprintln!(
            "VARIES {:.1}x  {}: {:.2}s .. {:.2}s over {} runs (load {:.1} at the slowest)",
            t.slowest / t.fastest.max(0.001),
            t.test,
            t.fastest,
            t.slowest,
            t.runs,
            t.load_at_slowest
        );
    }
}
