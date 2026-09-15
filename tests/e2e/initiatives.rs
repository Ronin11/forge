use crate::support::*;

/// Write a directive named `investigate-filer` that behaves exactly like
/// the built-in `investigate` (plan contract, reads without writing)
/// except that it sets `file_into_initiative = true`, plus a workflow
/// `filer` that runs it and then `code`. The code step must never run
/// when the task has an initiative id: `neverrun.sh` fails loudly if it
/// does.
fn write_filer_workflow(e: &Env) {
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/investigate-filer.toml"),
        "name = \"investigate-filer\"\nkind = \"directive\"\ncontract = \"plan\"\n\
         description = \"investigates and files its plan into the task's initiative\"\n\
         consumes = [\"branch\"]\nproduces = [\"plan\"]\nmax_turns = 25\n\
         file_into_initiative = true\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/filer.toml"),
        "name = \"filer\"\ndescription = \"d\"\n\
         steps = [{ action = \"investigate-filer\" }, { action = \"code\" }]\n\
         [meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
}

/// Parse `created initiative N` from `forge initiative new`'s stdout.
fn created_id(o: &std::process::Output) -> i64 {
    let out = String::from_utf8_lossy(&o.stdout);
    out.lines()
        .find_map(|l| l.strip_prefix("created initiative "))
        .unwrap_or_else(|| panic!("{out}"))
        .parse()
        .unwrap()
}

#[test]
fn an_initiative_from_a_file_lands_a_dependency_and_settles_done_with_the_event() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    std::fs::write(
        &file,
        "write 42 to answer.txt\n\nafter: 1\nadd an extra file",
    )
    .unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "answer.txt says 42 everywhere it matters",
            "--from",
            file.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    // Both tasks landed in the initiative, the second waiting on the first.
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, "[1]");
    let initiatives: (Option<i64>, Option<i64>) = e
        .db()
        .query_row(
            "SELECT (SELECT initiative FROM tasks WHERE id=1), (SELECT initiative FROM tasks WHERE id=2)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(initiatives, (Some(id), Some(id)));

    // `ok.sh` lands the first task, then `addfile.sh` gives the second a
    // real change of its own (the base already has `answer.txt`, so
    // `ok.sh` again would report a change git does not see).
    assert!(
        e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "succeeded");
    assert!(e.forge("addfile.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");

    // The initiative settled once the last task reached a terminal state.
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    let settled = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == "initiative_settled")
        .unwrap_or_else(|| panic!("no initiative_settled event in:\n{events}"));
    assert_eq!(settled["id"], id);
    assert_eq!(settled["state"], "done");

    let (settled_at,): (Option<i64>,) = e
        .db()
        .query_row(
            "SELECT settled_at FROM initiatives WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?,)),
        )
        .unwrap();
    assert!(settled_at.is_some());

    let report = e.forge(
        "ok.sh",
        &["initiative", "report", &id.to_string(), "--json"],
    );
    let doc: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(doc["state"], "done");
    assert_eq!(doc["outcome"], "answer.txt says 42 everywhere it matters");
    assert_eq!(doc["tasks"].as_array().unwrap().len(), 2);
}

