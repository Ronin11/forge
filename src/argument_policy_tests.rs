//! Keep argument-count exceptions explicit when new helpers are introduced.

use std::path::Path;

fn check_source_tree(dir: &Path, failures: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            check_source_tree(&path, failures);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = std::fs::read_to_string(&path).expect("read Rust source");
            for line in unexplained_allowances(&source) {
                failures.push(format!("{}:{line}", path.display()));
            }
        }
    }
}

fn unexplained_allowances(source: &str) -> Vec<usize> {
    let lint = concat!("clippy::", "too_many_arguments");
    let mut previous = "";
    let mut failures = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.contains(lint)
            && previous
                .trim()
                .strip_prefix("// Reason:")
                .is_none_or(|reason| reason.trim().is_empty())
        {
            failures.push(index + 1);
        }
        previous = line;
    }
    failures
}

#[test]
fn argument_count_allowances_have_a_reason_on_the_line_above() {
    let mut failures = Vec::new();
    check_source_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut failures,
    );
    assert!(
        failures.is_empty(),
        "argument-count allowances need a // Reason: comment immediately above:\n{}",
        failures.join("\n")
    );
}

#[test]
fn argument_count_reason_must_be_nonempty_and_immediately_above() {
    let allow = concat!("#[allow(clippy::", "too_many_arguments)]");
    assert_eq!(unexplained_allowances(allow), [1]);
    assert_eq!(unexplained_allowances(&format!("// Reason:\n{allow}")), [2]);
    assert_eq!(
        unexplained_allowances(&format!("// Reason: fixture inputs\n\n{allow}")),
        [3]
    );
    assert!(unexplained_allowances(&format!("// Reason: fixture inputs\n{allow}")).is_empty());
}
