//! The three labeled sets `forge eval jev` measures, pulled from the store
//! (or replayed from a recorded file).

use super::*;

fn concierge_criteria() -> Vec<(&'static str, String)> {
    // docs/INTAKE.md, "The front door is not the interview".
    vec![
        (
            "request",
            "they have said exactly what they want built or changed, as in \"make the quote text say 'usually same day'\"; it becomes a task".into(),
        ),
        (
            "question",
            "they ask something to be answered from the project's record, as in \"did the reminder go to the Hendersons?\"; no build".into(),
        ),
        (
            "need",
            "a symptom with a workflow underneath, as in \"I keep losing track of who I've quoted\"; the interview's case".into(),
        ),
        (
            "unclear",
            "it cannot be told which of the three it is; one question would settle it".into(),
        ),
    ]
}

fn size_criteria() -> Vec<(&'static str, String)> {
    vec![
        ("small", format!("changes at most {SMALL_LINES} lines")),
        (
            "medium",
            format!("changes more than {SMALL_LINES} and at most {MEDIUM_LINES} lines"),
        ),
        ("large", format!("changes more than {MEDIUM_LINES} lines")),
    ]
}

/// The empty sets with their questions, in report order.
fn empty_sets() -> Vec<Set> {
    vec![
        Set {
            name: "concierge",
            title: "Concierge decisions",
            kind: "choice",
            instructions: "Sort this customer message into what the concierge should do with it.".into(),
            criteria: concierge_criteria(),
            items: vec![],
        },
        Set {
            name: "demotions",
            title: "Review demotions",
            kind: "noul",
            instructions: "Does this review demotion name a reproducible defect with a command or steps, and ask no question?".into(),
            criteria: vec![
                ("yes", "it names a reproducible defect with a command or steps".into()),
                ("no", "it asks something, or reproduces nothing".into()),
            ],
            items: vec![],
        },
        Set {
            name: "size",
            title: "Task size",
            kind: "score",
            instructions: format!(
                "How large is the change this task asks for, in lines changed at landing (small is at most {SMALL_LINES}, medium at most {MEDIUM_LINES})?"
            ),
            criteria: size_criteria(),
            items: vec![],
        },
    ]
}

/// A concierge task's message: the customer's own words, which a request
/// keeps as its title and a need carries with the contact appended.
fn message_of(t: &crate::store::Task, kind: &str) -> String {
    match kind {
        "request" => t.title.clone().unwrap_or_else(|| t.task.clone()),
        "need" => match t.task.rfind(" Contact: ") {
            Some(i) => t.task[..i].to_string(),
            None => t.task.clone(),
        },
        _ => t.task.clone(),
    }
}

fn concierge(f: &Forge) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for t in f.store.concierge_tasks()? {
        let raw = t.concierge_json.as_deref().unwrap_or_default();
        let Some(kind) = serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| v["kind"].as_str().map(str::to_string))
        else {
            continue;
        };
        items.push(Item {
            state: message_of(&t, &kind),
            label: kind,
        });
    }
    for d in f.store.decisions_answered_by("concierge")? {
        items.push(Item {
            state: d.question,
            label: "question".into(),
        });
    }
    Ok(items)
}

/// Each review demotion, `yes` when the record shows it filed as a task
/// (the demotion-as-task rule) or answered "do it as stated", `no` when it
/// blocked as a question.
fn demotions(f: &Forge) -> Result<Vec<Item>> {
    let filed: std::collections::HashSet<(i64, String)> = f
        .store
        .decisions_of_kind_since("demotion-as-task", 0)?
        .into_iter()
        .filter_map(|d| {
            d.task_id.map(|id| {
                let text = d
                    .question
                    .strip_prefix("review demoted: ")
                    .unwrap_or(&d.question)
                    .trim();
                (id, text.to_string())
            })
        })
        .collect();
    let mut items = Vec::new();
    for r in f.store.question_records(None)? {
        if r.kind != "review" {
            continue;
        }
        let text = r
            .question
            .strip_prefix("review demoted: ")
            .unwrap_or(&r.question)
            .trim()
            .to_string();
        let stated = crate::view::is_as_stated(&r);
        items.push(Item {
            label: if stated || filed.contains(&(r.task_id, text.clone())) {
                "yes"
            } else {
                "no"
            }
            .into(),
            state: text,
        });
    }
    Ok(items)
}

