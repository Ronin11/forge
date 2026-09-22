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
fn forge_task_set_changes_a_queued_tasks_limits_and_refuses_a_running_one() {
    let e = Env::new();
    let id = e.add(&[]);
    let limits = |e: &Env| -> (Option<f64>, i64, i64, i64) {
        e.db()
            .query_row(
                "SELECT budget_usd, max_turns, max_attempts, timeout_secs FROM tasks WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    };
    let (budget, ..) = limits(&e);
    assert_eq!(budget, None);

    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &id.to_string(),
            "--budget",
            "20",
            "--max-turns",
            "50",
            "--timeout-secs",
            "900",
            "--retries",
            "3",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("max 50 turns, max 4 attempts, 900s timeout"),
        "{out}"
    );
    assert!(out.contains("task cap $20.00"), "{out}");

    let (budget, max_turns, max_attempts, timeout_secs) = limits(&e);
    assert_eq!(budget, Some(20.0));
    assert_eq!(max_turns, 50);
    assert_eq!(max_attempts, 4);
    assert_eq!(timeout_secs, 900);
    assert_eq!(e.task(id).0, "queued", "the change does not touch state");

    // The change is on the record as a decision, like an operator's answer.
    let ds: serde_json::Value = e.decisions_json();
    let d = &ds.as_array().unwrap()[0];
    assert_eq!(d["task_id"], id);
    assert_eq!(d["answered_by"], "operator");
    assert!(
        d["answer"]
            .as_str()
            .unwrap()
            .contains("budget unset → $20.00"),
        "{d}"
    );
    assert_eq!(d["retry_id"], id);

    // A nonsense limits set is refused up front, before anything changes.
    let bad = e.forge("ok.sh", &["task", "set", &id.to_string()]);
    assert!(!bad.status.success());

    // Setting a running task's limits is refused, and nothing changes.
    e.db()
        .execute("UPDATE tasks SET state='running' WHERE id=?1", [id])
        .unwrap();
    let bad = e.forge("ok.sh", &["task", "set", &id.to_string(), "--budget", "99"]);
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("running"), "{err}");
    let (budget, ..) = limits(&e);
    assert_eq!(budget, Some(20.0), "left untouched");
}

