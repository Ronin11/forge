use crate::support::*;
use forge_client::{RequestRow, Snapshot, TaskRow, TraceDoc};

#[test]
fn answer_records_a_decision_and_requeues_with_the_answer_appended() {
    let e = Env::new();
    assert!(!e.run("needsinput.sh", &["--retries", "2"]).status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.starts_with("needs input: Which answer file"),
        "{reason}"
    );

    let o = e.forge("ok.sh", &["answer", "1", "Use answer.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (task_text, retry_of): (String, Option<i64>) = e
        .db()
        .query_row("SELECT task, retry_of FROM tasks WHERE id=2", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(retry_of, Some(1));
    assert_eq!(
        task_text,
        "write 42 to answer.txt\n\nOperator's answer to a question from an earlier attempt: Use answer.txt"
    );

    let (dtask, dq, da): (i64, String, String) = e
        .db()
        .query_row(
            "SELECT task_id, question, answer FROM decisions WHERE task_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(dtask, 1);
    assert_eq!(dq, "Which answer file: answer.txt or ANSWER.txt?");
    assert_eq!(da, "Use answer.txt");

    let o = e.forge("ok.sh", &["decisions"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Which answer file"), "{out}");
    assert!(out.contains("Use answer.txt"), "{out}");

    // `forge show` on the retry prints the decision recorded on the task it
    // retries, right after the lineage line.
    let o = e.forge("ok.sh", &["show", "2"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let lineage_at = out.find("lineage    ").unwrap_or_else(|| panic!("{out}"));
    let decision_at = out
        .find("decision   Which answer file: answer.txt or ANSWER.txt? → Use answer.txt")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(decision_at > lineage_at, "{out}");

    // Only a task blocked with a needs_input question is answered.
    let bad = e.forge("ok.sh", &["answer", "2", "no"]);
    assert!(!bad.status.success());
}

#[test]
fn a_blocked_task_is_withdrawn_and_a_dependent_blocks_with_the_reason() {
    let e = Env::new();
    // Lands (no --no-land) so a dependent may be queued --after it.
    let o = e.forge(
        "needsinput.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--retries",
            "2",
        ],
    );
    assert!(!o.status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.starts_with("needs input: Which answer file"),
        "{reason}"
    );

    // A dependent queued behind the blocked task.
    let dep = e.add(&["--after", "1"]);

    let o = e.forge(
        "ok.sh",
        &["withdraw", "1", "--reason", "superseded by task 9"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("withdrew task 1: superseded by task 9"),
        "{out}"
    );

    let (state, reason, _) = e.task(1);
    assert_eq!(state, "withdrawn");
    assert_eq!(reason, "superseded by task 9");

    // The reason is a decision row, beside supervisor rulings.
    let ds: serde_json::Value = e.decisions_json();
    let d = &ds.as_array().unwrap()[0];
    assert_eq!(d["task_id"], 1);
    assert_eq!(d["answered_by"], "operator");
    assert_eq!(d["answer"], "superseded by task 9");
    assert_eq!(d["retry_id"], 1);
    assert_eq!(d["outcome"], "withdrawn");
    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["decisions"]).stdout).to_string();
    assert!(
        text.contains("A (operator): superseded by task 9"),
        "{text}"
    );
    assert!(text.contains("→ task 1 withdrawn"), "{text}");

    // The worker notices on its next pass and blocks the dependent with the
    // withdrawn reason, the same path a failed or unverified dependency
    // takes; withdraw creates no replacement task to reroute it onto.
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    let (state, reason, _) = e.task(dep);
    assert_eq!(state, "blocked", "{reason}");
    assert!(
        reason.starts_with("waits on task 1 (withdrawn: superseded by task 9)"),
        "{reason}"
    );

    // Withdrawing a running task is refused.
    let running = e.add(&[]);
    e.db()
        .execute("UPDATE tasks SET state='running' WHERE id=?1", [running])
        .unwrap();
    let bad = e.forge(
        "ok.sh",
        &["withdraw", &running.to_string(), "--reason", "no"],
    );
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("running"), "{err}");
    assert_eq!(e.task(running).0, "running", "left untouched");

    // Only a blocked or queued task is withdrawn; task 1 is already withdrawn.
    let bad2 = e.forge("ok.sh", &["withdraw", "1", "--reason", "again"]);
    assert!(!bad2.status.success());
}

#[test]
fn stats_json_is_the_text_form_as_one_object() {
    let e = Env::new();
    assert!(e.run("tooly.sh", &["--retries", "0"]).status.success());

    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["stats"]).stdout).to_string();
    assert!(text.contains("direct"), "{text}");

    let doc: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json"]).stdout).unwrap();
    let wf = doc["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["WF"] == "direct")
        .expect("direct workflow entry");
    assert_eq!(wf["TASKS"], 1);
    assert_eq!(wf["OK"], 1);
    assert_eq!(wf["FAIL"], 0);
    assert_eq!(wf["ATT"], 1);
    assert!(wf["COST"].as_f64().unwrap() > 0.0, "{wf}");
    assert_eq!(wf["$/OK"], wf["COST"]);
    // The task ran with --no-land, so it succeeded without landing.
    assert_eq!(wf["LANDED"], 0);
    assert!(wf["$/LANDED"].is_null(), "{wf}");
    // No tools key without --tools.
    assert!(doc.get("tools").is_none(), "{doc}");

    let step = doc["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["WF"] == "direct" && s["STEP"] == "code")
        .expect("direct/code step entry");
    assert_eq!(step["OK"], 1);
    assert_eq!(step["ATT"], 1);

    let both: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json", "--tools"]).stdout).unwrap();
    assert!(
        both["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["WF"] == "direct"),
        "{both}"
    );
    assert!(
        both["tools"]["code"]["by_tool"]["Bash"]["calls"]
            .as_u64()
            .unwrap()
            >= 1,
        "{both}"
    );

    // Land a second task (default: land unless --no-land) and confirm it's
    // counted separately from tasks that merely succeeded.
    assert!(
        e.forge(
            "ok.sh",
            &[
                "run",
                e.repo.to_str().unwrap(),
                "write 43 to answer.txt",
                "--retries",
                "0",
            ],
        )
        .status
        .success()
    );
    let landed_text = String::from_utf8_lossy(&e.forge("ok.sh", &["stats"]).stdout).to_string();
    assert!(landed_text.contains("LANDED"), "{landed_text}");
    assert!(landed_text.contains("$/LANDED"), "{landed_text}");

    let landed_doc: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json"]).stdout).unwrap();
    let landed_wf = landed_doc["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["WF"] == "direct")
        .expect("direct workflow entry");
    assert_eq!(landed_wf["LANDED"], 1);
    assert_eq!(landed_wf["$/LANDED"], landed_wf["COST"]);
}

#[test]
fn trace_requests_and_stats_expose_the_whole_run() {
    let e = Env::new();
    tdd_repo(&e);
    assert!(!run_tdd(&e, "ok.sh", "greentests.sh", "x").status.success());
    assert!(
        !e.run("workflowreq.sh", &["--retries", "0"])
            .status
            .success()
    );
    assert!(
        run_tdd(&e, "ok.sh", "testwriter.sh", "make answer.txt contain 42")
            .status
            .success()
    );

    let o = e.forge("ok.sh", &["trace", "1"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("workflow   tdd"), "{out}");
    assert!(
        out.contains("| name = \"tdd\""),
        "the exact workflow text is recorded:\n{out}"
    );
    assert!(
        out.contains("=== attempt 1 [tests seq 1] checks_failed"),
        "{out}"
    );
    assert!(
        out.contains("inputs     model=sonnet max_turns=40"),
        "per-step params are recorded:\n{out}"
    );
    assert!(out.contains("verdict    ✗ L1 red-on-base"), "{out}");
    assert!(
        out.contains("what       the tests step wrote tests that already pass"),
        "{out}"
    );
    assert!(
        out.contains("action     Either the task is already done"),
        "{out}"
    );

    let doc: serde_json::Value = e.trace_json("3");
    assert_eq!(doc["task"]["state"], "succeeded");
    assert_eq!(doc["attempts"][0]["inputs"]["step"], "tests");
    assert!(
        doc["attempts"][0]["outputs"]["verify_ref"]
            .as_str()
            .unwrap()
            .starts_with("verify/3@")
    );
    assert_eq!(doc["attempts"][1]["inputs"]["step"], "code");
    assert_eq!(doc["attempts"][1]["inputs"]["overlay_refs"][0], "verify/3");
    assert!(
        doc["attempts"][1]["inputs"]["interface"]
            .as_str()
            .unwrap()
            .contains("answer.txt")
    );
    assert_eq!(
        doc["attempts"][1]["outputs"]["changed_files"][0],
        "answer.txt"
    );
    assert_eq!(
        doc["attempts"][1]["outputs"]["end_sha"]
            .as_str()
            .unwrap()
            .len(),
        40
    );
    assert_eq!(doc["diagnosis"].as_array().unwrap().len(), 0);

    let o = e.forge("ok.sh", &["requests"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("workflow"), "{out}");
    assert!(out.contains("This needs a browser e2e step"), "{out}");

    let o = e.forge("ok.sh", &["stats"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("direct"), "{out}");
    assert!(out.contains("tdd"), "{out}");
    assert!(out.contains("tests"), "{out}");

    let o = e.forge("ok.sh", &["show", "2"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("action     A workflow request"), "{out}");

    let o = e.forge("ok.sh", &["workflows"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("measured   unknown ("), "{out}");
}

#[test]
fn log_pages_back_with_before_and_searches_text_or_id() {
    let e = Env::new();
    for _ in 0..3 {
        e.add(&[]);
    }
    let ids = |args: &[&str]| -> Vec<i64> {
        let mut a = vec!["log", "--json"];
        a.extend_from_slice(args);
        let v: serde_json::Value = serde_json::from_slice(&e.forge("ok.sh", &a).stdout).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(ids(&[]), vec![3, 2, 1]);
    assert_eq!(ids(&["--before", "3"]), vec![2, 1]);
    assert_eq!(ids(&["--before", "3", "--limit", "1"]), vec![2]);
    assert_eq!(ids(&["--grep", "answer.txt"]), vec![3, 2, 1]);
    assert_eq!(ids(&["--grep", "3"]), vec![3], "an exact id matches");
    assert_eq!(ids(&["--grep", "nothing like this"]), Vec::<i64>::new());
    assert_eq!(ids(&["--workflow", "direct"]), vec![3, 2, 1]);
    assert_eq!(ids(&["--workflow", "tdd"]), Vec::<i64>::new());
}

#[test]
fn requests_can_be_scoped_to_one_repo() {
    let e = Env::new();
    std::fs::write(e.repo.join("forge.toml"), "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n[verify]\nnamespace = [\"tests/acceptance/\"]\n").unwrap();
    git(&e.repo, &["commit", "-qam", "namespace"]);
    assert!(!e.run("suiteright.sh", &["--retries", "1"]).status.success());
    let (state, _, _) = e.task(1);
    assert_eq!(state, "blocked");

    let o = e.forge("ok.sh", &["requests", "--repo", e.repo.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("suite"));

    let o = e.forge("ok.sh", &["requests", "--repo", e.origin.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("no blocked tasks"));
}

#[test]
fn events_are_a_json_log_and_a_snapshot_names_where_to_subscribe_from() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["events"]).stdout).to_string();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = events.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(types.first(), Some(&"task_queued"), "{types:?}");
    assert_eq!(events[0]["workflow"], "direct");
    assert!(events[0]["retry_of"].is_null());
    assert!(
        types.contains(&"attempt_started")
            && types.contains(&"check")
            && types.contains(&"attempt_done")
            && types.contains(&"task_done"),
        "{types:?}"
    );
    assert!(
        events
            .iter()
            .all(|v| v["task"] == 1 && v["ts"].as_i64().is_some() && v["text"].as_str().is_some())
    );
    let done = events.iter().find(|v| v["type"] == "task_done").unwrap();
    assert_eq!(done["state"], "succeeded");
    let snap: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["snapshot"]).stdout).unwrap();
    let offset = snap["events_offset"].as_u64().unwrap();
    assert_eq!(
        offset,
        std::fs::metadata(e.home.join("events.jsonl"))
            .unwrap()
            .len()
    );
    assert_eq!(snap["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(snap["worker"]["running"], false);
    let after = e.forge("ok.sh", &["events", "--since", &offset.to_string()]);
    assert!(after.stdout.is_empty(), "nothing after the snapshot");
    // A second task's events follow the offset, and --task filters.
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let later = String::from_utf8_lossy(
        &e.forge(
            "ok.sh",
            &["events", "--since", &offset.to_string(), "--task", "2"],
        )
        .stdout,
    )
    .to_string();
    assert!(!later.is_empty());
    assert!(
        later
            .lines()
            .all(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["task"] == 2)
    );
}

#[test]
fn events_roll_twice_and_dot_2_holds_the_oldest_generation() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());

    let path = e.home.join("events.jsonl");
    let path_1 = e.home.join("events.jsonl.1");
    let path_2 = e.home.join("events.jsonl.2");
    assert!(path.exists());
    assert!(!path_1.exists());

    let oversized = |marker: &str| {
        format!(
            "{{\"marker\":\"{marker}\"}}\n{}\n",
            "x".repeat(50 * 1024 * 1024 + 1024)
        )
    };

    // Past the roll size, events.jsonl becomes .1.
    std::fs::write(&path, oversized("gen_a")).unwrap();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    assert!(
        path_1.exists(),
        "events.jsonl.1 should exist after the first roll"
    );
    assert!(!path_2.exists(), "no .2 yet: only one roll has happened");
    assert!(std::fs::read_to_string(&path_1).unwrap().contains("gen_a"));

    // Past the roll size again, the old .1 becomes .2 and the new events.jsonl becomes .1.
    std::fs::write(&path, oversized("gen_b")).unwrap();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    assert!(
        path_2.exists(),
        "events.jsonl.2 should exist after two rolls"
    );
    assert!(
        std::fs::read_to_string(&path_2).unwrap().contains("gen_a"),
        "events.jsonl.2 should hold the oldest generation"
    );
    assert!(std::fs::read_to_string(&path_1).unwrap().contains("gen_b"));
}

#[test]
fn the_journal_tells_the_next_agent_what_earlier_ones_said_and_what_the_checks_found() {
    let e = Env::new();
    // Attempt 1 is wrong; attempt 2 is told what 1 said and what failed.
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    let o = e.forge("ok.sh", &["journal", "1"]);
    let j = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(j.contains("So far in this piece of work"), "{j}");
    assert!(j.contains("1 code    rejected by the checks"), "{j}");
    assert!(j.contains("found:   L1 answer:"), "{j}");
    let oj = e.forge("ok.sh", &["journal", "1", "--json"]);
    let entries: serde_json::Value = serde_json::from_slice(&oj.stdout).unwrap();
    let arr = entries.as_array().unwrap();
    assert_eq!(arr.len(), 1, "{entries}");
    assert_eq!(arr[0]["state"], "checks_failed", "{entries}");
    // A retry inherits the whole lineage's journal, and its own attempt records it verbatim.
    assert!(e.forge("ok.sh", &["retry", "1"]).status.success());
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");
    let prompt = e.log_text(2, 1);
    assert!(
        prompt.contains("So far in this piece of work"),
        "the retry's coder saw the journal"
    );
    assert!(prompt.contains("task 1 (direct), failed"), "{prompt}");
    let doc: serde_json::Value = e.trace_json("2");
    assert!(
        doc["attempts"][0]["inputs"]["journal"]
            .as_str()
            .unwrap()
            .contains("found:   L1 answer")
    );
    assert_eq!(
        doc["attempts"][0]["outputs"]["first_edit_call"], 0,
        "ok.sh edits on its first tool call"
    );
    assert!(
        doc["task"]["journal"]
            .as_str()
            .unwrap()
            .contains("task 2 (direct, this task)")
    );
    // Nothing ran before the first attempt of a fresh task.
    let o = e.forge("ok.sh", &["journal", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("1 code"));
    let stats = String::from_utf8_lossy(&e.forge("ok.sh", &["stats"]).stdout).to_string();
    assert!(stats.contains("EDIT@"), "{stats}");
    // Task 1's unpublished commit is superseded by task 2's success: gc lets it go.
    let gc = String::from_utf8_lossy(&e.forge("ok.sh", &["gc"]).stdout).to_string();
    assert!(
        gc.lines()
            .any(|l| l.starts_with("task 1 ") && l.contains("removed")),
        "{gc}"
    );
    assert!(!e.home.join("worktrees/1").exists());
}

#[test]
fn what_an_attempt_ran_is_recorded_with_durations_and_shown() {
    let e = Env::new();
    assert!(e.run("tooly.sh", &["--retries", "0"]).status.success());
    let doc: serde_json::Value = e.trace_json("1");
    let tools = &doc["attempts"][0]["outputs"]["tools"];
    assert_eq!(tools["by_tool"]["Read"]["calls"], 1);
    assert_eq!(tools["by_tool"]["Bash"]["calls"], 1);
    assert!(
        tools["shell"]["npx vitest"]["ms"].as_u64().unwrap() >= 250,
        "{tools}"
    );
    assert_eq!(tools["reads"]["hello.sh"], 1, "{tools}");
    assert_eq!(doc["attempts"][0]["outputs"]["first_edit_call"], 2);
    // Every frame carries Forge's clock.
    let log = e.log_text(1, 1);
    assert!(
        log.lines().filter(|l| l.contains("\"forge_ms\"")).count() >= 7,
        "{log}"
    );
    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    assert!(
        show.contains("ran     ") && show.contains("shell: npx vitest 1 ("),
        "{show}"
    );
    let st = String::from_utf8_lossy(&e.forge("ok.sh", &["stats", "--tools"]).stdout).to_string();
    assert!(
        st.contains("npx vitest") && st.contains("most read: hello.sh (1)"),
        "{st}"
    );
    // The journal has a control arm.
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    assert!(e.forge("ok.sh", &["retry", "2"]).status.success());
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
            "--no-journal",
        ],
    );
    assert!(o.status.success());
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    assert!(
        e.log_text(3, 1).contains("So far in this piece of work"),
        "the retry got the journal"
    );
    let four: serde_json::Value = e.trace_json("4");
    assert_eq!(four["task"]["journal_enabled"], false);
    assert_eq!(four["task"]["journal_arm"], "explicit");

    // --step <name> filters the per_step map: a second, differently-named
    // step ("fix", from the cheap workflow) must not leak into the section
    // for "code" once filtered.
    assert!(
        e.run("tooly.sh", &["--workflow", "cheap", "--retries", "0"])
            .status
            .success()
    );
    let both = String::from_utf8_lossy(&e.forge("ok.sh", &["stats", "--tools"]).stdout).to_string();
    assert!(
        both.contains("code  (") && both.contains("fix  ("),
        "{both}"
    );
    let code_only = String::from_utf8_lossy(
        &e.forge("ok.sh", &["stats", "--tools", "--step", "code"])
            .stdout,
    )
    .to_string();
    assert!(code_only.contains("code  ("), "{code_only}");
    assert!(!code_only.contains("fix  ("), "{code_only}");
    // The kernel's own verify row carries the attempt's real duration.
    let doc: serde_json::Value = e.trace_json("1");
    let verify_ms = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "verify")
        .map(|o| o["ms"].as_i64().unwrap_or(0))
        .unwrap_or(0);
    assert!(verify_ms >= 250, "verify row ms {verify_ms}");
}

#[test]
fn forge_client_parses_trace_snapshot_log_and_requests() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());

    let trace: TraceDoc =
        serde_json::from_slice(&e.forge("ok.sh", &["trace", "1", "--json"]).stdout).unwrap();
    assert_eq!(trace.task["id"], 1);
    assert_eq!(trace.task["state"], "succeeded");

    let snap: Snapshot = serde_json::from_slice(&e.forge("ok.sh", &["snapshot"]).stdout).unwrap();
    let row = snap
        .tasks
        .iter()
        .find(|t| t.id == 1)
        .expect("task 1 in snapshot");
    assert_eq!(row.state, "succeeded");

    let log: Vec<TaskRow> =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    let row = log.iter().find(|t| t.id == 1).expect("task 1 in log");
    assert_eq!(row.state, "succeeded");

    let requests: Vec<RequestRow> =
        serde_json::from_slice(&e.forge("ok.sh", &["requests", "--json"]).stdout).unwrap();
    assert!(requests.is_empty(), "{requests:?}");
}

#[test]
fn log_repo_filters_to_one_repository_canonicalized_like_add() {
    let e = Env::new();
    let repo2 = e._dir.path().join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    git(&repo2, &["init", "-q", "-b", "main"]);
    git(&repo2, &["config", "user.name", "Test"]);
    git(&repo2, &["config", "user.email", "test@example.com"]);
    std::fs::write(
        repo2.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\n",
    )
    .unwrap();
    git(&repo2, &["add", "-A"]);
    git(&repo2, &["commit", "-qm", "init"]);

    let a1 = e.add(&[]);
    let a2 = e.add(&[]);
    let o = e.forge(
        "ok.sh",
        &["add", repo2.to_str().unwrap(), "write 42 to answer.txt"],
    );
    assert!(o.status.success());
    let b1: i64 = String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();

    let list = |args: &[&str]| -> Vec<i64> {
        let out: serde_json::Value =
            serde_json::from_slice(&e.forge("ok.sh", args).stdout).unwrap();
        out.as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().unwrap())
            .collect()
    };

    assert_eq!(
        list(&["log", "--json", "--repo", e.repo.to_str().unwrap()]),
        vec![a2, a1],
        "repo1 sees only its own tasks, newest first"
    );
    assert_eq!(
        list(&["log", "--json", "--repo", repo2.to_str().unwrap()]),
        vec![b1],
        "repo2 sees only its own task"
    );
    assert_eq!(
        list(&["log", "--json"]).len(),
        3,
        "unfiltered log still sees every repo"
    );

    // Combinable with --state.
    assert_eq!(
        list(&[
            "log",
            "--json",
            "--repo",
            e.repo.to_str().unwrap(),
            "--state",
            "queued",
        ]),
        vec![a2, a1]
    );
    assert!(
        list(&[
            "log",
            "--json",
            "--repo",
            e.repo.to_str().unwrap(),
            "--state",
            "succeeded",
        ])
        .is_empty()
    );

    // Canonicalized the same way `add` canonicalizes: an uncanonical path still matches.
    let uncanon = repo2.join(".").join("..").join("repo2");
    assert_eq!(
        list(&["log", "--json", "--repo", uncanon.to_str().unwrap()]),
        vec![b1]
    );
}

#[test]
fn project_list_shows_the_test_repository_after_one_task() {
    let e = Env::new();
    e.add(&[]);

    let o = e.forge("ok.sh", &["project", "list", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");

    let repo = e.repo.canonicalize().unwrap().display().to_string();
    assert_eq!(rows[0]["name"], "repo", "{rows:?}");
    let repos = rows[0]["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["repo"], repo);
    assert!(repos[0]["scope"].is_null());
    assert_eq!(rows[0]["queued"], 1);
    assert_eq!(rows[0]["cost_usd"], 0.0);

    // `forge project show` agrees, and the task itself carries the project.
    let o = e.forge("ok.sh", &["project", "show", "repo", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["name"], "repo");
    assert_eq!(row["queued"], 1);

    let project: Option<String> = e
        .db()
        .query_row("SELECT project FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(project.as_deref(), Some("repo"));

    // Unknown project names fail rather than printing nothing.
    let bad = e.forge("ok.sh", &["project", "show", "no-such-project"]);
    assert!(!bad.status.success());
}

#[test]
fn a_projects_scope_fails_a_task_that_writes_outside_it() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "scoped",
                "--purpose",
                "p",
                "--repo",
                &format!("{repo}:src/"),
            ],
        )
        .status
        .success()
    );

    // The only task's repository (this one) lists exactly one project, so
    // it is the default: no --project needed. `ok.sh` writes answer.txt,
    // outside the project's "src/" scope.
    assert!(!e.run("ok.sh", &["--retries", "0"]).status.success());
    let a = e.attempts(1);
    assert_eq!(a[0].2, "L0 failed: paths-in-scope");
    assert_eq!(check(&a[0].4, "L0", "paths-in-scope"), Some(false));
}

#[test]
fn a_projects_per_task_budget_applies_unless_the_task_overrides_it() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "budgeted",
                "--purpose",
                "p",
                "--repo",
                repo
            ],
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &["project", "set", "budgeted", "--per-task-usd", "0.005"],
        )
        .status
        .success()
    );

    // No --budget on the task: the project's default applies and stops
    // it after the first $0.01 attempt, the same way an explicit
    // `--budget 0.005` would (see `task_budget_stops_retries`).
    assert!(!e.run("flaky.sh", &["--retries", "3"]).status.success());
    assert_eq!(e.attempts(1).len(), 1);
    assert!(
        e.task(1).1.starts_with("task budget reached"),
        "{}",
        e.task(1).1
    );

    // The task's own --budget wins over the project's default, so it
    // affords the second attempt `flaky.sh` needs to get it right.
    assert!(
        e.run("flaky.sh", &["--retries", "3", "--budget", "1.0"])
            .status
            .success()
    );
    assert_eq!(e.task(2).0, "succeeded");
}

#[test]
fn forge_add_refuses_a_repository_listed_by_several_projects() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "a", "--purpose", "p", "--repo", repo]
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "b", "--purpose", "p", "--repo", repo]
        )
        .status
        .success()
    );

    let o = e.forge("ok.sh", &["add", repo, "write 42 to answer.txt"]);
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("listed by several projects"), "{stderr}");
    assert!(stderr.contains('a') && stderr.contains('b'), "{stderr}");

    // Naming which one with --project succeeds.
    let o = e.forge(
        "ok.sh",
        &["add", repo, "write 42 to answer.txt", "--project", "a"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn stats_project_filters_the_workflow_rows_and_lists_every_project_when_unfiltered() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();

    // No project named yet: the repository's own default project, named
    // after its directory ("repo"), gets this one.
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());

    // A second project, not registered to any repository, still takes a
    // task named explicitly with --project (see
    // `forge_add_refuses_a_repository_listed_by_several_projects`: naming
    // a project does not require it to list the repository).
    assert!(
        e.forge("ok.sh", &["project", "new", "other", "--purpose", "p"])
            .status
            .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &[
                "run",
                repo,
                "write 42 to answer.txt",
                "--no-land",
                "--retries",
                "0",
                "--project",
                "other",
            ],
        )
        .status
        .success()
    );

    // Filtered to the default project: only its one task is counted.
    let filtered: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["stats", "--project", "repo", "--json"])
            .stdout,
    )
    .unwrap();
    let wf = filtered["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["workflow"] == "direct")
        .expect("direct workflow entry");
    assert_eq!(wf["pieces"], 1, "{filtered}");
    // Scoped: no per-project rollup.
    assert!(filtered.get("projects").is_none(), "{filtered}");

    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["stats", "--project", "repo"]).stdout)
        .to_string();
    assert!(text.contains("direct"), "{text}");

    // Unfiltered: both tasks are counted together, and a per-project
    // section lists both projects.
    let all: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json"]).stdout).unwrap();
    let wf_all = all["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["workflow"] == "direct")
        .expect("direct workflow entry");
    assert_eq!(wf_all["pieces"], 2, "{all}");

    let mut names: Vec<&str> = all["projects"]
        .as_array()
        .unwrap_or_else(|| panic!("{all}"))
        .iter()
        .map(|p| p["project"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["other", "repo"]);
}
