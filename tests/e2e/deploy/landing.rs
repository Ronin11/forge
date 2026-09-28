use super::*;

#[test]
fn a_task_landing_on_a_repository_deploys_its_on_landing_targets_tied_to_the_task() {
    landing_deploys(false);
}

#[test]
fn forge_land_unverified_succeeds_settles_initiative_and_deploys() {
    landing_deploys(true);
}

fn landing_deploys(by_hand: bool) {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    // Exactly the arrangement `a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question`
    // uses: a fake ssh/rsync reaching a directory on this machine, and the
    // check reading what got deployed there.
    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "command=true",
            "--check",
            "grep -qx 42 answer.txt",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    if by_hand {
        prepare_unverified_landing(&e);
    }
    let args = if by_hand {
        vec!["land", "1"]
    } else {
        vec!["run", repo_s, "write 42", "--retries", "0"]
    };
    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    if by_hand {
        assert_landed_initiative(&e);
    }

    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(calls.contains("rsync"), "{calls}");
    assert!(calls.contains("ssh remotebox"), "{calls}");

    // The deploy row is tied to the task and its check passed.
    let (task_id, check_ok): (Option<i64>, Option<i64>) = e
        .db()
        .query_row(
            "SELECT task_id, check_ok FROM deploys WHERE project='demo' AND target='prod'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(task_id, Some(1));
    assert_eq!(check_ok, Some(1));

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["check_ok"], true);

    assert_landing_deploy_events(&e);
}

fn prepare_unverified_landing(e: &Env) {
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    assert!(e.task(1).2, "the branch was pushed");
    assert!(
        e.forge(
            "ok.sh",
            &["initiative", "new", "demo", "--outcome", "ship answer"]
        )
        .status
        .success()
    );
    // Model a pushed candidate whose review could not finish.
    e.db().execute(
        "UPDATE tasks SET state='unverified', reason='review login expired', finished_at=NULL, initiative=1 WHERE id=1",
        [],
    ).unwrap();
    let before: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["initiative", "show", "1", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(before["unverified"], 1);
    assert_eq!(before["succeeded"], 0);
}

fn assert_landed_initiative(e: &Env) {
    let after: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["initiative", "show", "1", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(after["unverified"], 0);
    assert_eq!(after["succeeded"], 1);
    assert!(after["settled_at"].is_number());
    let (sha, finished): (String, Option<i64>) = e
        .db()
        .query_row(
            "SELECT landed_sha, finished_at FROM tasks WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(e.task(1).1, format!("landed main @ {}", &sha[..8]));
    assert!(finished.is_some());
}

fn assert_landing_deploy_events(e: &Env) {
    // DeployStarted and DeployFinished carry the task id.
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    let parsed: Vec<serde_json::Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let started = parsed
        .iter()
        .find(|v| v["type"] == "deploy_started")
        .unwrap_or_else(|| panic!("no deploy_started event in:\n{events}"));
    assert_eq!(started["task"], 1, "{started}");
    let finished = parsed
        .iter()
        .find(|v| v["type"] == "deploy_finished")
        .unwrap_or_else(|| panic!("no deploy_finished event in:\n{events}"));
    assert_eq!(finished["task"], 1, "{finished}");
    assert_eq!(finished["ok"], true, "{finished}");
}
