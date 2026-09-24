//! Keep source files small; pre-existing large files have a shrinking exception list.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

// Ceilings are the line counts at the CLI split's base plus 200.
// Remove entries as files are split; never add new exceptions.
const ALLOWLIST: &[(&str, usize, &str)] = &[
    (
        "client/src/lib.rs",
        1986,
        "Existing module awaiting a focused split",
    ),
    (
        "repomap/src/main.rs",
        1738,
        "Existing module awaiting a focused split",
    ),
    (
        "src/agent.rs",
        3473,
        "Existing module awaiting a focused split",
    ),
    (
        "src/config.rs",
        1930,
        "Existing module awaiting a focused split",
    ),
    (
        "src/engine.rs",
        1988,
        "Existing module awaiting a focused split",
    ),
    (
        "src/job.rs",
        2970,
        "Existing module awaiting a focused split",
    ),
    (
        "src/queue.rs",
        2088,
        "Existing module awaiting a focused split",
    ),
    (
        "src/verify.rs",
        2308,
        "Existing module awaiting a focused split",
    ),
    (
        "src/view.rs",
        4891,
        "Existing module awaiting a focused split",
    ),
    (
        "src/worker.rs",
        1917,
        "Existing module awaiting a focused split",
    ),
    (
        "src/workflows.rs",
        3677,
        "Existing module awaiting a focused split",
    ),
    (
        "tests/e2e/deploy.rs",
        2060,
        "Existing integration coverage awaiting a focused split",
    ),
    (
        "tests/e2e/jobs.rs",
        4290,
        "Existing integration coverage awaiting a focused split",
    ),
    (
        "tests/e2e/landing.rs",
        1779,
        "Existing integration coverage awaiting a focused split",
    ),
    (
        "tests/e2e/listing.rs",
        2212,
        "Existing integration coverage awaiting a focused split",
    ),
    (
        "tests/e2e/plugins.rs",
        2371,
        "Existing integration coverage awaiting a focused split",
    ),
    (
        "tui/src/lib.rs",
        1795,
        "Existing module awaiting a focused split",
    ),
    (
        "web/src/main.rs",
        2051,
        "Existing module awaiting a focused split",
    ),
];

#[test]
fn tracked_rust_files_stay_within_their_line_limits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs"])
        .current_dir(root)
        .output()
        .expect("list tracked Rust files");
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths = std::str::from_utf8(&output.stdout).expect("UTF-8 tracked paths");
    let mut counts = BTreeMap::new();
    let mut failures = Vec::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        let full = root.join(path);
        if !full.exists() {
            failures.push(format!("{path}: tracked file is missing"));
            continue;
        }
        let lines = std::fs::read_to_string(full)
            .expect("read tracked Rust source")
            .lines()
            .count();
        counts.insert(path, lines);
        let ceiling = ALLOWLIST
            .iter()
            .find(|(p, _, _)| *p == path)
            .map_or(1500, |(_, ceiling, _)| *ceiling);
        if lines > ceiling {
            failures.push(format!("{path}: {lines} lines exceeds ceiling {ceiling}"));
        }
    }
    for &(path, _, reason) in ALLOWLIST {
        assert!(!reason.is_empty(), "{path}: exception needs a reason");
        if counts.get(path).is_none_or(|lines| *lines <= 1500) {
            failures.push(format!(
                "{path}: stale allowlist entry; delete it (file is gone or at most 1,500 lines)"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\nSplit the file the way src/store/ and src/cli/ were split.",
        failures.join("\n")
    );
}
