//! The review contract's own rows, and the notes a reviewer may leave.
//!
//! A demotion becomes a follow-up task that starts from a fresh clone,
//! long after the reviewer's sandbox and its `/tmp` are gone (task 855's
//! review cited `python3 /tmp/forge855-review.py`; its follow-up could
//! only ask for the file). So a demotion's reproduction must be something
//! a fresh clone can run as written: inline commands and inputs, existing
//! test names, or a file the reviewer writes under
//! `tests/review-notes/<task>/`, which `capture_notes` takes off the
//! branch and attaches to the demotion record verbatim.

use super::{GitFacts, Rule, Subject, emit_rows, l0};
use crate::agent::Outcome;
use crate::checks::CheckResult;
use crate::envelope::{Envelope, Kind, ReviewNote};
use crate::report::Event;
use anyhow::Result;
use std::path::Path;

/// Where a reviewer may write its reproduction files, one directory per
/// task under this root.
pub const NOTES_ROOT: &str = "tests/review-notes";

/// How much of one note is kept, and how many notes: a reproduction, not
/// a dump.
const NOTE_BYTES: usize = 64 * 1024;
const NOTE_FILES: usize = 20;

/// What every re-ask for a self-contained reproduction starts with: the
/// one feedback a reviewer is shown (see `asked`).
pub const REPRODUCTION_ASK: &str = "Your demotion cannot stand as written: ";

/// The task's own notes directory, with a trailing slash.
pub fn notes_dir(task_id: i64) -> String {
    format!("{NOTES_ROOT}/{task_id}/")
}

/// Read every file the reviewer left under the task's notes directory,
/// committed or not, then take them off the branch: a commit that touched
/// nothing else is undone, the files removed and unstaged. A reviewer that
/// also changed anything else keeps its branch as found, for `no-writes`
/// to name.
pub async fn capture_notes(s: &Subject<'_>) -> Result<Vec<ReviewNote>> {
    let dir = notes_dir(s.task_id);
    let root = s.worktree.join(&dir);
    let mut notes = Vec::new();
    collect(&root, &dir, &mut notes);
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes.truncate(NOTE_FILES);
    if notes.is_empty() {
        return Ok(notes);
    }
    let changed = crate::git::changed_paths(s.worktree, s.start_sha).await?;
    if !changed.iter().all(|p| p.starts_with(&dir)) {
        return Ok(notes);
    }
    if !changed.is_empty() {
        crate::git::reset_tracked(s.worktree, s.start_sha).await?;
    }
    let _ = std::fs::remove_dir_all(&root);
    crate::git::unstage(s.worktree, &dir).await?;
    let _ = std::fs::remove_dir(s.worktree.join(NOTES_ROOT));
    let _ = std::fs::remove_dir(s.worktree.join("tests"));
    Ok(notes)
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<ReviewNote>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            collect(&path, &format!("{rel}{name}/"), out);
        } else if kind.is_file()
            && let Ok(bytes) = std::fs::read(&path)
        {
            let mut content =
                String::from_utf8_lossy(&bytes[..bytes.len().min(NOTE_BYTES)]).into_owned();
            if bytes.len() > NOTE_BYTES {
                content.push_str("\n[truncated]");
            }
            out.push(ReviewNote {
                path: format!("{rel}{name}"),
                content,
            });
        }
    }
}

