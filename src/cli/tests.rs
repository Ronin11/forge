use super::jobs::job_test_target;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[test]
fn job_test_reads_a_lone_directory_as_the_path_and_a_lone_name_as_the_workflow() {
    let target = |w: Option<&str>, p: Option<&str>| {
        job_test_target(w.map(str::to_string), p.map(PathBuf::from))
    };
    let here = PathBuf::from(".");
    assert_eq!(target(None, None), (None, here.clone()));
    assert_eq!(target(Some("."), None), (None, here.clone()));
    assert_eq!(
        target(Some("../repo"), None),
        (None, PathBuf::from("../repo"))
    );
    // The crate root, where tests run, holds a `src` directory and no
    // run workflow called `src` or `no-such-workflow`.
    assert_eq!(target(Some("src"), None), (None, PathBuf::from("src")));
    assert_eq!(
        target(Some("no-such-workflow"), None),
        (Some("no-such-workflow".to_string()), here)
    );
    assert_eq!(
        target(Some("wf"), Some("/repo")),
        (Some("wf".to_string()), PathBuf::from("/repo"))
    );
}

/// Functions over 80 lines that stay only because they are named here,
/// each with the one-line reason it is not a parse-call-print verb. A
/// function may leave this list once it shrinks under 80 lines; nothing
/// new joins it (docs/REVIEW-2.md, stage 3: "a cli.rs function parses
/// arguments, calls one kernel function and prints").
const OVER_80_ALLOWED: &[(&str, &str)] = &[
    (
        "show",
        "renders a task's full record: env, ops, attempts, verdicts",
    ),
    (
        "trace",
        "renders a task's lineage, tokens, rate limits and outcome",
    ),
    (
        "initiative_report",
        "renders an initiative's plan, tasks and blockers",
    ),
    ("log", "renders the task list table, column by column"),
    (
        "list_workflows",
        "renders the workflow and action catalog, text and json",
    ),
    (
        "show_workflow",
        "finds one workflow (catalog or repo), resolves its steps, renders text and json",
    ),
    (
        "stats",
        "renders whichever of five stats tables the flags ask for",
    ),
    (
        "quality_stats",
        "renders the defect-escape and delayed-cost table",
    ),
    (
        "land_task",
        "lands a verified branch and reports every landing outcome",
    ),
];

/// Blank out comments and string/char literals so the braces they
/// contain (format strings are full of `{}`) don't confuse the line
/// counter below; every other byte, including newlines, is kept in
/// place so line numbers still line up with the source.
fn strip_noise(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if c[i] == '/' && i + 1 < n && c[i + 1] == '/' {
            while i < n && c[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c[i] == '/' && i + 1 < n && c[i + 1] == '*' {
            out.push_str("  ");
            i += 2;
            while i < n && !(c[i] == '*' && i + 1 < n && c[i + 1] == '/') {
                out.push(if c[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            if i < n {
                out.push_str("  ");
                i += 2;
            }
        } else if c[i] == '"' {
            out.push(' ');
            i += 1;
            while i < n && c[i] != '"' {
                if c[i] == '\\' && i + 1 < n {
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(if c[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            if i < n {
                out.push(' ');
                i += 1;
            }
        } else if c[i] == '\'' && i + 3 < n && c[i + 1] == '\\' && c[i + 3] == '\'' {
            out.push_str("    ");
            i += 4;
        } else if c[i] == '\'' && i + 2 < n && c[i + 1] != '\\' && c[i + 2] == '\'' {
            out.push_str("   ");
            i += 3;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// The bare name after a leading `pub`/`pub(crate)`/`async fn`, or
/// `None` if the line does not open a function.
fn fn_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub(super) "))
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t);
    let t = t.strip_prefix("async ").unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Every `fn`'s name and the number of lines from its signature to its
/// closing brace, found by matching braces on the noise-stripped source.
fn fn_line_counts(src: &str) -> Vec<(String, usize)> {
    let clean = strip_noise(src);
    let lines: Vec<&str> = src.lines().collect();
    let clean_lines: Vec<&str> = clean.lines().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(name) = fn_name(lines[i]) else {
            i += 1;
            continue;
        };
        let mut depth = 0i32;
        let mut started = false;
        let mut end = i;
        'body: for (j, cl) in clean_lines.iter().enumerate().skip(i) {
            for ch in cl.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        started = true;
                    }
                    '}' => {
                        depth -= 1;
                        if started && depth == 0 {
                            end = j;
                            break 'body;
                        }
                    }
                    _ => {}
                }
            }
        }
        found.push((name, end - i + 1));
        i = end + 1;
    }
    found
}

#[test]
fn no_cli_function_grows_past_eighty_lines_unless_named() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cli");
    let mut pending = vec![root];
    let mut functions = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                fs::read_dir(path)
                    .expect("read CLI directory")
                    .map(|e| e.unwrap().path()),
            );
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            functions.extend(fn_line_counts(
                &fs::read_to_string(path).expect("read CLI source"),
            ));
        }
    }
    let allowed: HashSet<&str> = OVER_80_ALLOWED.iter().map(|(name, _)| *name).collect();
    let offenders: Vec<(String, usize)> = functions
        .into_iter()
        .filter(|(name, len)| *len > 80 && !allowed.contains(name.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "CLI functions over 80 lines with no allowlist entry: {offenders:?}"
    );
}
