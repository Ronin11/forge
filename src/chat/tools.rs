//! The fixed tool set: deterministic code, never a shell. `call` is the
//! whole dispatch — a tool name and its JSON arguments in, a redacted,
//! bounded answer out, or for a write verb a proposal that changes
//! nothing. A name that is not in `TOOLS` is an error the model reads,
//! not a fallback to anything else.

use super::actions;
use super::reads;
use super::redact::Redactor;
use crate::ctx::Forge;
use anyhow::{Result, bail};
use serde_json::Value;

/// A tool result never carries more than this many bytes of JSON.
const RESULT_BYTES: usize = 12 * 1024;

pub struct Tool {
    pub name: &'static str,
    pub write: bool,
    /// What it does and what it takes, as the model reads it.
    pub about: &'static str,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "task",
        write: false,
        about: "One task: its state and reason, every attempt with the checks that failed and their first lines, its operations, and the kernel's diagnosis. {\"id\": 903}",
    },
    Tool {
        name: "attempt",
        write: false,
        about: "One attempt of a task in full: its verdict rows, the agent's summary, claims and changes. {\"task\": 903, \"attempt_no\": 2}",
    },
    Tool {
        name: "tasks",
        write: false,
        about: "The queue, newest first (forge log). All optional: {\"state\": \"failed\", \"project\": \"p\", \"initiative\": 56, \"grep\": \"text\", \"before\": 900, \"limit\": 15}",
    },
    Tool {
        name: "initiative",
        write: false,
        about: "One initiative: its outcome, state and hold, each lineage's latest task, open questions and rulings. {\"id\": 56}",
    },
    Tool {
        name: "initiatives",
        write: false,
        about: "Every initiative with counts by state. {\"project\": \"p\"} optional.",
    },
    Tool {
        name: "decisions",
        write: false,
        about: "Recorded answers and rulings, newest first. {\"task\": 903, \"grep\": \"text\", \"limit\": 15} all optional.",
    },
    Tool {
        name: "requests",
        write: false,
        about: "Every task blocked on a question, and what it is waiting on. {}",
    },
    Tool {
        name: "doctor",
        write: false,
        about: "Forge's health checks; the ones that are fine are only named. {}",
    },
    Tool {
        name: "stats",
        write: false,
        about: "Spend in the last 24 hours against the per-day cap, daily landings and cost, per-workflow and per-role figures. {\"days\": 7} optional.",
    },
    Tool {
        name: "log_tail",
        write: false,
        about: "The last lines of an attempt's log. {\"task\": 903, \"attempt_no\": 2, \"lines\": 20}; attempt_no and lines optional.",
    },
    Tool {
        name: "events_since",
        write: false,
        about: "The event log: {\"cursor\": \"0:0\", \"since_ts\": 1700000000, \"task\": 903, \"limit\": 30}, all optional; the answer's `next` continues from where it stopped.",
    },
    Tool {
        name: "add_task",
        write: true,
        about: "PROPOSE filing a task; the operator confirms before anything is filed. {\"task\": \"what to do\", \"project\": \"p\", \"workflow\": \"w\", \"initiative\": 56, \"after\": [900]}; task and (project or initiative) required.",
    },
    Tool {
        name: "answer_question",
        write: true,
        about: "PROPOSE answering a blocked task's question; the operator confirms. {\"task\": 903, \"answer\": \"text\"}",
    },
    Tool {
        name: "retry_task",
        write: true,
        about: "PROPOSE retrying a finished task as a new one; the operator confirms. {\"task\": 903, \"workflow\": \"w\", \"again\": false}; workflow and again optional.",
    },
];

/// The tool list as the system prompt carries it: rendered from `TOOLS`,
/// so the model is told exactly what dispatch will accept.
pub fn catalog() -> String {
    let mut out = String::from("The tools (read tools answer; write tools only propose):\n");
    for t in TOOLS {
        out.push_str(&format!("- {}: {}\n", t.name, t.about));
    }
    out
}

/// What a tool call came to.
#[derive(Debug)]
pub enum Called {
    /// A read tool's answer.
    Answer(Value),
    /// A write tool's proposal: normalized arguments and the summary.
    Proposal { arguments: Value, summary: String },
}

pub fn is_write(name: &str) -> bool {
    TOOLS.iter().any(|t| t.write && t.name == name)
}

/// Every secret value Forge holds, for the redactor: the projects'
/// secrets, the providers' environment, and the keys their `api_key_env`
/// name in this process's environment.
pub fn redactor(f: &Forge) -> Redactor {
    let mut secrets: Vec<String> = f
        .project_secrets
        .values()
        .flat_map(|m| m.values().cloned())
        .collect();
    for p in f.providers.values() {
        for reference in [&p.api_key, &p.account_id, &p.cloudflare_api_key]
            .into_iter()
            .flatten()
        {
            if let Ok(Some(value)) =
                crate::secret_store::resolve_at(&f.paths.home, Some(reference), None)
            {
                secrets.push(value);
            }
        }
        secrets.extend(p.env.iter().map(|(_, v)| v.clone()));
        secrets.extend(
            [&p.api_key_env, &p.account_id_env, &p.cloudflare_key_env]
                .into_iter()
                .flatten()
                .filter_map(|var| std::env::var(var).ok()),
        );
    }
    if let Ok(t) = std::fs::read_to_string(f.paths.home.join("web.token")) {
        secrets.push(t.trim().to_string());
    }
    Redactor::new(secrets)
}

