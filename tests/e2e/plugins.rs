use crate::support::*;
use rusqlite::OptionalExtension;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

#[test]
fn plugin_list_shows_the_valid_plugin_and_doctor_reports_the_invalid_one_without_failing() {
    let e = Env::new();
    let plugins_dir = e.home.join("plugins");

    std::fs::create_dir_all(plugins_dir.join("notify")).unwrap();
    std::fs::write(
        plugins_dir.join("notify/plugin.toml"),
        "name = \"notify\"\ndescription = \"posts a notification\"\nrun = [\"./notify.sh\"]\ncapabilities = [\"events\"]\n",
    )
    .unwrap();

    // Invalid: the manifest's own `name` does not match the directory name.
    std::fs::create_dir_all(plugins_dir.join("broken")).unwrap();
    std::fs::write(
        plugins_dir.join("broken/plugin.toml"),
        "name = \"not-broken\"\nrun = [\"./x\"]\ncapabilities = [\"events\"]\n",
    )
    .unwrap();

    let o = e.forge("ok.sh", &["plugin", "list", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rows.len(), 1, "only the valid plugin loads: {rows:?}");
    assert_eq!(rows[0]["name"], "notify");
    assert_eq!(rows[0]["description"], "posts a notification");
    assert_eq!(rows[0]["restart"], "on-failure");
    assert_eq!(rows[0]["capabilities"], serde_json::json!(["events"]));
    assert_eq!(rows[0]["enabled"], false);
    assert!(
        rows[0]["dir"].as_str().unwrap().ends_with("plugins/notify"),
        "{rows:?}"
    );

    // The text listing surfaces the broken plugin as a problem, not silently.
    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["plugin", "list"]).stdout).to_string();
    assert!(
        text.contains("problem:") && text.contains("broken/plugin.toml"),
        "{text}"
    );

    // `forge doctor` reports it too, as a warning that does not fail the run.
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "a broken plugin must not fail doctor: {out}"
    );
    assert!(out.contains("WARN plugins"), "{out}");
    assert!(out.contains("does not match the directory name"), "{out}");

    // `forge plugin status` reports enabled/not, and the run state: never
    // supervised (no worker has run it yet) reads as stopped.
    let o = e.forge("ok.sh", &["plugin", "status", "notify", "--json"]);
    assert!(o.status.success());
    let status: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(status["name"], "notify");
    assert_eq!(status["enabled"], false);
    assert_eq!(status["state"], "stopped");
    assert_eq!(status["last_exit"], serde_json::Value::Null);

    let o = e.forge("ok.sh", &["plugin", "status", "nope"]);
    assert!(!o.status.success(), "an unknown plugin name is refused");
}

#[test]
fn a_shadowed_plugin_dirs_entry_and_a_missing_root_are_non_blocking_problems() {
    let e = Env::new();
    let extra = e.home.join("extra-plugins");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::create_dir_all(e.home.join("plugins/notify")).unwrap();
    std::fs::write(
        e.home.join("plugins/notify/plugin.toml"),
        "name = \"notify\"\ndescription = \"home copy\"\nrun = [\"./a\"]\ncapabilities = [\"events\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(extra.join("notify")).unwrap();
    std::fs::write(
        extra.join("notify/plugin.toml"),
        "name = \"notify\"\ndescription = \"shadowed copy\"\nrun = [\"./b\"]\ncapabilities = [\"events\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!(
            "plugin_dirs = [{:?}, {:?}]\n",
            extra.display().to_string(),
            e.home.join("no-such-dir").display().to_string()
        ),
    )
    .unwrap();

    let o = e.forge("ok.sh", &["plugin", "list", "--json"]);
    assert!(o.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["description"], "home copy",
        "the earlier root (FORGE2_HOME/plugins) wins"
    );

    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["plugin", "list"]).stdout).to_string();
    assert!(text.contains("shadowed"), "{text}");
    assert!(text.contains("does not exist"), "{text}");

    let o = e.forge("ok.sh", &["doctor"]);
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("WARN plugins"), "{out}");
}

