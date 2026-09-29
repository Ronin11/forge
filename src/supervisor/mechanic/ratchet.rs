//! Recognizing one of the repository's own ratchets among a failed check's
//! tests, and the guidance a refile needs: which file or function is at its
//! bound, and that new code belongs in a new module or function — or, for
//! the built-in history ratchet, that the old blob hash needs appending.

use crate::checks::CheckResult;

/// One of the four ratchets `docs/CONTRIBUTING.md` describes: three test
/// files with a shrinking allowlist, and the built-in history test inside
/// `workflows::shadow` (its own module, not a `tests/*.rs` file).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ratchet {
    FileSize,
    FnLength,
    Layers,
    BuiltinHistory,
}

impl Ratchet {
    fn test_name(self) -> &'static str {
        match self {
            Ratchet::FileSize => "tracked_rust_files_stay_within_their_line_limits",
            Ratchet::FnLength => "tracked_rust_functions_stay_within_their_line_limits",
            Ratchet::Layers => "layers_only_import_downward",
            Ratchet::BuiltinHistory => "builtin_history_lists_every_built_in_current_text",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Ratchet::FileSize => "tests/file_size.rs",
            Ratchet::FnLength => "tests/fn_length.rs",
            Ratchet::Layers => "tests/layers.rs",
            Ratchet::BuiltinHistory => "workflows::shadow's built-in history test",
        }
    }

    const ALL: [Ratchet; 4] = [
        Ratchet::FileSize,
        Ratchet::FnLength,
        Ratchet::Layers,
        Ratchet::BuiltinHistory,
    ];
}

/// A ratchet found among a check's failing tests, with the guidance a
/// refile appends to the task text.
pub struct Hit {
    pub ratchet: Ratchet,
    pub guidance: String,
}

/// The first ratchet test named among `checks`' failing tests, if any.
pub fn detect(checks: &[CheckResult]) -> Option<Hit> {
    checks.iter().filter(|c| !c.ok).find_map(|c| {
        let tests = if c.failing_tests.is_empty() {
            crate::checks::failing_tests(&c.tail)
        } else {
            c.failing_tests.clone()
        };
        Ratchet::ALL.into_iter().find_map(|r| {
            tests
                .iter()
                .any(|t| t.contains(r.test_name()))
                .then(|| Hit {
                    ratchet: r,
                    guidance: guidance(r, &c.tail),
                })
        })
    })
}

/// A line of `tail` naming the file or function at its bound, turned into
/// guidance for the refiled task; a generic line when the exact bound could
/// not be read back out (the ratchet still matched by test name alone).
fn guidance(r: Ratchet, tail: &str) -> String {
    let bound = tail.lines().find_map(|l| match r {
        Ratchet::FileSize => file_size_bound(l.trim()),
        Ratchet::FnLength => fn_length_bound(l.trim()),
        Ratchet::Layers => layers_bound(l.trim()),
        Ratchet::BuiltinHistory => history_bound(l.trim()),
    });
    match (r, bound) {
        (Ratchet::BuiltinHistory, Some(file)) => format!(
            "{file} changed. The old blob hash must be appended to src/builtins/history.tsv's history list (run scripts/builtin-history.sh) before this text ships."
        ),
        (Ratchet::BuiltinHistory, None) => {
            "A built-in action or operation changed. The old blob hash must be appended to src/builtins/history.tsv's history list (run scripts/builtin-history.sh) before this text ships.".to_string()
        }
        (_, Some(bound)) => format!("{bound} New code goes in a new module or function, not this one."),
        (_, None) => format!(
            "{} is at its ratchet's bound. New code goes in a new module or function, not this one.",
            r.label()
        ),
    }
}

/// `"{path}: {n} lines exceeds ceiling {c}"` -> `"{path} is at its line
/// ceiling ({n} exceeds {c})."`
fn file_size_bound(line: &str) -> Option<String> {
    let (path, rest) = line.split_once(": ")?;
    let (n, c) = rest.split_once(" lines exceeds ceiling ")?;
    Some(format!("{path} is at its line ceiling ({n} exceeds {c})."))
}