/// Run one tool call. The answer is redacted and bounded here, once, so
/// nothing a tool returns reaches the model or the record raw.
pub fn call(f: &Forge, red: &Redactor, name: &str, args: &Value) -> Result<Called> {
    if !args.is_object() {
        bail!("`arguments` is a JSON object; got {args}");
    }
    let answer = match name {
        "task" => reads::task(f, args),
        "attempt" => reads::attempt(f, args),
        "tasks" => reads::tasks(f, args),
        "initiative" => reads::initiative(f, args),
        "initiatives" => reads::initiatives(f, args),
        "decisions" => reads::decisions(f, args),
        "requests" => reads::requests(f, args),
        "doctor" => reads::doctor(f, args),
        "stats" => reads::stats(f, args),
        "log_tail" => reads::log_tail(f, args),
        "events_since" => reads::events_since(f, args),
        w if is_write(w) => {
            let (arguments, summary) = actions::propose(f, w, args)?;
            return Ok(Called::Proposal {
                arguments: red.value(&arguments),
                summary: red.text(&summary),
            });
        }
        other => bail!(
            "no tool named {other:?}; the tools are {}",
            TOOLS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
        ),
    }?;
    Ok(Called::Answer(reads::bound(
        red.value(&answer),
        RESULT_BYTES,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Store, Task, TaskState};
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    fn seed(f: &Forge, state: TaskState, reason: &str) -> i64 {
        f.store
            .insert_task(&Task {
                repo: "/repo".into(),
                task: "make the thing".into(),
                base_branch: "main".into(),
                model: "m".into(),
                max_turns: 1,
                max_attempts: 2,
                timeout_secs: 60,
                state,
                reason: reason.into(),
                workflow: "direct".into(),
                created_at: crate::unix_now(),
                ..Default::default()
            })
            .unwrap()
    }

    fn red(f: &Forge) -> Redactor {
        redactor(f)
    }

    #[test]
    fn every_tool_in_the_catalog_is_dispatched_and_nothing_else_is() {
        let (_d, f) = fixture();
        let r = red(&f);
        for t in TOOLS {
            let e = call(&f, &r, t.name, &json!({}))
                .err()
                .map(|e| e.to_string());
            if let Some(e) = e {
                assert!(
                    !e.contains("no tool named"),
                    "{} is not dispatched: {e}",
                    t.name
                );
            }
        }
        for bad in ["bash", "shell", "git", "read_file", "", "Task"] {
            let e = call(&f, &r, bad, &json!({})).unwrap_err().to_string();
            assert!(e.contains("no tool named"), "{bad}: {e}");
            assert!(
                e.contains("task, attempt"),
                "the error lists the tools: {e}"
            );
        }
    }

    #[test]
    fn the_catalog_names_every_tool_and_marks_the_writes_as_proposals() {
        let text = catalog();
        for t in TOOLS {
            assert!(text.contains(&format!("- {}:", t.name)), "{}", t.name);
        }
        let writes: Vec<&str> = TOOLS.iter().filter(|t| t.write).map(|t| t.name).collect();
        assert_eq!(writes, ["add_task", "answer_question", "retry_task"]);
        for t in TOOLS.iter().filter(|t| t.write) {
            assert!(t.about.contains("PROPOSE"), "{}", t.name);
            assert!(is_write(t.name));
        }
        assert!(!is_write("task"));
    }

    #[test]
    fn arguments_must_be_an_object() {
        let (_d, f) = fixture();
        let e = call(&f, &red(&f), "task", &json!([1]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("JSON object"), "{e}");
    }

    #[test]
    fn the_task_tool_answers_from_the_store_and_reports_a_missing_id() {
        let (_d, f) = fixture();
        let id = seed(&f, TaskState::Failed, "test failed: cargo test exited 101");
        let Called::Answer(v) = call(&f, &red(&f), "task", &json!({"id": id})).unwrap() else {
            panic!("a read tool answers");
        };
        assert_eq!(v["task"]["id"], id);
        assert_eq!(v["task"]["state"], "failed");
        assert!(v["task"]["reason"].as_str().unwrap().contains("exited 101"));
        // Whitelisted: the worktree path and the workflow's text are not offered.
        assert!(v["task"].get("worktree").is_none());
        assert!(v["task"].get("workflow_text").is_none());
        let e = call(&f, &red(&f), "task", &json!({"id": id + 100})).unwrap_err();
        assert!(e.to_string().contains("no task"), "{e}");
    }

    #[test]
    fn a_secret_in_a_task_never_reaches_a_tool_result() {
        let (_d, f) = fixture();
        let id = seed(
            &f,
            TaskState::Failed,
            "deploy failed with key sk-ant-api03-abcdefghijklmnop and password=hunter22",
        );
        let Called::Answer(v) = call(&f, &red(&f), "task", &json!({"id": id})).unwrap() else {
            panic!();
        };
        let s = v.to_string();
        assert!(!s.contains("sk-ant-api03"), "{s}");
        assert!(!s.contains("hunter22"), "{s}");
        assert!(s.contains("[redacted]"), "{s}");
    }

    #[test]
    fn a_configured_project_secret_is_masked_in_results() {
        let (_d, mut f) = fixture();
        f.project_secrets.insert(
            "p".into(),
            [("DB_URL".to_string(), "postgres-live-value-123".to_string())].into(),
        );
        let id = seed(
            &f,
            TaskState::Failed,
            "connect to postgres-live-value-123 refused",
        );
        let Called::Answer(v) = call(&f, &red(&f), "task", &json!({"id": id})).unwrap() else {
            panic!();
        };
        assert!(!v.to_string().contains("postgres-live-value-123"), "{v}");
    }

    #[test]
    fn a_list_tool_bounds_its_rows() {
        let (_d, f) = fixture();
        for _ in 0..5 {
            seed(&f, TaskState::Queued, "");
        }
        let Called::Answer(v) = call(&f, &red(&f), "tasks", &json!({"limit": 2})).unwrap() else {
            panic!();
        };
        assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
        let e = call(&f, &red(&f), "tasks", &json!({"state": "nonsense"})).unwrap_err();
        assert!(e.to_string().contains("state"), "{e}");
    }

    #[test]
    fn log_tail_reads_only_under_the_logs_directory() {
        use crate::store::Attempt;
        let (d, f) = fixture();
        let id = seed(&f, TaskState::Failed, "x");
        let inside = f.paths.logs.join("a.jsonl");
        std::fs::write(&inside, "one\ntwo password=hunter22\nthree\n").unwrap();
        let outside = d.path().join("outside.txt");
        std::fs::write(&outside, "not a log\n").unwrap();
        let attempt = |no: i64, path: &std::path::Path| {
            f.store
                .insert_attempt(&Attempt {
                    task_id: id,
                    attempt_no: no,
                    step: "code".into(),
                    provider: "anthropic".into(),
                    started_at: crate::unix_now(),
                    log_path: path.display().to_string(),
                    ..Default::default()
                })
                .unwrap()
        };
        attempt(1, &inside);
        attempt(2, &outside);
        let Called::Answer(v) = call(
            &f,
            &red(&f),
            "log_tail",
            &json!({"task": id, "attempt_no": 1, "lines": 2}),
        )
        .unwrap() else {
            panic!();
        };
        let lines = v["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(!v.to_string().contains("hunter22"), "{v}");
        let e = call(
            &f,
            &red(&f),
            "log_tail",
            &json!({"task": id, "attempt_no": 2}),
        )
        .unwrap_err();
        assert!(e.to_string().contains("not under FORGE_HOME/logs"), "{e}");
    }

    #[test]
    fn a_write_tool_proposes_and_changes_nothing() {
        let (_d, f) = fixture();
        let id = seed(&f, TaskState::Blocked, "needs an answer: which port?");
        let before = f.store.task(id).unwrap().unwrap();
        let Called::Proposal { arguments, summary } = call(
            &f,
            &red(&f),
            "answer_question",
            &json!({"task": id, "answer": "8080"}),
        )
        .unwrap() else {
            panic!("a write tool proposes");
        };
        assert_eq!(arguments["answer"], "8080");
        assert!(summary.contains("Answer task"), "{summary}");
        let after = f.store.task(id).unwrap().unwrap();
        assert_eq!(after.state, before.state);
        assert!(f.store.decisions(&Default::default()).unwrap().is_empty());
    }

    #[test]
    fn a_proposal_is_checked_against_the_record_when_it_is_made() {
        let (_d, f) = fixture();
        let running = seed(&f, TaskState::Running, "");
        let done = seed(&f, TaskState::Succeeded, "");
        let r = red(&f);
        for (tool, args, want) in [
            (
                "answer_question",
                json!({"task": done, "answer": "x"}),
                "only a blocked task",
            ),
            (
                "answer_question",
                json!({"task": 9999, "answer": "x"}),
                "no task",
            ),
            (
                "answer_question",
                json!({"task": done}),
                "`answer` is required",
            ),
            (
                "retry_task",
                json!({"task": running}),
                "only a finished task",
            ),
            ("retry_task", json!({"task": 9999}), "no task"),
            ("add_task", json!({"task": "x"}), "give the `project`"),
            ("add_task", json!({"project": "p"}), "`task`"),
            (
                "add_task",
                json!({"task": "x", "project": "nope"}),
                "lists no repository",
            ),
            (
                "add_task",
                json!({"task": "x", "repo": "/etc"}),
                "not a repository any project lists",
            ),
        ] {
            let e = call(&f, &r, tool, &args).unwrap_err().to_string();
            assert!(e.contains(want), "{tool} {args}: {e}");
        }
    }
}