/// The review's rows after the shared L0: no writes, something executed,
/// and a reproduction a fresh clone can run. A demotion stands only with
/// something executed and with nothing cited from the reviewer's sandbox;
/// otherwise `question` is cleared, and a sandbox citation fails its row.
pub async fn rows(
    s: &Subject<'_>,
    agent: &Outcome,
    facts: &GitFacts,
    envelope: Option<&Envelope>,
    notes: &[ReviewNote],
    question: &mut Option<(Kind, String)>,
    checks: &mut Vec<CheckResult>,
) -> Result<()> {
    let added = crate::git::changed_paths(s.worktree, s.start_sha).await?;
    checks.push(l0(
        Rule::NoWrites,
        added.is_empty() && facts.dirty.is_empty(),
        format!(
            "the reviewer changed the branch: {}",
            added
                .iter()
                .chain(facts.dirty.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ));
    checks.push(CheckResult {
        level: Rule::ExecutedSomething.level().into(),
        name: Rule::ExecutedSomething.name().into(),
        ok: agent.tool_calls > 0,
        tail: if agent.tool_calls > 0 { String::new() } else { "the reviewer ran no tool; a review that reads without running is an opinion, so any demotion is ignored".into() },
        ..Default::default()
    });
    let demoted = matches!(question, Some((Kind::Review, _))) && agent.tool_calls > 0;
    let cited = if demoted {
        let dirty = crate::git::dirty_files(s.worktree).await?;
        let dir = notes_dir(s.task_id);
        let uncommitted = |p: &str| {
            dirty.iter().any(|d| d == p)
                || (p.starts_with(NOTES_ROOT)
                    && !(p.starts_with(&dir) && notes.iter().any(|n| n.path == p)))
        };
        sandbox_refs(&demotion_prose(envelope), &uncommitted)
    } else {
        Vec::new()
    };
    checks.push(l0(
        Rule::ReproductionSelfContained,
        cited.is_empty(),
        format!(
            "the demotion cites what a fresh clone does not have: {}",
            cited.join(", ")
        ),
    ));
    emit_rows(s.report, s.task_id, checks);
    if let Some((Kind::Review, text)) = question.as_ref()
        && agent.tool_calls == 0
    {
        s.report.emit(
            s.task_id,
            Event::Note {
                text: &format!("review   demotion ignored (no executed evidence): {text}"),
            },
        );
        *question = None;
    }
    if !cited.is_empty() {
        *question = None;
    }
    Ok(())
}

/// The demotion's own words: its question and its context, the parts a
/// follow-up is given.
fn demotion_prose(envelope: Option<&Envelope>) -> String {
    envelope
        .and_then(|e| e.needs_input.as_ref())
        .map(|q| format!("{}\n{}", q.question, q.context))
        .unwrap_or_default()
}

/// Paths a demotion names that a fresh clone will not have: anything
/// under a temporary directory or a home directory, and any relative path
/// `uncommitted` says is not in the commit. A temporary path the text
/// itself writes (`cat > /tmp/r.py <<'EOF'`, `tee /tmp/r.py`) is inline,
/// so it is not a citation.
pub fn sandbox_refs(text: &str, uncommitted: &dyn Fn(&str) -> bool) -> Vec<String> {
    const ROOTS: &[&str] = &[
        "/tmp",
        "/var/tmp",
        "/dev/shm",
        "$TMPDIR",
        "${TMPDIR}",
        "~",
        "$HOME",
        "${HOME}",
    ];
    const HOMES: &[&str] = &["/home/", "/root/", "/Users/"];
    let tokens: Vec<(bool, String)> = tokens(text);
    let written: Vec<&str> = tokens
        .iter()
        .filter(|(after_write, _)| *after_write)
        .map(|(_, t)| t.as_str())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for (_, tok) in &tokens {
        let tok = tok.as_str();
        let sandbox = ROOTS
            .iter()
            .any(|r| tok == *r && *r != "~" || tok.starts_with(&format!("{r}/")))
            || HOMES.iter().any(|h| tok.starts_with(h));
        let relative = !tok.starts_with('/')
            && !tok.contains("://")
            && !tok.starts_with('$')
            && !tok.starts_with('-')
            && (tok.contains('/') || tok.contains('.'))
            && uncommitted(tok.trim_start_matches("./"));
        if (sandbox && !written.contains(&tok) || relative) && !out.iter().any(|o| o == tok) {
            out.push(tok.to_string());
        }
    }
    out
}

/// The text's words, stripped of quoting and trailing punctuation, each
/// marked with whether it is the target of a write (`>`, `>>`, `tee`).
fn tokens(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut writes_next = false;
    for raw in text.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '=' | '<' | '|' | '&')
    }) {
        let redirect = raw.starts_with('>');
        let word = raw.trim_start_matches('>');
        let tok = word
            .trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '[' | ']' | '{' | '}'))
            .trim_end_matches(['.', ':', '!', '?']);
        let tok = tok.trim_matches(|c: char| matches!(c, '`' | '\'' | '"'));
        if tok.is_empty() {
            writes_next = redirect || raw == "tee";
            continue;
        }
        out.push((writes_next || redirect, tok.to_string()));
        writes_next = tok == "tee";
    }
    out
}

/// A review is told nothing of its earlier attempts but this: the kernel's
/// one ask to inline a reproduction it cited from its sandbox.
pub fn asked(feedback: Option<&str>) -> Option<&str> {
    feedback.filter(|f| f.starts_with(REPRODUCTION_ASK))
}

