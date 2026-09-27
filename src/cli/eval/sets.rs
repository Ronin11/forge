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

/// The kind a recorded concierge decision names.
fn kind_of(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v["kind"].as_str().map(str::to_string))
}

/// Every message the concierge decided, labeled by the kind it recorded,
/// oldest first: each of its own runs (whose plan is the decision), each
/// `decisions` row it answered, and each task filed through it, the latter
/// two only where no run already stands for them.
fn concierge(f: &Forge) -> Result<Vec<Item>> {
    let mut items: Vec<(i64, Item)> = Vec::new();
    let mut runs = std::collections::HashSet::new();
    let mut plans = std::collections::HashSet::new();
    for t in f.store.concierge_runs()? {
        let Some(kind) = kind_of(&t.plan) else {
            continue;
        };
        runs.insert(t.id);
        plans.insert(t.plan.clone());
        items.push((
            t.id,
            Item {
                state: t.task.clone(),
                label: kind,
            },
        ));
    }
    for d in f.store.decisions_answered_by("concierge")? {
        if d.task_id.is_some_and(|id| runs.contains(&id)) {
            continue;
        }
        items.push((
            d.task_id.unwrap_or(i64::MAX),
            Item {
                state: d.question,
                label: "question".into(),
            },
        ));
    }
    for t in f.store.concierge_tasks()? {
        let raw = t.concierge_json.as_deref().unwrap_or_default();
        if plans.contains(raw) {
            continue;
        }
        let Some(kind) = kind_of(raw) else {
            continue;
        };
        items.push((
            t.id,
            Item {
                state: message_of(&t, &kind),
                label: kind,
            },
        ));
    }
    items.sort_by_key(|(id, _)| *id);
    Ok(items.into_iter().map(|(_, item)| item).collect())
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

    fn forge_on(dir: &tempfile::TempDir, store: Store) -> Forge {
        Forge::open_with(
            Paths {
                home: dir.path().to_path_buf(),
                worktrees: dir.path().join("worktrees"),
                logs: dir.path().join("logs"),
            },
            store,
        )
        .unwrap()
    }

    #[test]
    fn the_concierge_set_is_every_run_decision_and_filed_task_labeled_by_its_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        // As `concierge::ask` does: insert, then record the decision.
        let file = |t: Task| {
            let id = store.insert_task(&t).unwrap();
            store.update_task(&Task { id, ..t }).unwrap();
            id
        };
        let run = |message: &str, plan: &str| {
            file(Task {
                repo: "r".into(),
                task: message.into(),
                workflow: "concierge".into(),
                plan: plan.into(),
                state: TaskState::Succeeded,
                ..Default::default()
            })
        };
        let request = r#"{"kind":"request","task":"change the quote text"}"#;
        run("make the quote say usually same day", request);
        file(Task {
            repo: "r".into(),
            task: "change the quote text".into(),
            title: Some("make the quote say usually same day".into()),
            concierge_json: Some(request.into()),
            ..Default::default()
        });
        let asked = run(
            "did the reminder go out?",
            r#"{"kind":"question","answer":"yes"}"#,
        );
        let answer = |task_id: i64, question: &str| {
            store
                .insert_decision_by(InsertDecisionBy {
                    task_id,
                    repo: "r",
                    question,
                    answer: "yes",
                    answered_by: "concierge",
                    citations: "",
                    answered_for: None,
                })
                .unwrap();
        };
        answer(asked, "did the reminder go out?");
        run("I keep losing track of quotes", r#"{"kind":"need"}"#);
        // A task filed before its run was kept, and an answer whose run is gone.
        let old = file(Task {
            repo: "r".into(),
            task: "what is this? Contact: Ann.".into(),
            concierge_json: Some(r#"{"kind":"unclear","question":"which?"}"#.into()),
            ..Default::default()
        });
        answer(old, "is it booked?");
        let items = concierge(&forge_on(&dir, store)).unwrap();
        let got: Vec<(&str, &str)> = items
            .iter()
            .map(|i| (i.state.as_str(), i.label.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("make the quote say usually same day", "request"),
                ("did the reminder go out?", "question"),
                ("I keep losing track of quotes", "need"),
                ("is it booked?", "question"),
                ("what is this? Contact: Ann.", "unclear"),
            ]
        );
    }

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
            let items = demotions(&forge_on(&dir, store)).unwrap();
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