#[test]
fn same_rule_failures_hold_the_initiative_and_the_report_names_the_rule() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    // `noop.sh` commits nothing, so every one of these fails L0's
    // has-commits rule, deterministically and independently.
    std::fs::write(&file, "first task\n\nsecond task\n\nthird task").unwrap();

    let o = e.forge(
        "noop.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "three independent things get done",
            "--from",
            file.to_str().unwrap(),
            "--stop-after",
            "2",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    assert!(e.forge("noop.sh", &["work", "--once"]).status.success());

    assert_eq!(e.task(1).0, "failed");
    assert_eq!(e.task(2).0, "failed");
    assert!(
        e.task(1).1.starts_with("L0 failed: has-commits"),
        "{}",
        e.task(1).1
    );
    assert!(
        e.task(2).1.starts_with("L0 failed: has-commits"),
        "{}",
        e.task(2).1
    );
    // The third task never got a chance: the worker stopped claiming this
    // initiative's tasks after two same-rule failures in a row.
    assert_eq!(e.task(3).0, "queued");

    let o = e.forge(
        "noop.sh",
        &["initiative", "show", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["state"], "held");
    assert_eq!(row["held_rule"], "has-commits");

    let o = e.forge(
        "noop.sh",
        &["initiative", "report", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["state"], "held");
    assert_eq!(doc["held_rule"], "has-commits");
    let refused = doc["refused"].as_array().unwrap();
    assert!(
        refused
            .iter()
            .any(|r| r["rule"] == "has-commits" && r["count"].as_i64().unwrap() >= 2),
        "{refused:?}"
    );

    // The text form names the rule too.
    let o = e.forge("noop.sh", &["initiative", "report", &id.to_string()]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("held"), "{out}");
    assert!(out.contains("has-commits"), "{out}");
}

#[test]
fn a_task_blocked_on_a_question_keeps_the_initiative_open() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    std::fs::write(&file, "write an answer").unwrap();

    let o = e.forge(
        "commitneedsinput.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "an answer is on record",
            "--from",
            file.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    // The task commits an answer, then asks the operator a question
    // instead of finishing: not terminal, not landed, not withdrawn.
    assert!(
        e.forge("commitneedsinput.sh", &["work", "--once"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "blocked");

    let o = e.forge(
        "commitneedsinput.sh",
        &["initiative", "show", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["state"], "open");
    assert_eq!(row["blocked"], 1);
    assert_eq!(row["settled_at"], serde_json::Value::Null);

    // No settlement event fired for a still-blocked initiative.
    let events_path = e.home.join("events.jsonl");
    if events_path.exists() {
        let events = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            !events
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .any(|v| v["type"] == "initiative_settled"),
            "{events}"
        );
    }
}

#[test]
fn from_plan_refuses_a_task_with_no_recorded_plan() {
    let e = Env::new();
    let o = run_wf(&e, "ok.sh", &[], "direct", "write 42 to answer.txt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");

    let o = e.forge("ok.sh", &["initiative", "from-plan", "1"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("no recorded plan"), "{err}");

    // Nothing was created: no initiative, no sibling tasks.
    let count: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn from_plan_creates_one_task_per_plan_item_chained_in_order() {
    let e = Env::new();
    // The `planned` workflow's investigate step records a three-item
    // plan (see `planner-multi.sh`); the coder then finishes the task.
    let o = run_wf(
        &e,
        "promptdump.sh",
        &[("FORGE2_CLAUDE_BIN_INVESTIGATE", "planner-multi.sh")],
        "planned",
        "make the answer 42",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    let origin_project: Option<String> = e
        .db()
        .query_row("SELECT project FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert!(origin_project.is_some());

    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "from-plan",
            "1",
            "--outcome",
            "answer.txt says 42 and the plan's steps are all on record",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    // Three plan items, three tasks (2, 3, 4), chained to one another in
    // order; the first has no dependency of its own.
    let count: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    for (id_str, expect_after) in [("2", "[]"), ("3", "[2]"), ("4", "[3]")] {
        let after: String = e
            .db()
            .query_row(
                &format!("SELECT after_json FROM tasks WHERE id={id_str}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, expect_after, "task {id_str}");
    }

    // Filed against the originating task's repo and project, in the new
    // initiative.
    let (repo2, project2, init2): (String, Option<String>, Option<i64>) = e
        .db()
        .query_row(
            "SELECT repo, project, initiative FROM tasks WHERE id=2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(repo2, e.repo.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(project2, origin_project);
    assert_eq!(init2, Some(id));

    // Every filed task carries a `plan` reference to task 1.
    for tid in [2, 3, 4] {
        let o = e.forge("ok.sh", &["ref", "list", &tid.to_string(), "--json"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let refs: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let refs = refs.as_array().unwrap();
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0]["kind"], "plan");
        assert_eq!(refs[0]["url"], "forge://task/1");
    }

    let o = e.forge(
        "ok.sh",
        &["initiative", "report", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        doc["outcome"],
        "answer.txt says 42 and the plan's steps are all on record"
    );
    assert_eq!(doc["tasks"].as_array().unwrap().len(), 3);
}

#[test]
fn from_plan_defaults_the_outcome_to_the_tasks_own_text() {
    let e = Env::new();
    let o = run_wf(
        &e,
        "promptdump.sh",
        &[("FORGE2_CLAUDE_BIN_INVESTIGATE", "planner-multi.sh")],
        "planned",
        "make the answer 42, please",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge("ok.sh", &["initiative", "from-plan", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    let outcome: String = e
        .db()
        .query_row("SELECT outcome FROM initiatives WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(outcome, "make the answer 42, please");
}

#[test]
fn file_into_initiative_files_siblings_and_the_origin_task_ends_succeeded() {
    let e = Env::new();
    write_filer_workflow(&e);
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );
    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "the plan's steps are all filed",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let iid = created_id(&o);

    let o = e
        .with_role("neverrun.sh", "INVESTIGATE_FILER", "planner-multi.sh")
        .args([
            "run",
            "--no-land",
            repo,
            "make the answer 42",
            "--workflow",
            "filer",
            "--initiative",
            &iid.to_string(),
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // The origin task ends succeeded, without pushing anything (the plan
    // contract is untouched, and `code` never ran), and its reason says
    // how many it filed.
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(!pushed, "nothing to push: the plan step changed nothing");
    assert!(
        reason.contains("filed 3 task(s) into initiative"),
        "{reason}"
    );
    assert!(reason.contains(&iid.to_string()), "{reason}");

    // Three siblings, chained, in the same initiative, each referencing
    // the origin task as its plan's source.
    let count: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    for (id_str, expect_after) in [("2", "[]"), ("3", "[2]"), ("4", "[3]")] {
        let (after, init, state): (String, Option<i64>, String) = e
            .db()
            .query_row(
                &format!("SELECT after_json, initiative, state FROM tasks WHERE id={id_str}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(after, expect_after, "task {id_str}");
        assert_eq!(init, Some(iid), "task {id_str}");
        assert_eq!(state, "queued", "task {id_str} was not run");
    }
    for tid in [2, 3, 4] {
        let o = e.forge("ok.sh", &["ref", "list", &tid.to_string(), "--json"]);
        let refs: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let refs = refs.as_array().unwrap();
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0]["kind"], "plan");
        assert_eq!(refs[0]["url"], "forge://task/1");
    }
}

#[test]
fn file_into_initiative_without_an_initiative_id_runs_code_as_usual() {
    let e = Env::new();
    write_filer_workflow(&e);
    // No initiative: the flag has nothing to file into, so the workflow
    // runs its `code` step exactly as if the flag were absent.
    let o = e
        .with_role("ok.sh", "INVESTIGATE_FILER", "planner-multi.sh")
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "make the answer 42",
            "--workflow",
            "filer",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(!reason.contains("filed"), "{reason}");
    let count: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "no siblings without an initiative id");
}