/// When a review's attempt failed only for citing its sandbox, and it has
/// not been asked yet (`previous` is not already the ask), the feedback
/// that asks it, once, to inline the reproduction.
pub fn reask(checks: &[CheckResult], previous: Option<&str>, task_id: i64) -> Option<String> {
    if asked(previous).is_some() {
        return None;
    }
    let failed: Vec<&CheckResult> = checks
        .iter()
        .filter(|c| !c.ok && c.level != "note")
        .collect();
    let [row] = failed.as_slice() else {
        return None;
    };
    if row.name != Rule::ReproductionSelfContained.name() {
        return None;
    }
    Some(format!(
        "{REPRODUCTION_ASK}{}. Those files lived in your sandbox and are gone when this attempt ends; the follow-up \
         task starts from a fresh clone and cannot run them. Review again and, if the defect stands, demote with the \
         reproduction inline: the exact commands and inputs in the question (a heredoc such as `python3 - <<'EOF' ... EOF`), \
         or the names of existing tests, or a file you write under {} (committed or not), which Forge attaches to the \
         demotion verbatim. Cite no path under /tmp, your home directory, or any file the commit does not contain.",
        row.tail,
        notes_dir(task_id)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn a_demotion_citing_tmp_or_home_is_refused() {
        let text = "The command python3 /tmp/forge855-review.py demonstrates that rejecting the cursor update loses the event";
        assert_eq!(sandbox_refs(text, &none), vec!["/tmp/forge855-review.py"]);
        assert_eq!(
            sandbox_refs("run `bash ~/repro.sh` and see", &none),
            vec!["~/repro.sh"]
        );
        assert_eq!(
            sandbox_refs("see $HOME/r.py, /home/ronin/x.txt and /root/y", &none),
            vec!["$HOME/r.py", "/home/ronin/x.txt", "/root/y"]
        );
        assert_eq!(
            sandbox_refs("output went to '/var/tmp/out.log'.", &none),
            vec!["/var/tmp/out.log"]
        );
        assert_eq!(
            sandbox_refs("cat >/tmp/a.txt then read /tmp/b.txt", &none),
            vec!["/tmp/b.txt"]
        );
    }

    #[test]
    fn an_inline_reproduction_is_self_contained() {
        let heredoc = "`forge show 3` omits the reason:\n```\ncat > /tmp/r.py <<'EOF'\nprint(1)\nEOF\npython3 /tmp/r.py\n```";
        assert!(
            sandbox_refs(heredoc, &none).is_empty(),
            "{:?}",
            sandbox_refs(heredoc, &none)
        );
        let piped = "python3 - <<'EOF'\nimport json; print(json.dumps({}))\nEOF\nprints {} but cargo test store::tasks fails";
        assert!(sandbox_refs(piped, &none).is_empty());
        let tee = "printf 'x' | tee /tmp/in.txt; ./target/debug/forge add /tmp/in.txt";
        assert!(sandbox_refs(tee, &none).is_empty());
        assert!(
            sandbox_refs(
                "answer.txt is 42; `xxd answer.txt` shows no newline, see https://x.io/a/b",
                &none
            )
            .is_empty()
        );
    }

    #[test]
    fn a_demotion_citing_an_uncommitted_file_is_refused() {
        let uncommitted = |p: &str| p == "repro.py" || p == "tests/review-notes/9/gone.sh";
        assert_eq!(
            sandbox_refs("run `python3 ./repro.py` against src/lib.rs", &uncommitted),
            vec!["./repro.py"]
        );
        assert_eq!(
            sandbox_refs("bash tests/review-notes/9/gone.sh fails", &uncommitted),
            vec!["tests/review-notes/9/gone.sh"]
        );
    }

    #[test]
    fn the_reviewer_is_asked_once_and_only_for_its_citation() {
        let row = |name: &str| CheckResult {
            level: "L0".into(),
            name: name.into(),
            ok: false,
            tail: "the demotion cites what a fresh clone does not have: /tmp/x.py".into(),
            ..Default::default()
        };
        let ask = reask(&[row("reproduction-self-contained")], None, 7).unwrap();
        assert!(
            ask.starts_with(REPRODUCTION_ASK) && ask.contains("/tmp/x.py"),
            "{ask}"
        );
        assert!(ask.contains("tests/review-notes/7/"), "{ask}");
        assert_eq!(asked(Some(&ask)), Some(ask.as_str()));
        assert!(reask(&[row("reproduction-self-contained")], Some(&ask), 7).is_none());
        assert!(reask(&[row("no-writes")], None, 7).is_none());
        assert!(
            reask(
                &[row("reproduction-self-contained"), row("clean-tree")],
                None,
                7
            )
            .is_none()
        );
        assert!(asked(Some("The previous attempt failed verification")).is_none());
    }
}