/// The size a change of `lines` lines is labeled with.
pub(super) fn size_label(lines: u64) -> &'static str {
    match lines {
        _ if lines <= SMALL_LINES => "small",
        _ if lines <= MEDIUM_LINES => "medium",
        _ => "large",
    }
}

/// Each landed task's text, labeled by the lines its landing changed
/// against its base; a task whose repository no longer has both commits is
/// left out.
async fn sizes(f: &Forge) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        if t.base_sha.is_empty() {
            continue;
        }
        let Ok(lines) = git::diff_line_total(Path::new(&t.repo), &t.base_sha, &t.landed_sha).await
        else {
            continue;
        };
        items.push(Item {
            state: t.task.clone(),
            label: size_label(lines).into(),
        });
    }
    Ok(items)
}

pub(super) async fn from_store(f: &Forge) -> Result<Vec<Set>> {
    let mut sets = empty_sets();
    sets[0].items = concierge(f)?;
    sets[1].items = demotions(f)?;
    sets[2].items = sizes(f).await?;
    Ok(sets)
}

pub(super) fn from_fixture(path: &Path) -> Result<Vec<Set>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: BTreeMap<String, Vec<Item>> = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a recorded eval set", path.display()))?;
    let mut sets = empty_sets();
    for s in &mut sets {
        s.items = doc.remove(s.name).unwrap_or_default();
    }
    if let Some(unknown) = doc.keys().next() {
        bail!(
            "{} has a set {unknown:?} this eval does not know",
            path.display()
        );
    }
    Ok(sets)
}

pub(super) fn record(path: &Path, sets: &[Set]) -> Result<()> {
    let doc: BTreeMap<&str, &Vec<Item>> = sets.iter().map(|s| (s.name, &s.items)).collect();
    std::fs::write(path, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{
        Attempt, AttemptState, FinishAttempt, InsertDecisionBy, Store, Task, TaskState,
    };

    #[test]
    fn demotion_labels_match_the_task_that_filed_the_follow_up() {
        for prefix in ["", "review demoted: "] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("forge.db")).unwrap();
            let text = "cargo test parse::edge fails with left 3, right 4";
            for index in 0..2 {
                let id = store
                    .insert_task(&Task {
                        repo: "r".into(),
                        task: format!("task {index}"),
                        state: TaskState::Blocked,
                        ..Default::default()
                    })
                    .unwrap();
                let attempt = store
                    .insert_attempt(&Attempt {
                        task_id: id,
                        attempt_no: 1,
                        step: "review".into(),
                        state: AttemptState::NeedsInput,
                        ..Default::default()
                    })
                    .unwrap();
                store
                    .finish_attempt(&FinishAttempt {
                        id: attempt,
                        state: AttemptState::NeedsInput,
                        reason: format!("review demoted: {text}"),
                        finished_at: Some(1),
                        ..Default::default()
                    })
                    .unwrap();
                if index == 0 {
                    let decision = store
                        .insert_decision_by(InsertDecisionBy {
                            task_id: id,
                            repo: "r",
                            question: &format!("{prefix}{text}"),
                            answer: "filed the demotion as a follow-up task",
                            answered_by: "supervisor",
                            citations: "",
                            answered_for: None,
                        })
                        .unwrap();
                    store
                        .set_decision_kind(decision, "demotion-as-task")
                        .unwrap();
                }
            }
            let f = Forge::open_with(
                Paths {
                    home: dir.path().to_path_buf(),
                    worktrees: dir.path().join("worktrees"),
                    logs: dir.path().join("logs"),
                },
                store,
            )
            .unwrap();
            let items = demotions(&f).unwrap();
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.label.as_str())
                    .collect::<Vec<_>>(),
                ["yes", "no"]
            );
            assert!(items.iter().all(|item| item.state == text));
        }
    }
}
