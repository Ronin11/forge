//! The journal: everything earlier in a piece of work, for the next
//! agent and for the operator. One derivation walks the lineage and
//! yields an entry per attempt with the kernel's verdict, what the checks
//! found, and what the agent claimed; the prose form the agents read is
//! rendered from those entries, verdict first, cut to a budget from the
//! oldest end. The test author's words never reach the coder through it:
//! what the coder learns of the hidden tests is the interface alone.

use crate::ctx::Forge;
use crate::engine::{Classify, Fault};
use crate::store::{AttemptState, Task};

/// One attempt in a piece of work's journal, as data rather than prose.
#[derive(serde::Serialize, Debug, Clone)]
pub struct JournalEntry {
    pub task: i64,
    pub attempt: i64,
    pub step: String,
    pub state: String,
    /// What the agent said it did; never for the tests step.
    pub said: Option<String>,
    /// The failed checks, one line each: level, name, what went wrong.
    pub found: Vec<String>,
    /// The attempt's reason as recorded: why it ended without a result,
    /// or the question it stopped with.
    pub reason: String,
}

impl JournalEntry {
    /// The kernel's verdict on the attempt, in the words the prose uses.
    pub fn verdict(&self) -> &'static str {
        match AttemptState::try_from(self.state.as_str()) {
            Ok(AttemptState::Succeeded) => "verified",
            Ok(AttemptState::ChecksFailed) => "rejected by the checks",
            Ok(AttemptState::AgentFailed) => "ended without a result",
            Ok(AttemptState::NeedsInput) => "stopped with a question",
            Ok(AttemptState::Unverified) => "unverified",
            _ => "running",
        }
    }
}

/// The lineage as structured entries, one per finished attempt, across
/// every task in the piece of work, oldest first.
pub fn entries_for(f: &Forge, t: &Task) -> Result<Vec<JournalEntry>, Fault> {
    let lineage = f.store.lineage(t.id).env()?;
    let mut entries = Vec::new();
    for l in &lineage {
        let attempts = f.store.attempts(l.id).env()?;
        for a in attempts.iter().filter(|a| a.state != AttemptState::Running) {
            let said = if a.step == "tests" {
                None
            } else {
                serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json)
                    .ok()
                    .map(|e| e.summary)
                    .filter(|s| !s.trim().is_empty())
            };
            let found: Vec<String> =
                serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json)
                    .unwrap_or_default()
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| {
                        let what = if c.failing_tests.is_empty() {
                            salient_line(&c.tail)
                        } else {
                            c.failing_tests
                                .iter()
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("; ")
                        };
                        format!("{} {}: {}", c.level, c.name, what)
                    })
                    .collect();
            entries.push(JournalEntry {
                task: l.id,
                attempt: a.attempt_no,
                step: a.step.clone(),
                state: a.state.as_str().to_string(),
                said,
                found,
                reason: a.reason.clone(),
            });
        }
    }
    Ok(entries)
}

/// The prose the agents read: every attempt across the lineage, the
/// verdict first, then what the checks found, then what the agent
/// claimed. Cut to a budget from the oldest end. Empty when nothing ran
/// before.
pub fn journal_for(f: &Forge, t: &Task) -> Result<String, Fault> {
    const BUDGET: usize = 6000;
    let lineage = f.store.lineage(t.id).env()?;
    let entries = entries_for(f, t)?;
    let mut lines: Vec<(String, String)> = Vec::new(); // (short line, long line)
    for l in &lineage {
        let own: Vec<&JournalEntry> = entries.iter().filter(|e| e.task == l.id).collect();
        if own.is_empty() {
            continue;
        }
        let head = if l.id == t.id {
            format!("task {} ({}, this task)", l.id, l.workflow)
        } else {
            format!(
                "task {} ({}), {}{}",
                l.id,
                l.workflow,
                l.state,
                if l.reason.is_empty() {
                    String::new()
                } else {
                    format!(": {}", first_line(&l.reason))
                }
            )
        };
        lines.push((head.clone(), head));
        for e in own {
            // Verdict first, then what the checks found, then what the
            // agent claimed: a reader acts on the first two and treats the
            // third as unverified. Measured the other way round, the journal
            // cost an extra attempt on every paired task.
            let short = format!("  {} {:<7} {}", e.attempt, e.step, e.verdict());
            let mut long = short.clone();
            let state = AttemptState::try_from(e.state.as_str()).ok();
            if !e.found.is_empty() {
                long.push_str(&format!(
                    "\n    found:   {}",
                    clip(&e.found.join("; "), 400)
                ));
            } else if state == Some(AttemptState::AgentFailed) {
                long.push_str(&format!("\n    found:   {}", first_line(&e.reason)));
            } else if state == Some(AttemptState::NeedsInput) {
                long.push_str(&format!(
                    "\n    found:   the checks passed; it stopped with: {}",
                    clip(&first_line(&e.reason), 400)
                ));
            } else if state == Some(AttemptState::Succeeded) {
                long.push_str("\n    found:   every check passed");
            }
            if let Some(said) = &e.said {
                long.push_str(&format!("\n    claimed: {}", clip(said, 400)));
            }
            lines.push((short, long));
        }
    }
    if !lines.iter().any(|(short, _)| short.starts_with("  ")) {
        return Ok(String::new());
    }
    // Fit the budget: the newest entries keep their words, the oldest go to a line.
    let mut cut = 0;
    let render = |cut: usize| -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, (short, long))| if i < cut { short.clone() } else { long.clone() })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut body = render(cut);
    while body.len() > BUDGET && cut < lines.len() {
        cut += 1;
        body = render(cut);
    }
    Ok(format!(
        "So far in this piece of work, oldest first. Each attempt's line is the kernel's verdict; `found` is what the checks established and is fact; `claimed` is what that agent said and is unverified. Act on what was found and do not spend turns re-checking claims. Commits from earlier attempts on this task are already on your branch.\n{body}"
    ))
}

/// The line of a check's output that says what went wrong: the first that
/// names a failure, else the last that says anything.
pub fn salient_line(tail: &str) -> String {
    let lines: Vec<String> = tail
        .lines()
        .map(strip_ansi)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| {
            let low = l.to_ascii_lowercase();
            ["fail", "error", "✗", "assert", "panic", "expected"]
                .iter()
                .any(|k| low.contains(k))
        })
        .or(lines.last())
        .cloned()
        .unwrap_or_default()
}

pub fn strip_ansi(line: &str) -> String {
    let mut clean = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    clean
}

/// The first line that says something: blank lines and terminal colour
/// codes skipped, since check output often opens with both.
pub fn first_line(s: &str) -> String {
    s.lines()
        .map(strip_ansi)
        .map(|l| l.trim().to_string())
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

pub fn clip(s: &str, n: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= n {
        one
    } else {
        format!("{}…", one.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salient_and_first_lines_skip_noise() {
        assert_eq!(
            salient_line("\n  building\n\u{1b}[31merror: boom\u{1b}[0m\n done"),
            "error: boom"
        );
        assert_eq!(salient_line("just this"), "just this");
        assert_eq!(first_line("\n\u{1b}[1m\u{1b}[0m\n  hello\n"), "hello");
        assert_eq!(clip("a\nb", 10), "a b");
        assert_eq!(clip("abcdef", 3), "abc…");
    }
}
