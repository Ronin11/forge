//! The failing-test parser: test names from go, pytest, jest and cargo test
//! output. Unknown output yields nothing rather than a guess.

/// Failing test names in order of first appearance, each once.
pub fn failing_tests(out: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        let n = n.trim();
        if !n.is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    // Inside cargo's `failures:` name list (indented, ends at a blank line).
    let mut listing = false;
    for raw in out.lines() {
        let line = raw.trim();
        if listing {
            if line.is_empty() || !raw.starts_with("    ") {
                listing = false;
            } else {
                add(line);
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("--- FAIL: ") {
            // go test
            if let Some(n) = rest.split_whitespace().next() {
                add(n);
            }
        } else if let Some(rest) = line.strip_prefix("FAILED ").filter(|r| r.contains("::")) {
            // pytest
            if let Some(n) = rest.split_whitespace().next() {
                add(n);
            }
        } else if let Some(rest) = line.strip_prefix("✕ ").or_else(|| line.strip_prefix("✗ ")) {
            // jest
            add(rest);
        } else if let Some(rest) = line.strip_prefix("test ") {
            // cargo: `test foo::bar ... FAILED`
            if let Some((n, _)) = rest.rsplit_once(" ... FAILED") {
                add(n);
            }
        } else if line == "failures:" {
            // cargo: the summary list follows; the stdout-detail section
            // before it has no indented lines, so it adds nothing here.
            listing = true;
        } else if let Some(rest) = line.strip_prefix("---- ") {
            // cargo: `---- foo::bar stdout ----`
            if let Some(n) = rest.strip_suffix(" stdout ----") {
                add(n);
            }
        } else if let Some(rest) = line.strip_prefix("thread '") {
            // cargo: `thread 'foo::bar' panicked at src/lib.rs:3:5:`
            if let Some((n, tail)) = rest.split_once('\'')
                && tail.starts_with(" panicked at")
                && n != "main"
                && n != "<unnamed>"
            {
                add(n);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_go_pytest_and_jest_failures_only() {
        let out = "\
=== RUN   TestA
--- FAIL: TestA (0.00s)
--- PASS: TestB (0.00s)
FAILED tests/test_x.py::test_one - AssertionError
FAILED not a pytest line
  ✕ renders the header (12 ms)
  ✓ renders the footer
random FAIL text
";
        assert_eq!(
            failing_tests(out),
            vec![
                "TestA",
                "tests/test_x.py::test_one",
                "renders the header (12 ms)"
            ]
        );
        assert!(failing_tests("all good").is_empty());
    }

    #[test]
    fn cargo_all_pass_is_empty() {
        let out = "\
   Compiling demo v0.1.0 (/tmp/demo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.31s
     Running unittests src/lib.rs (target/debug/deps/demo-0123)

running 2 tests
test tests::adds ... ok
test tests::ignored ... ignored

test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
";
        assert!(failing_tests(out).is_empty());
    }

    #[test]
    fn cargo_failure_with_a_panic() {
        let out = "\
running 2 tests
test math::adds ... ok
test math::subtracts ... FAILED

failures:

---- math::subtracts stdout ----

thread 'math::subtracts' panicked at src/math.rs:12:9:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    math::subtracts

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`
";
        assert_eq!(failing_tests(out), vec!["math::subtracts"]);
    }

    #[test]
    fn cargo_several_test_binaries() {
        let out = "\
     Running unittests src/lib.rs (target/debug/deps/demo-0123)

running 1 test
test a::one ... FAILED

failures:

---- a::one stdout ----

thread 'a::one' panicked at src/a.rs:4:5:
boom

failures:
    a::one

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/api.rs (target/debug/deps/api-4567)

running 2 tests
test api::ok ... ok
test api::bad ... FAILED

failures:

---- api::bad stdout ----

thread 'api::bad' panicked at tests/api.rs:9:5:
no

failures:
    api::bad

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";
        assert_eq!(failing_tests(out), vec!["a::one", "api::bad"]);
    }

    #[test]
    fn cargo_compile_error_names_no_test() {
        let out = "\
   Compiling demo v0.1.0 (/tmp/demo)
error[E0425]: cannot find value `x` in this scope
 --> src/lib.rs:3:5
  |
3 |     x
  |     ^ not found in this scope

error: could not compile `demo` (lib test) due to 1 previous error
";
        assert!(failing_tests(out).is_empty());
    }

    #[test]
    fn cargo_doctest_names_keep_their_spaces() {
        let out = "\
test src/lib.rs - add (line 3) ... FAILED

failures:

---- src/lib.rs - add (line 3) stdout ----
Test executable failed (exit status: 101).

failures:
    src/lib.rs - add (line 3)

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
";
        assert_eq!(failing_tests(out), vec!["src/lib.rs - add (line 3)"]);
    }

    #[test]
    fn a_panic_in_main_thread_is_not_a_test() {
        assert!(failing_tests("thread 'main' panicked at src/main.rs:2:5:\nx").is_empty());
    }
}