/// `forge work` supervises an enabled plugin (starts it, restarts it per
/// its manifest's `restart` policy) and stops supervising a disabled one.
#[test]
fn the_worker_supervises_a_failing_plugin_and_stops_it_when_disabled() {
    let e = Env::new();
    let plugin_dir = e.home.join("plugins").join("flaky");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "name = \"flaky\"\nrun = [\"./run.sh\"]\ncapabilities = [\"events\"]\nrestart = \"always\"\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("run.sh"),
        "#!/bin/bash\necho line >> \"$FORGE_PLUGIN_STATE/count\"\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(
        plugin_dir.join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "flaky"])
            .status
            .success()
    );

    // A task that takes a couple of seconds gives the plugin's supervisor
    // enough wall-clock time to fail and restart at least once.
    e.add(&[]);
    let o = e
        .cmd("ok.sh")
        .env("FAKE_SLEEP", "1")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let count_file = e.home.join("plugins-state").join("flaky").join("count");
    let lines = std::fs::read_to_string(&count_file)
        .unwrap_or_default()
        .lines()
        .count();
    assert!(lines > 1, "expected a restart, only ran {lines} time(s)");

    let log_path = e.home.join("logs").join("plugins").join("flaky.log");
    assert!(log_path.exists(), "expected a plugin log at {log_path:?}");

    // Disabled, a fresh worker run must not start it at all.
    assert!(
        e.forge("ok.sh", &["plugin", "disable", "flaky"])
            .status
            .success()
    );
    let before = std::fs::read_to_string(&count_file)
        .unwrap()
        .lines()
        .count();
    e.add(&[]);
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    let after = std::fs::read_to_string(&count_file)
        .unwrap()
        .lines()
        .count();
    assert_eq!(before, after, "a disabled plugin must not run");

    let o = e.forge("ok.sh", &["plugin", "status", "flaky", "--json"]);
    let status: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(status["enabled"], false);
    assert_eq!(status["state"], "stopped");
}

/// `forge plugin install` refuses a name already installed, and
/// `forge plugin uninstall` clears the enabled flag and removes the
/// installed copy but leaves plugins-state (the plugin's own memory)
/// alone.
#[test]
fn install_refuses_a_duplicate_name_and_uninstall_leaves_plugins_state_alone() {
    let e = Env::new();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/notify");

    let o = e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.home.join("plugins/notify/plugin.toml").is_file());
    assert!(e.home.join("plugins/notify/notify.sh").is_file());

    let o = e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()]);
    assert!(
        !o.status.success(),
        "a second install of the same name must be refused"
    );

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "notify"])
            .status
            .success()
    );
    std::fs::create_dir_all(e.home.join("plugins-state/notify")).unwrap();
    std::fs::write(e.home.join("plugins-state/notify/cursor"), "123").unwrap();

    let o = e.forge("ok.sh", &["plugin", "uninstall", "notify"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        !e.home.join("plugins/notify").exists(),
        "the installed copy is removed"
    );
    assert!(
        e.home.join("plugins-state/notify/cursor").is_file(),
        "plugins-state is left alone"
    );

    let o = e.forge("ok.sh", &["plugin", "status", "notify", "--json"]);
    assert!(!o.status.success(), "notify is no longer in the catalog");
}

/// The reference plugin end to end: installed from the repository path,
/// enabled, and run by the worker. It follows events for the task it
/// watches and, on `task_done`, runs the command file dropped into its
/// installed directory with the task id as `$1`.
#[test]
fn the_reference_plugin_runs_its_command_on_task_done() {
    let e = Env::new();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/notify");

    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );

    let hits = e._dir.path().join("hits.txt");
    std::fs::write(
        e.home.join("plugins/notify/command"),
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> {}\n",
            hits.display()
        ),
    )
    .unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "notify"])
            .status
            .success()
    );

    let id = e.add(&[]);

    let o = e
        .cmd("ok.sh")
        .env("FAKE_SLEEP", "1")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    assert!(
        wait_until(
            || std::fs::read_to_string(&hits)
                .unwrap_or_default()
                .lines()
                .any(|l| l == id.to_string()),
            Duration::from_secs(5)
        ),
        "expected task {id} to appear in {}: {:?}",
        hits.display(),
        std::fs::read_to_string(&hits)
    );
}