/// `"{path}:{line}: fn {name} is {n} lines, exceeds ceiling {c}"` ->
/// `"fn {name} in {path} is at its line ceiling ({n} exceeds {c})."`
fn fn_length_bound(line: &str) -> Option<String> {
    let (loc, rest) = line.split_once(": fn ")?;
    let (name, rest) = rest.split_once(" is ")?;
    let (n, c) = rest.split_once(" lines, exceeds ceiling ")?;
    Some(format!(
        "fn {name} in {loc} is at its line ceiling ({n} exceeds {c})."
    ))
}

/// `"{from} (L) -> {to} (L): {why}, found in {file}"` -> the edge, why, and file.
fn layers_bound(line: &str) -> Option<String> {
    let (edge, rest) = line.split_once(": ")?;
    let (why, file) = rest.split_once(", found in ")?;
    Some(format!("{edge}: {why}, found in {file}."))
}

/// `"{file} is not in src/builtins/history.tsv at its current text: run
/// scripts/builtin-history.sh"` -> `file`.
fn history_bound(line: &str) -> Option<String> {
    line.strip_suffix(" at its current text: run scripts/builtin-history.sh")
        .and_then(|rest| rest.strip_suffix(" is not in src/builtins/history.tsv"))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, tail: &str, tests: &[&str]) -> CheckResult {
        CheckResult {
            level: "L1".into(),
            name: name.into(),
            ok: false,
            tail: tail.into(),
            failing_tests: tests.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn file_size_hit_names_the_file_and_ceiling() {
        let c = check(
            "test",
            "src/foo.rs: 1512 lines exceeds ceiling 1500\nSplit the file the way src/store/ and src/cli/ were split.",
            &["tracked_rust_files_stay_within_their_line_limits"],
        );
        let hit = detect(&[c]).unwrap();
        assert_eq!(hit.ratchet, Ratchet::FileSize);
        assert!(
            hit.guidance
                .contains("src/foo.rs is at its line ceiling (1512 exceeds 1500)")
        );
        assert!(hit.guidance.contains("new module or function"));
    }

    #[test]
    fn fn_length_hit_names_the_function_and_ceiling() {
        let c = check(
            "test",
            "src/foo.rs:12: fn bar is 130 lines, exceeds ceiling 120\nSplit the function into named steps instead of raising a ceiling.",
            &["tracked_rust_functions_stay_within_their_line_limits"],
        );
        let hit = detect(&[c]).unwrap();
        assert_eq!(hit.ratchet, Ratchet::FnLength);
        assert!(
            hit.guidance
                .contains("fn bar in src/foo.rs:12 is at its line ceiling (130 exceeds 120)")
        );
    }

    #[test]
    fn layers_hit_names_the_edge_and_file() {
        let c = check(
            "test",
            "cli (kernel) -> store (store): points up a layer, found in src/cli/mod.rs\n    (\"cli\", \"store\", \"reason\"),",
            &["layers_only_import_downward"],
        );
        let hit = detect(&[c]).unwrap();
        assert_eq!(hit.ratchet, Ratchet::Layers);
        assert!(hit.guidance.contains("found in src/cli/mod.rs"));
    }

    #[test]
    fn builtin_history_hit_names_the_file_and_the_hash_instruction() {
        let c = check(
            "test",
            "actions/code.toml is not in src/builtins/history.tsv at its current text: run scripts/builtin-history.sh",
            &["workflows::shadow::tests::builtin_history_lists_every_built_in_current_text"],
        );
        let hit = detect(&[c]).unwrap();
        assert_eq!(hit.ratchet, Ratchet::BuiltinHistory);
        assert!(hit.guidance.starts_with("actions/code.toml changed."));
        assert!(hit.guidance.contains("old blob hash must be appended"));
    }

    #[test]
    fn an_ordinary_test_failure_is_not_a_ratchet() {
        let c = check("test", "assertion failed", &["worker::window_hold_waits"]);
        assert!(detect(&[c]).is_none());
    }
}