/// `forge task set` edits the spec, not only the limits: text, workflow,
/// dependencies and checks, each held to what `forge add` holds it to,
/// each recorded old → new on the decision row.
#[test]
fn forge_task_set_replaces_text_workflow_after_and_checks_and_records_old_and_new() {
    let e = Env::new();
    let dep = e.add(&[]);
    let id = e.add(&["--check", "test -f answer.txt"]);
    let sid = id.to_string();
    let spec = |e: &Env| -> (String, String, String, String) {
        e.db()
            .query_row(
                "SELECT task, workflow, after_json, checks_json FROM tasks WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    };
    assert_eq!(
        spec(&e),
        (
            "write 42 to answer.txt".into(),
            "direct".into(),
            "[]".into(),
            "[\"test -f answer.txt\"]".into()
        )
    );

    let text_file = e.repo.join("new-text.txt");
    std::fs::write(&text_file, "write 43 to answer.txt and src/lib.rs\n").unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &sid,
            "--text-file",
            text_file.to_str().unwrap(),
            "--workflow",
            "reviewed",
            "--after",
            &dep.to_string(),
            "--check",
            "test -f answer.txt",
            "--check",
            "grep -qx 43 answer.txt",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        spec(&e),
        (
            "write 43 to answer.txt and src/lib.rs\n".into(),
            "reviewed".into(),
            format!("[{dep}]"),
            "[\"test -f answer.txt\",\"grep -qx 43 answer.txt\"]".into()
        )
    );
    let (len, paths, hash): (i64, i64, String) = e
        .db()
        .query_row(
            "SELECT shape_text_len, shape_path_tokens, workflow_hash FROM tasks WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((len, paths), (38, 2), "the shape follows the text");
    assert!(!hash.is_empty(), "the workflow hash follows the workflow");
    assert_eq!(e.task(id).0, "queued", "the change does not touch state");

    let d = e.decisions_json();
    let d = d
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["task_id"] == id)
        .unwrap()
        .clone();
    assert_eq!(d["question"], format!("task {id}'s spec"));
    let answer = d["answer"].as_str().unwrap();
    for part in [
        "text 22 chars ",
        " → 38 chars ",
        "workflow direct → reviewed",
        &format!("after [] → [{dep}]"),
        "checks [\"test -f answer.txt\"] → [\"test -f answer.txt\",\"grep -qx 43 answer.txt\"]",
    ] {
        assert!(answer.contains(part), "{part:?} missing from {answer:?}");
    }

    // `--text` and `--no-after` / `--no-checks`: the repository declares
    // checks, so a task with none is still verified.
    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &sid,
            "--text",
            "write 42",
            "--no-after",
            "--no-checks",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        spec(&e),
        (
            "write 42".into(),
            "reviewed".into(),
            "[]".into(),
            "[]".into()
        )
    );

    // Refusals, each leaving the spec as it was.
    for (bad, why) in [
        (vec!["--workflow", "no-such-workflow"], "unknown workflow"),
        (vec!["--after", "999"], "no such task"),
        (vec!["--after", &sid], "wait on itself"),
        (vec!["--text", "  "], "empty"),
        (vec!["--check", ""], "empty"),
    ] {
        let mut args = vec!["task", "set", &sid];
        args.extend(bad.iter());
        let o = e.forge("ok.sh", &args);
        assert!(!o.status.success(), "{bad:?} must be refused");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(why), "{bad:?}: {err}");
    }
    assert_eq!(
        spec(&e),
        (
            "write 42".into(),
            "reviewed".into(),
            "[]".into(),
            "[]".into()
        )
    );

    // A cycle: dep waits on id, so id may not wait on dep.
    assert!(
        e.forge("ok.sh", &["task", "set", &dep.to_string(), "--after", &sid])
            .status
            .success()
    );
    let o = e.forge("ok.sh", &["task", "set", &sid, "--after", &dep.to_string()]);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("wait on each other"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // `--no-checks` on a repository that declares no checks leaves
    // nothing to verify the work, so it is refused.
    std::fs::write(e.repo.join("forge.toml"), "").unwrap();
    let o = e.forge("ok.sh", &["task", "set", &sid, "--no-checks"]);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("nothing would verify"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// `forge add --json` names the task it queued; `forge show --json` is
/// the task's record with every field of its spec, so a client can feed
/// one task's spec back into `forge add` and get the same spec.
#[test]
fn add_json_names_the_task_and_show_json_round_trips_the_spec_through_add() {
    let e = Env::new();
    let dep = e.add(&[]);
    let repo = e.repo.to_str().unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            repo,
            "write 42 to answer.txt",
            "--workflow",
            "reviewed",
            "--check",
            "test -f answer.txt",
            "--check",
            "grep -qx 42 answer.txt",
            "--after",
            &dep.to_string(),
            "--budget",
            "7.5",
            "--max-turns",
            "40",
            "--retries",
            "2",
            "--timeout-secs",
            "600",
            "--show-checks",
            "--allow-protected",
            "--no-land",
            "--json",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let added: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(added["id"], dep + 1);
    assert_eq!(added["queued"], 2, "{added}");
    let id = added["id"].as_i64().unwrap();

    let show = |id: i64| -> serde_json::Value {
        let o = e.forge("ok.sh", &["show", &id.to_string(), "--json"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    };
    let t = show(id);
    assert_eq!(t, e.trace_json(id)["task"], "show --json is TraceDoc.task");
    assert_eq!(t["text"], "write 42 to answer.txt");
    assert_eq!(t["workflow"], "reviewed");
    assert_eq!(t["after"], serde_json::json!([dep]));
    assert_eq!(t["budget_usd"], 7.5);
    assert_eq!(t["land"], false);

    // Feed the spec back into `forge add`, then compare the two records.
    let text = t["text"].as_str().unwrap().to_string();
    let workflow = t["workflow"].as_str().unwrap().to_string();
    let budget = t["budget_usd"].as_f64().unwrap().to_string();
    let max_turns = t["max_turns"].as_i64().unwrap().to_string();
    let retries = (t["max_attempts"].as_i64().unwrap() - 1).to_string();
    let timeout = t["timeout_secs"].as_i64().unwrap().to_string();
    let mut args: Vec<String> = vec![
        "add".into(),
        repo.into(),
        text,
        "--workflow".into(),
        workflow,
        "--budget".into(),
        budget,
        "--max-turns".into(),
        max_turns,
        "--retries".into(),
        retries,
        "--timeout-secs".into(),
        timeout,
        "--json".into(),
    ];
    for c in t["checks"].as_array().unwrap() {
        args.push("--check".into());
        args.push(c.as_str().unwrap().into());
    }
    for a in t["after"].as_array().unwrap() {
        args.push("--after".into());
        args.push(a.to_string());
    }
    for (flag, on) in [
        ("--show-checks", t["show_checks"] == true),
        ("--allow-protected", t["allow_protected"] == true),
        ("--no-land", t["land"] == false),
    ] {
        if on {
            args.push(flag.into());
        }
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = e.forge("ok.sh", &argv);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let again: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let copy = show(again["id"].as_i64().unwrap());
    for field in [
        "text",
        "workflow",
        "checks",
        "after",
        "budget_usd",
        "max_turns",
        "max_attempts",
        "timeout_secs",
        "show_checks",
        "allow_protected",
        "land",
        "provider",
        "model",
        "project",
        "initiative",
        "trust",
        "repo",
    ] {
        assert_eq!(copy[field], t[field], "{field} did not round-trip");
    }
    assert_ne!(copy["id"], t["id"]);

    let o = e.forge("ok.sh", &["show", "999", "--json"]);
    assert!(!o.status.success());
}

/// `forge log --touches PATH` lists tasks by the files their attempts
/// recorded changing, on a `/` boundary; `--touches-text` adds tasks
/// whose text only mentions the path, marked so.
#[test]
fn forge_log_touches_finds_tasks_by_recorded_changes_on_a_slash_boundary() {
    let e = Env::new();
    let mentions = e.add(&[]);
    e.db()
        .execute(
            "UPDATE tasks SET task = 'later, tidy a/b.rs' WHERE id = ?1",
            [mentions],
        )
        .unwrap();
    let seed = |path: &str| -> i64 {
        let id = e.add(&[]);
        e.db()
            .execute("UPDATE tasks SET state = 'succeeded' WHERE id = ?1", [id])
            .unwrap();
        e.db()
            .execute(
                "INSERT INTO attempts (task_id, attempt_no, step, state, started_at, finished_at, cost_usd, envelope_json)
                 VALUES (?1, 1, 'code', 'succeeded', 1, 2, 0.1,
                 json_object('schema_version', 1, 'summary', 's', 'needs_input', NULL,
                             'changes', json_array(json_object('path', ?2, 'kind', 'modified', 'summary', '')),
                             'checks_run', json_array(), 'claims', json_array()))",
                rusqlite::params![id, path],
            )
            .unwrap();
        id
    };
    let changed_b = seed("a/b.rs");
    let changed_bc = seed("a/bc.rs");

    let ids = |args: &[&str]| -> Vec<(i64, Option<String>)> {
        let mut a = vec!["log", "--json"];
        a.extend_from_slice(args);
        let o = e.forge("ok.sh", &a);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["id"].as_i64().unwrap(),
                    r["touch"].as_str().map(str::to_string),
                )
            })
            .collect()
    };
    let changes = Some("changes".to_string());
    assert_eq!(
        ids(&["--touches", "a/b.rs"]),
        vec![(changed_b, changes.clone())]
    );
    assert_eq!(
        ids(&["--touches", "a"]),
        vec![(changed_bc, changes.clone()), (changed_b, changes.clone())],
        "a directory matches everything under it"
    );
    assert_eq!(
        ids(&["--touches", "a/b"]),
        vec![],
        "a/b names neither a/b.rs nor a directory above it: the boundary is /"
    );
    assert_eq!(
        ids(&["--touches", "a/b.rs", "--touches", "a/bc.rs"]),
        vec![(changed_bc, changes.clone()), (changed_b, changes.clone())]
    );
    assert_eq!(
        ids(&["--touches", "a/b.rs", "--touches-text"]),
        vec![
            (changed_b, changes.clone()),
            (mentions, Some("text".to_string()))
        ],
        "the queued task that only mentions the path is marked by text"
    );
    assert_eq!(
        ids(&["--touches", "a/b.rs", "--touches-text", "--state", "queued"]),
        vec![(mentions, Some("text".to_string()))],
        "combines with the other filters"
    );
    let plain = ids(&["--limit", "1"]);
    assert_eq!(plain[0].1, None, "no touch field without the filter");

    let text = String::from_utf8_lossy(
        &e.forge("ok.sh", &["log", "--touches", "a/b.rs", "--touches-text"])
            .stdout,
    )
    .to_string();
    let by_text: Vec<&str> = text.lines().filter(|l| l.contains("(by text)")).collect();
    assert_eq!(by_text.len(), 1, "{text}");
    assert!(by_text[0].starts_with(&mentions.to_string()), "{text}");
}

/// `forge log --failed-on NAME` and `--reason TEXT` list tasks by what
/// failed on an attempt, straight from the verdict rows; unknown names
/// are refused with the known ones.
#[test]
fn forge_log_failed_on_and_reason_find_tasks_by_their_attempts_failures() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"grep -qx 42 answer.txt\"]\ntest = [\"bash\", \"-c\", \"grep -qx 42 answer.txt\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "a test check"]);
    // 1 fails `test` on its first attempt and passes on its second.
    assert!(
        e.run("flaky.sh", &["--retries", "3", "--budget", "1.0"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "succeeded");
    // 2 fails the L0 rule clean-tree; 3 dies with agent exit 1.
    assert!(!e.run("dirty.sh", &["--retries", "0"]).status.success());
    assert!(!e.run("crash.sh", &["--retries", "0"]).status.success());

    let rows = |args: &[&str]| -> Vec<serde_json::Value> {
        let mut a = vec!["log", "--json"];
        a.extend_from_slice(args);
        let o = e.forge("ok.sh", &a);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<serde_json::Value>(&o.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    };
    let ids = |rows: &[serde_json::Value]| -> Vec<i64> {
        rows.iter().map(|r| r["id"].as_i64().unwrap()).collect()
    };

    let r = rows(&["--failed-on", "test"]);
    assert_eq!(ids(&r), vec![1]);
    let f = r[0]["failures"].as_array().unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0]["attempt_no"], 1);
    assert_eq!(f[0]["step"], "code");
    assert_eq!(f[0]["name"], "test");
    assert!(f[0]["tail"].is_string());

    assert_eq!(ids(&rows(&["--failed-on", "clean-tree"])), vec![2]);
    assert_eq!(
        ids(&rows(&["--failed-on", "test", "--failed-on", "clean-tree"])),
        vec![2, 1]
    );
    assert_eq!(
        ids(&rows(&[
            "--failed-on",
            "clean-tree",
            "--state",
            "succeeded"
        ])),
        Vec::<i64>::new(),
        "combines with the other filters"
    );

    let r = rows(&["--reason", "agent exit 1"]);
    assert_eq!(ids(&r), vec![3]);
    let f = r[0]["failures"].as_array().unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0]["name"].is_null());
    assert!(
        f[0]["reason"].as_str().unwrap().contains("agent exit 1"),
        "{f:?}"
    );
    assert!(rows(&["--limit", "1"])[0].get("failures").is_none());

    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["log", "--failed-on", "test"]).stdout)
        .to_string();
    assert!(text.contains("(failed: test)"), "{text}");

    let o = e.forge("ok.sh", &["log", "--failed-on", "no-such-row"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    for known in ["clean-tree", "changes-match-git", "test", "answer"] {
        assert!(err.contains(known), "{known} missing from {err}");
    }
}

#[test]
fn forge_task_set_rejects_a_non_finite_budget_and_changes_nothing() {
    let e = Env::new();
    let id = e.add(&["--budget", "12"]);
    let budget = |e: &Env| -> Option<f64> {
        e.db()
            .query_row("SELECT budget_usd FROM tasks WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert_eq!(budget(&e), Some(12.0));

    for bad_budget in ["NaN", "inf"] {
        let o = e.forge(
            "ok.sh",
            &["task", "set", &id.to_string(), "--budget", bad_budget],
        );
        assert!(
            !o.status.success(),
            "--budget {bad_budget} should be refused"
        );
        assert_eq!(
            budget(&e),
            Some(12.0),
            "--budget {bad_budget} must not change the task's budget"
        );
        let ds = e.decisions_json();
        assert!(
            ds.as_array().unwrap().iter().all(|d| d["task_id"] != id),
            "--budget {bad_budget} must not record a decision: {ds}"
        );
    }
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
        out.contains("inputs     model=sonnet runner=claude-cli provider=anthropic max_turns=40"),
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
fn project_set_purpose_replaces_the_migrations_placeholder_everywhere_it_shows() {
    let e = Env::new();
    e.add(&[]);

    // The migration-created project starts with the placeholder purpose,
    // which `project show` and `project view` (the portal document) both
    // treat as absent rather than printing the repository path.
    let o = e.forge("ok.sh", &["project", "show", "repo", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["purpose"], "", "{row:?}");

    let o = e.forge("ok.sh", &["project", "view", "repo", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["purpose"], "", "{doc:?}");

    // `forge project set --purpose` gives it a real one, which then shows
    // up in both places.
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "set",
                "repo",
                "--purpose",
                "Keeps the orders flowing."
            ],
        )
        .status
        .success()
    );
    let o = e.forge("ok.sh", &["project", "show", "repo", "--json"]);
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["purpose"], "Keeps the orders flowing.");

    let o = e.forge("ok.sh", &["project", "view", "repo", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["purpose"], "Keeps the orders flowing.");
}

/// `--title` gives a task the day it was filed in the customer's own
/// words (see docs/PORTAL.md): what `PortalDoc`'s "Done" line uses
/// instead of deriving one from the task's own text.
#[test]
fn forge_add_title_is_stored_on_the_task() {
    let e = Env::new();
    let id = e.add(&["--title", "Show the annual discount on every quote"]);
    let title: Option<String> = e
        .db()
        .query_row("SELECT title FROM tasks WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(
        title.as_deref(),
        Some("Show the annual discount on every quote")
    );
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

#[test]
fn stats_json_carries_a_time_to_live_row_for_a_landed_workflow() {
    let e = Env::new();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "run",
                e.repo.to_str().unwrap(),
                "write 42 to answer.txt",
                "--retries",
                "0",
            ],
        )
        .status
        .success(),
        "a task that lands"
    );

    let doc: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json"]).stdout).unwrap();
    let rows = doc["time_to_live"].as_array().expect("time_to_live array");
    assert_eq!(rows.len(), 1, "{doc}");
    assert_eq!(rows[0]["workflow"], "direct", "{doc}");
    assert_eq!(rows[0]["n"], 1, "{doc}");
    assert!(rows[0]["median_secs"].as_f64().unwrap() >= 0.0, "{doc}");
    assert!(rows[0]["p90_secs"].as_f64().unwrap() >= 0.0, "{doc}");

    let text =
        String::from_utf8_lossy(&e.forge("ok.sh", &["stats", "--quality"]).stdout).to_string();
    assert!(text.contains("MEDIAN"), "{text}");
    assert!(text.contains("P90"), "{text}");
}

#[test]
fn forge_log_and_show_print_times_as_utc_whatever_the_tz() {
    let e = Env::new();
    assert!(e.run("ok.sh", &[]).status.success());
    // 2026-09-21T07:00:00Z
    e.db()
        .execute("UPDATE tasks SET created_at = 1789974000 WHERE id = 1", [])
        .unwrap();
    for tz in ["Asia/Tokyo", "America/Los_Angeles"] {
        let log = e
            .cmd("ok.sh")
            .env("TZ", tz)
            .args(["log"])
            .output()
            .expect("forge log");
        let out = String::from_utf8_lossy(&log.stdout);
        assert!(out.contains("2026-09-21T07:00:00Z"), "{tz}: {out}");
        let show = e
            .cmd("ok.sh")
            .env("TZ", tz)
            .args(["show", "1"])
            .output()
            .expect("forge show");
        let out = String::from_utf8_lossy(&show.stdout);
        assert!(out.contains("2026-09-21T07:00:00Z"), "{tz}: {out}");
    }
    // The JSON keeps its shape: the integer is the record, the legacy string is UTC too.
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    assert_eq!(rows[0]["created_at"], 1_789_974_000);
    assert_eq!(rows[0]["created"], "2026-09-21 07:00:00");
}