/// The Signal plugin end to end, against a stub `signal-cli` early on
/// `PATH` (the plugin's `SIGNAL_CLI` variable defaults to the bare name,
/// so shadowing it on `PATH` is enough): a task that blocks on a
/// question gets one outbound message carrying that question, and an
/// `/answer` reply from an allowed sender re-queues the task with the
/// answer appended to its text, the same way `forge answer` does by
/// hand.
#[test]
fn the_signal_plugin_notifies_a_blocked_task_and_files_an_answer_from_a_reply() {
    let e = Env::new();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/signal");

    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );

    let outgoing = e._dir.path().join("signal-out.txt");
    let incoming = e._dir.path().join("signal-in.txt");
    std::fs::write(&outgoing, "").unwrap();
    std::fs::write(&incoming, "").unwrap();

    std::fs::write(
        e.home.join("plugins/signal/config"),
        format!(
            "SIGNAL_ACCOUNT=+15555550100\n\
             SIGNAL_TO=+15555550199\n\
             SIGNAL_ALLOWED=+15555550199\n\
             POLL_SECONDS=1\n\
             TARGET_REPO={}\n\
             WORKFLOW=direct\n\
             NOTIFY_ON=blocked failed\n",
            e.repo.display()
        ),
    )
    .unwrap();

    let stub_dir = e._dir.path().join("stub-bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    std::fs::write(
        stub_dir.join("signal-cli"),
        format!(
            "#!/bin/sh\n\
             case \"$3\" in\n\
             send)\n\
             shift 3\n\
             msg=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
             -m) msg=$2; shift 2 ;;\n\
             *) shift ;;\n\
             esac\n\
             done\n\
             printf '%s\\n===\\n' \"$msg\" >> {out}\n\
             ;;\n\
             receive)\n\
             if [ -s {inc} ]; then\n\
             cat {inc}\n\
             : > {inc}\n\
             fi\n\
             ;;\n\
             esac\n",
            out = outgoing.display(),
            inc = incoming.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        stub_dir.join("signal-cli"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "signal"])
            .status
            .success()
    );

    let id = e.add(&[]);

    let path = format!("{}:{}", stub_dir.display(), std::env::var("PATH").unwrap());
    let stderr_path = e.home.join("worker-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let mut child = e
        .cmd("needsinput.sh")
        .env("PATH", path)
        .args(["work"])
        .stderr(stderr_file)
        .spawn()
        .unwrap();

    assert!(
        wait_until(|| e.task(id).0 == "blocked", Duration::from_secs(20)),
        "the task never blocked: {:?}",
        std::fs::read_to_string(&stderr_path)
    );

    assert!(
        wait_until(
            || std::fs::read_to_string(&outgoing)
                .unwrap_or_default()
                .contains("Which answer file"),
            Duration::from_secs(10)
        ),
        "expected the question in {}: {:?}",
        outgoing.display(),
        std::fs::read_to_string(&outgoing)
    );

    std::fs::write(
        &incoming,
        format!(
            "{}\n",
            serde_json::json!({
                "envelope": {
                    "source": "+15555550199",
                    "sourceNumber": "+15555550199",
                    "dataMessage": {"message": format!("/answer {id} Use answer.txt")}
                }
            })
        ),
    )
    .unwrap();

    assert!(
        wait_until(
            || e.db()
                .query_row(
                    "SELECT task, retry_of FROM tasks WHERE retry_of=?1",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
                )
                .optional()
                .unwrap()
                .is_some(),
            Duration::from_secs(10)
        ),
        "the answer never re-queued a task"
    );

    let (task_text, retry_of): (String, Option<i64>) = e
        .db()
        .query_row(
            "SELECT task, retry_of FROM tasks WHERE retry_of=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(retry_of, Some(id));
    assert!(
        task_text.contains("Use answer.txt"),
        "expected the answer in the re-queued task's text: {task_text}"
    );

    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let _ = child.wait();
}
