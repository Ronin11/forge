//! The markdown report of a `forge eval jev` run.

use super::*;
use std::fmt::Write as _;

/// The share of `hits` in `n`, as a percentage.
fn pct(hits: usize, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * hits as f64 / n as f64
    }
}

/// Which of ten confidence deciles a confidence falls in; 1.0 is the last.
fn decile(confidence: f64) -> usize {
    ((confidence.clamp(0.0, 1.0) * 10.0) as usize).min(9)
}

/// Confidence decile against accuracy, one row per decile that has answers.
fn calibration(set: &Set, judged: &[Judged]) -> String {
    let mut bins = [(0usize, 0usize, 0.0f64); 10];
    for (item, j) in set.items.iter().zip(judged) {
        let Some(p) = &j.predicted else { continue };
        let b = &mut bins[decile(j.confidence)];
        b.0 += 1;
        b.1 += usize::from(*p == item.label);
        b.2 += j.confidence;
    }
    let mut t =
        String::from("| confidence | answers | mean confidence | accuracy |\n|---|---|---|---|\n");
    for (i, (n, hits, sum)) in bins.iter().enumerate().filter(|(_, b)| b.0 > 0) {
        let _ = writeln!(
            t,
            "| {:.1}-{:.1} | {n} | {:.2} | {:.1}% |",
            i as f64 / 10.0,
            (i + 1) as f64 / 10.0,
            sum / *n as f64,
            pct(*hits, *n)
        );
    }
    t
}

/// How many items carry each label, `a 3, b 1`.
fn label_counts(items: &[Item]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for i in items {
        *counts.entry(&i.label).or_default() += 1;
    }
    counts
        .iter()
        .map(|(l, n)| format!("{l} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn section(set: &Set, judged: &[Judged]) -> String {
    let mut s = format!("## {}\n\n{}\n\n", set.title, set.instructions);
    if set.items.is_empty() {
        s.push_str("No labeled items in the record.\n\n");
        return s;
    }
    let answered = judged.iter().filter(|j| j.predicted.is_some()).count();
    let hits = set
        .items
        .iter()
        .zip(judged)
        .filter(|(i, j)| j.predicted.as_deref() == Some(i.label.as_str()))
        .count();
    let latency = judged.iter().map(|j| j.latency_ms).sum::<u128>() as f64 / judged.len() as f64;
    let cost: f64 = judged.iter().map(|j| j.cost_usd).sum();
    let _ = writeln!(
        s,
        "- items: {} ({})",
        set.items.len(),
        label_counts(&set.items)
    );
    let _ = writeln!(
        s,
        "- accuracy: {:.1}% ({hits} of {answered} answered)",
        pct(hits, answered)
    );
    let _ = writeln!(s, "- errors: {}", judged.len() - answered);
    if let Some(e) = judged.iter().find_map(|j| j.error.as_deref()) {
        let _ = writeln!(s, "- first error: {e}");
    }
    let _ = writeln!(s, "- mean latency: {latency:.0} ms");
    let _ = writeln!(s, "- total cost: ${cost:.6}\n");
    s.push_str("Calibration:\n\n");
    s.push_str(&calibration(set, judged));
    s.push('\n');
    s
}

pub(super) fn render(
    provider: &str,
    day: &str,
    replayed: bool,
    sets: &[Set],
    results: &[Vec<Judged>],
) -> String {
    let source = if replayed {
        "a recorded fixture"
    } else {
        "the store"
    };
    let mut s = format!(
        "# Jev eval, {day}\n\nProvider `{provider}`, sets pulled from {source}. Accuracy is over answered items; \
         an error is a call that failed or returned no answer. Task size is labeled from lines changed at landing: \
         small up to {SMALL_LINES}, medium up to {MEDIUM_LINES}, large above.\n\n"
    );
    for (set, judged) in sets.iter().zip(results) {
        s.push_str(&section(set, judged));
    }
    s
}
