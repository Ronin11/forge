//! Whether a failed check is the tests' fault, not the implementer's, who
//! cannot see or edit the verification namespace.

use crate::checks::{CheckResult, last_lines};

/// The first failed L1 check whose reported file locations all lie inside
/// the verification namespace: the tests' fault, not the implementer's,
/// who cannot see or edit those files. The check name and its tail.
pub fn tests_fault(checks: &[CheckResult], namespace: &[String]) -> Option<(String, String)> {
    checks
        .iter()
        .filter(|c| !c.ok && c.level == "L1")
        .find_map(|c| {
            let locs = locations(&c.tail);
            let inside = !locs.is_empty()
                && locs
                    .iter()
                    .all(|l| namespace.iter().any(|n| l.starts_with(n.as_str())));
            inside.then(|| (c.name.clone(), last_lines(&c.tail, 20)))
        })
}

/// File locations mentioned in check output, in the two shapes compilers
/// and linters print: `path(line,col)` and `path:line`.
fn locations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'.' | b'/' | b'-'))
            {
                i += 1;
            }
            if i > start
                && i < b.len()
                && matches!(b[i], b'(' | b':')
                && b.get(i + 1).is_some_and(|c| c.is_ascii_digit())
            {
                let tok = line[start..i].trim_start_matches("./");
                if tok.contains('.') && !tok.starts_with('.') {
                    out.push(tok.to_string());
                }
            }
            if i == start {
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, ok: bool, tail: &str) -> CheckResult {
        CheckResult {
            level: "L1".into(),
            name: name.into(),
            ok,
            exit: Some(if ok { 0 } else { 2 }),
            ms: 1,
            timed_out: false,
            tail: tail.into(),
            failing_tests: vec![],
            log_path: String::new(),
            stdout: String::new(),
        }
    }

    #[test]
    fn locations_come_in_compiler_and_grep_shapes() {
        let t = "tests/acceptance/p.test.ts(27,12): error TS2532: x\n./src/a.ts:3:1 - warning\nerror TS1234 plain\n";
        assert_eq!(locations(t), vec!["tests/acceptance/p.test.ts", "src/a.ts"]);
    }

    #[test]
    fn a_failure_only_inside_the_namespace_is_the_tests_fault() {
        let ns = vec!["tests/acceptance/".to_string()];
        let only = check(
            "typecheck",
            false,
            "tests/acceptance/p.test.ts(27,12): error TS2532\ntests/acceptance/p.test.ts(28,12): error TS2532",
        );
        assert_eq!(
            tests_fault(&[check("lint", true, ""), only], &ns).map(|f| f.0),
            Some("typecheck".into())
        );
        let mixed = check(
            "typecheck",
            false,
            "tests/acceptance/p.test.ts(27,12): error\nsrc/sim/data.ts(4,1): error",
        );
        assert!(
            tests_fault(&[mixed], &ns).is_none(),
            "an error in the implementer's own files is theirs"
        );
        let none = check("test", false, "FAIL 3 tests\nexpected 42 got 41");
        assert!(
            tests_fault(&[none], &ns).is_none(),
            "no locations, no attribution"
        );
        let passing = check("typecheck", true, "tests/acceptance/p.test.ts(1,1): note");
        assert!(tests_fault(&[passing], &ns).is_none());
    }
}
