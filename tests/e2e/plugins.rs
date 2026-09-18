// Rule for every test in this file: no test asserts on a clock. A plugin's
// poll loop and any fake it drives (`gh`, `signal-cli`, ...) run on their
// own schedule, so a test must never assume a fixed sleep gave them enough
// wall-clock time to finish a step. Instead, wait on a file or database row
// the plugin writes only once that step is actually done, bounded by a
// generous `wait_until` timeout, and never assert on a related-but-weaker
// condition (e.g. "a task exists") as a stand-in for the one that actually
// matters (e.g. "its ref was filed").

use crate::support::*;
use rusqlite::OptionalExtension;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

fn plugin_status_json(e: &Env, name: &str) -> serde_json::Value {
    serde_json::from_slice(
        &e.forge("ok.sh", &["plugin", "status", name, "--json"])
            .stdout,
    )
    .unwrap()
}

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

/// Disabling then re-enabling a plugin already replaces its process (the
/// reconcile loop in `Supervisor::start` removes and re-adds it), but a
/// plugin that stays enabled the whole time never gets a fresh process on
/// its own: a config edit under `FORGE_PLUGIN_DIR/config` sits unread
/// until something restarts it. `forge plugin restart` is that something:
/// it replaces the process without ever touching the enabled flag.
#[test]
fn plugin_restart_reloads_config_without_touching_the_enabled_flag() {
    let e = Env::new();
    let plugin_dir = e.home.join("plugins").join("reloader");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "name = \"reloader\"\nrun = [\"./run.sh\"]\ncapabilities = [\"events\"]\n",
    )
    .unwrap();
    // Reads its config once at startup, records what it saw, then sits
    // there running: the same shape as the reference plugins (see
    // plugins/notify/notify.sh), which is exactly why an edit to a live
    // plugin's config needs an explicit restart to take effect.
    std::fs::write(
        plugin_dir.join("run.sh"),
        "#!/bin/bash\nset -u\nvalue=unset\nconfig=\"$FORGE_PLUGIN_DIR/config\"\nif [ -f \"$config\" ]; then\n    while IFS= read -r line || [ -n \"$line\" ]; do\n        case \"$line\" in\n            VALUE=*) value=${line#VALUE=} ;;\n        esac\n    done <\"$config\"\nfi\necho \"$value\" >\"$FORGE_PLUGIN_STATE/observed\"\nexec sleep 3600\n",
    )
    .unwrap();
    std::fs::set_permissions(
        plugin_dir.join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "reloader"])
            .status
            .success()
    );

    let observed = e
        .home
        .join("plugins-state")
        .join("reloader")
        .join("observed");

    let mut worker = Worker::spawn(e.cmd("ok.sh").args(["work", "--poll", "1"]));

    assert!(
        wait_until(
            || std::fs::read_to_string(&observed).ok().as_deref() == Some("unset\n"),
            Duration::from_secs(10),
        ),
        "expected the first process to start with no config: {:?}",
        std::fs::read_to_string(&observed)
    );
    let before_pid = plugin_status_json(&e, "reloader")["pid"].as_i64().unwrap();

    std::fs::write(plugin_dir.join("config"), "VALUE=updated\n").unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "restart", "reloader"])
            .status
            .success()
    );

    assert!(
        wait_until(
            || std::fs::read_to_string(&observed).ok().as_deref() == Some("updated\n"),
            Duration::from_secs(30),
        ),
        "expected a fresh process to observe the edited config: {:?}",
        std::fs::read_to_string(&observed)
    );
    let after = plugin_status_json(&e, "reloader");
    assert_eq!(after["enabled"], true, "restart never touches enabled");
    assert_ne!(
        after["pid"].as_i64().unwrap(),
        before_pid,
        "restart must replace the process, not reuse it"
    );

    worker.stop();
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
/// installed directory with the task id as `$1`, the task's state as
/// `$2`, and the first line of its reason as `$3`.
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
            "#!/bin/sh\ncat >/dev/null\necho \"$1|$2|$3\" >> {}\n",
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

    // A fake that blocks on a question (like needsinput.sh, but pausing
    // first when FAKE_SLEEP is set) so the command sees a non-empty state
    // and reason instead of the empty reason a plain success leaves
    // behind, with enough wall-clock time for the plugin to subscribe
    // before task_done fires.
    let claude_fake = e._dir.path().join("needsinput-slow.sh");
    std::fs::write(
        &claude_fake,
        "#!/bin/bash\n\
         cat >/dev/null\n\
         [ -n \"$FAKE_SLEEP\" ] && sleep 2\n\
         echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":2,\"total_cost_usd\":0.01,\"result\":\"done\",\"structured_output\":{\"schema_version\":1,\"summary\":\"blocked\",\"needs_input\":{\"tried\":\"read the tree and the task; stopped before writing anything\",\"question\":\"Which answer file: answer.txt or ANSWER.txt?\",\"options\":[\"answer.txt\",\"ANSWER.txt\"],\"context\":\"\",\"checkpoint\":null},\"changes\":[],\"checks_run\":[],\"claims\":[]}}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&claude_fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("FORGE2_CLAUDE_BIN", &claude_fake)
        .env("FAKE_SLEEP", "1")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let want = format!("{id}|blocked|needs input: Which answer file: answer.txt or ANSWER.txt?");
    assert!(
        wait_until(
            || std::fs::read_to_string(&hits)
                .unwrap_or_default()
                .lines()
                .any(|l| l == want),
            Duration::from_secs(5)
        ),
        "expected {want:?} in {}: {:?}",
        hits.display(),
        std::fs::read_to_string(&hits)
    );
}

/// A fake `rsync` for a `host=local` deploy target: records nothing, just
/// copies its source into its destination (no `host:` prefix to strip,
/// unlike the SSH-reaching fakes in tests/e2e/deploy.rs, since a local
/// target never shells out to `ssh`).
const FAKE_LOCAL_RSYNC: &str = r#"#!/bin/bash
args=()
for a in "$@"; do
  case "$a" in
    -*) ;;
    *) args+=("$a") ;;
  esac
done
src="${args[0]}"
dest="${args[1]}"
mkdir -p "$dest"
cp -a "$src"/. "$dest"/
"#;

/// notify.sh treats `deploy_finished` as a notification too (docs/DEPLOY.md,
/// "When a deploy runs"): an on-landing target whose check always fails
/// gets no previous deploy to roll back to, so it finishes `ok: false`
/// with no rollback, and the fake `command` sees it — carrying the
/// project, target and sha — without any config turning failure
/// notifications on (they are on by default; only a passing deploy is
/// quiet unless `NOTIFY_DEPLOY_OK=1`).
#[test]
fn the_notify_plugin_gets_a_deploy_finished_with_ok_false() {
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
            "#!/bin/sh\ncat >/dev/null\necho \"$1|$2|$3|$4|$5\" >> {}\n",
            hits.display()
        ),
    )
    .unwrap();

    assert!(
        e.forge("ok.sh", &["plugin", "enable", "notify"])
            .status
            .success()
    );

    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "demo",
                "--purpose",
                "p",
                "--repo",
                e.repo.to_str().unwrap(),
            ],
        )
        .status
        .success()
    );

    let remote = e._dir.path().join("remote");
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            e.repo.to_str().unwrap(),
            "--method",
            "deploy-command",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={}", remote.display()),
            "--arg",
            "command=true",
            "--check",
            "false",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let rsync_path = fakebin.join("rsync");
    std::fs::write(&rsync_path, FAKE_LOCAL_RSYNC).unwrap();
    std::fs::set_permissions(&rsync_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    e.add(&[]);

    // FAKE_SLEEP gives the plugin time to subscribe from its offset
    // before the task lands and its on-landing deploy fires, exactly as
    // `the_reference_plugin_runs_its_command_on_task_done` does.
    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("FAKE_SLEEP", "1")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let sha: String = e
        .db()
        .query_row(
            "SELECT sha FROM deploys WHERE project='demo' AND target='prod'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let want = format!("deploy|demo|prod|{sha}|failed");
    assert!(
        wait_until(
            || std::fs::read_to_string(&hits)
                .unwrap_or_default()
                .lines()
                .any(|l| l == want),
            Duration::from_secs(10)
        ),
        "expected {want:?} in {}: {:?}",
        hits.display(),
        std::fs::read_to_string(&hits)
    );
}

/// The github-issues plugin end to end, against a stub `gh`: intake files a
/// task for the one open issue the stub serves, records the issue as a
/// `ref`, and the events side comments back on the issue (and applies
/// `DONE_LABEL`) once the task lands.
#[test]
fn github_issues_files_a_task_and_reports_back_when_it_lands() {
    let e = Env::new();

    let bin_dir = e._dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fixture = bin_dir.join("issues.json").display().to_string();
    std::fs::write(
        &fixture,
        serde_json::json!([{
            "number": 42,
            "title": "Add a frobnicator",
            "body": "Please add a frobnicator.\nThanks!",
            "url": "https://github.com/acme/widgets/issues/42",
        }])
        .to_string(),
    )
    .unwrap();
    let calls = bin_dir.join("gh-calls.txt").display().to_string();
    std::fs::write(
        bin_dir.join("gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"issue list\") cat {fixture:?} ;;\n  \"issue comment\"|\"issue edit\") printf '%s\\n' \"$*\" >> {calls:?} ;;\n  *) echo \"gh-stub: unhandled $*\" >&2; exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin_dir.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/github-issues");
    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );
    std::fs::write(
        e.home.join("plugins/github-issues/config"),
        format!(
            "GH_REPO=acme/widgets\nLABEL=forge\nDONE_LABEL=forge-done\nPOLL_SECONDS=30\nTARGET_REPO={}\nWORKFLOW=direct\n",
            e.repo.display()
        ),
    )
    .unwrap();
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "github-issues"])
            .status
            .success()
    );

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut worker = Worker::spawn(e.cmd("ok.sh").env("PATH", path).args(["work", "--poll", "1"]));

    // Wait on the plugin's own `filed` state file rather than the tasks
    // table: `intake_once` writes it only after both `forge add` and
    // `forge ref add` have completed, so once it names the issue the ref
    // is guaranteed to exist too. Racing on the task row instead (as an
    // earlier version of this test did) could catch the task between
    // those two calls, with no ref written yet.
    let filed = e.home.join("plugins-state/github-issues/filed");
    assert!(
        wait_until(
            || std::fs::read_to_string(&filed)
                .unwrap_or_default()
                .starts_with("42 "),
            Duration::from_secs(15)
        ),
        "expected issue 42 filed: {:?}",
        std::fs::read_to_string(&filed)
    );
    let filed_text = std::fs::read_to_string(&filed).unwrap();
    let task_id: i64 = filed_text
        .split_whitespace()
        .nth(1)
        .expect("filed line is \"<issue> <task>\"")
        .parse()
        .unwrap();

    let refs: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["ref", "list", &task_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(refs[0]["kind"], "issue");
    assert_eq!(refs[0]["url"], "https://github.com/acme/widgets/issues/42");
    assert_eq!(refs[0]["by"], "github-issues");

    assert!(
        wait_until(|| e.task(task_id).0 == "succeeded", Duration::from_secs(15)),
        "expected the task to land: {:?}",
        e.task(task_id)
    );
    let (_, reason, _) = e.task(task_id);
    assert!(reason.starts_with("landed"), "{reason}");

    // Wait for the `edit 42` call, not just the `comment 42` that precedes
    // it: the events loop runs both `gh` calls one after another for the
    // same `task_done` line, but they are separate subprocesses, so
    // stopping at the first one and asserting on the second immediately
    // (with no wait) is itself a clock assumption under load.
    assert!(
        wait_until(
            || std::fs::read_to_string(&calls)
                .unwrap_or_default()
                .contains("edit 42"),
            Duration::from_secs(15)
        ),
        "expected a gh issue edit call: {:?}",
        std::fs::read_to_string(&calls)
    );
    let calls_text = std::fs::read_to_string(&calls).unwrap();
    assert!(calls_text.contains("comment 42"), "{calls_text}");
    assert!(calls_text.contains("succeeded"), "{calls_text}");
    assert!(calls_text.contains("landed"), "{calls_text}");
    assert!(
        calls_text.contains("edit 42") && calls_text.contains("forge-done"),
        "expected DONE_LABEL applied: {calls_text}"
    );

    worker.stop();
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
    let mut worker = Worker::spawn(
        e.cmd("needsinput.sh")
            .env("PATH", path)
            .args(["work"])
            .stderr(stderr_file),
    );

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

    worker.stop();
}

/// Intake's addressee mechanism (docs/INTAKE.md), end to end against the
/// same stub `signal-cli`: a fake agent that blocks with
/// `needs_input.to = "alice"`, a CONTACTS entry mapping that name to her
/// number, makes the plugin send the question to her number instead of
/// the operator's SIGNAL_TO, and a reply from her number — not prefixed
/// `/answer`, since she never names the task herself — re-queues the
/// task with the answer recorded as `answered_by = "alice"`, and the
/// decision's `answered_for` names her too.
#[test]
fn the_signal_plugin_delivers_an_addressed_question_to_its_contact_and_records_her_answer() {
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

    let operator_number = "+15555550199";
    let alice_number = "+15555550111";

    std::fs::write(
        e.home.join("plugins/signal/config"),
        format!(
            "SIGNAL_ACCOUNT=+15555550100\n\
             SIGNAL_TO={operator_number}\n\
             SIGNAL_ALLOWED={operator_number}\n\
             CONTACTS=alice:{alice_number}\n\
             POLL_SECONDS=1\n\
             TARGET_REPO={}\n\
             WORKFLOW=direct\n\
             NOTIFY_ON=blocked failed\n",
            e.repo.display()
        ),
    )
    .unwrap();

    // Like the stub in the sibling test, but each outgoing send also
    // records its destination number ahead of the message, so the test
    // can tell alice's number apart from the operator's.
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
             dest=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
             -m) msg=$2; shift 2 ;;\n\
             -g) dest=$2; shift 2 ;;\n\
             *) dest=$1; shift ;;\n\
             esac\n\
             done\n\
             printf '%s|%s\\n===\\n' \"$dest\" \"$msg\" >> {out}\n\
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
    let mut worker = Worker::spawn(
        e.cmd("needsinput-to.sh")
            .env("PATH", path)
            .args(["work"])
            .stderr(stderr_file),
    );

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

    let sent = std::fs::read_to_string(&outgoing).unwrap();
    let question_entry = sent
        .split("===\n")
        .find(|e| e.contains("Which answer file"))
        .unwrap_or_else(|| panic!("no entry carried the question: {sent}"));
    assert!(
        question_entry.starts_with(&format!("{alice_number}|")),
        "the question must go to alice's number, not the operator's: {question_entry:?}"
    );

    // Alice's own reply, at her own number, names no task: the plugin
    // finds the one open question addressed to her.
    std::fs::write(
        &incoming,
        format!(
            "{}\n",
            serde_json::json!({
                "envelope": {
                    "source": alice_number,
                    "sourceNumber": alice_number,
                    "dataMessage": {"message": "Use answer.txt"}
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
    assert!(
        task_text.contains("alice's answer"),
        "expected the re-queued task's text to credit alice, not the operator: {task_text}"
    );

    let (answered_by, answered_for): (String, Option<String>) = e
        .db()
        .query_row(
            "SELECT answered_by, answered_for FROM decisions WHERE task_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(answered_by, "alice");
    assert_eq!(answered_for.as_deref(), Some("alice"));

    worker.stop();
}

fn add_intake(e: &Env, task: &str) -> i64 {
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            task,
            "--workflow",
            "intake",
            "--no-land",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap()
}

fn answer(e: &Env, id: i64, text: &str, by: &str) -> i64 {
    let o = e.forge("ok.sh", &["answer", &id.to_string(), text, "--by", by]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(4)
        .unwrap()
        .parse()
        .unwrap()
}

/// The customer portal, step 4 (docs/PORTAL.md, "Reachable"): a `CONTACTS`
/// name gets their portal link two ways, both against the same stub
/// `signal-cli` as the sibling tests. First, unprompted: `forge intake
/// accept` creating nate's first project emits `project_created`, which
/// the still-running plugin picks up off the same event log and mints him
/// a link. Second, on request: nate texts `/portal` and gets a link back
/// too, minted fresh, for the project the first send already remembered
/// he owns.
#[test]
fn the_signal_plugin_sends_a_contact_their_portal_link_on_intake_accept_and_on_request() {
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

    let operator_number = "+15555550199";
    let nate_number = "+15555550122";

    std::fs::write(
        e.home.join("plugins/signal/config"),
        format!(
            "SIGNAL_ACCOUNT=+15555550100\n\
             SIGNAL_TO={operator_number}\n\
             SIGNAL_ALLOWED={operator_number}\n\
             CONTACTS=nate:{nate_number}\n\
             POLL_SECONDS=1\n\
             TARGET_REPO={}\n\
             WORKFLOW=direct\n\
             NOTIFY_ON=blocked failed\n\
             PORTAL_URL=https://portal.example.com\n",
            e.repo.display()
        ),
    )
    .unwrap();

    // Same dest-recording stub as the addressee test.
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
             dest=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
             -m) msg=$2; shift 2 ;;\n\
             -g) dest=$2; shift 2 ;;\n\
             *) dest=$1; shift ;;\n\
             esac\n\
             done\n\
             printf '%s|%s\\n===\\n' \"$dest\" \"$msg\" >> {out}\n\
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

    let mut id = add_intake(&e, "Nate runs a shop. Contact: nate.");

    let path = format!("{}:{}", stub_dir.display(), std::env::var("PATH").unwrap());
    let stderr_path = e.home.join("worker-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let mut worker = Worker::spawn(
        e.cmd("interviewer-confirmed.sh")
            .env("PATH", path)
            // The default 30s idle poll would outlast the wait below: after
            // nate answers, the worker must notice the newly requeued task
            // itself, not just claim the one already queued at startup.
            .args(["work", "--poll", "1"])
            .stderr(stderr_file),
    );

    // Turn 0: the interviewer blocks, addressed to nate, asking him to
    // confirm the brief.
    assert!(
        wait_until(|| e.task(id).0 == "blocked", Duration::from_secs(20)),
        "the intake task never blocked: {:?}",
        std::fs::read_to_string(&stderr_path)
    );

    id = answer(&e, id, "Yes, that's right.", "nate");

    // Turn 1: confirmed, so the intake task succeeds.
    assert!(
        wait_until(|| e.task(id).0 == "succeeded", Duration::from_secs(20)),
        "the intake task never succeeded: {:?}",
        std::fs::read_to_string(&stderr_path)
    );

    let accept = e.forge("ok.sh", &["intake", "accept", &id.to_string()]);
    assert!(
        accept.status.success(),
        "{}",
        String::from_utf8_lossy(&accept.stderr)
    );
    assert!(
        String::from_utf8_lossy(&accept.stdout).contains("created project nate"),
        "{}",
        String::from_utf8_lossy(&accept.stdout)
    );

    // Unprompted: the plugin saw `project_created` on the same event log
    // it is still tailing, and sent nate his portal link without being
    // asked.
    assert!(
        wait_until(
            || std::fs::read_to_string(&outgoing)
                .unwrap_or_default()
                .contains("https://portal.example.com/p/"),
            Duration::from_secs(10)
        ),
        "expected an unprompted portal link in {}: {:?}",
        outgoing.display(),
        std::fs::read_to_string(&outgoing)
    );

    let after_accept = std::fs::read_to_string(&outgoing).unwrap();
    let first_entry = after_accept
        .split("===\n")
        .find(|e| e.contains("https://portal.example.com/p/"))
        .unwrap_or_else(|| panic!("no entry carried the unprompted link: {after_accept}"));
    assert!(
        first_entry.starts_with(&format!("{nate_number}|")),
        "the unprompted link must go to nate's number, not the operator's: {first_entry:?}"
    );

    // On request: nate texts /portal and gets his link back too, minted
    // fresh (a different token from the unprompted one).
    std::fs::write(
        &incoming,
        format!(
            "{}\n",
            serde_json::json!({
                "envelope": {
                    "source": nate_number,
                    "sourceNumber": nate_number,
                    "dataMessage": {"message": "/portal"}
                }
            })
        ),
    )
    .unwrap();

    assert!(
        wait_until(
            || {
                std::fs::read_to_string(&outgoing)
                    .unwrap_or_default()
                    .matches("https://portal.example.com/p/")
                    .count()
                    >= 2
            },
            Duration::from_secs(10)
        ),
        "expected a second portal link after /portal in {}: {:?}",
        outgoing.display(),
        std::fs::read_to_string(&outgoing)
    );

    let after_portal = std::fs::read_to_string(&outgoing).unwrap();
    let entries: Vec<&str> = after_portal
        .split("===\n")
        .filter(|e| e.contains("https://portal.example.com/p/"))
        .collect();
    assert_eq!(entries.len(), 2, "{after_portal:?}");
    assert!(
        entries[1].starts_with(&format!("{nate_number}|")),
        "the /portal reply must go to nate's number: {:?}",
        entries[1]
    );
    assert_ne!(
        entries[0], entries[1],
        "each mint should be a fresh token: {after_portal:?}"
    );

    worker.stop();
}

/// The Signal plugin routes through the concierge (docs/INTAKE.md, "The
/// front door is not the interview"): unlike the sibling tests above,
/// this runs `plugins/signal/signal.sh` directly rather than through
/// `forge plugin install`/`forge work`, because a live plugin's own
/// child processes only get the production agent env
/// (`agent::agent_env`), which never carries the `FORGE2_CLAUDE_BIN`
/// test seam — and this is the first plugin behavior that needs one,
/// since `forge ask` runs the concierge directive as a real agent turn.
/// A contact's plain message, not a command and not an answer to an
/// open question, becomes `forge ask <project> <message> --from <name>`
/// (the project named by `PROJECTS`), and the concierge's "request"
/// decision (faked here the same way `tests/e2e/concierge.rs` does)
/// both files a task on that project and gets "on it" sent back to her.
#[test]
fn the_signal_plugin_routes_a_contacts_message_through_the_concierge() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "demo",
                "--purpose",
                "Demo runs a small repair shop over text messages.",
                "--repo",
                repo,
            ],
        )
        .status
        .success()
    );

    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugin");
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();

    std::fs::write(
        plugin_dir.join("config"),
        format!(
            "SIGNAL_ACCOUNT=+15555550100\n\
             SIGNAL_TO=+15555550199\n\
             CONTACTS=alice:+15555550111\n\
             PROJECTS=alice:demo\n\
             POLL_SECONDS=1\n\
             TARGET_REPO={repo}\n\
             WORKFLOW=direct\n"
        ),
    )
    .unwrap();

    let message = tmp.path().join("message.json");
    std::fs::write(
        &message,
        "{\"envelope\":{\"source\":\"+15555550111\",\"sourceNumber\":\"+15555550111\",\
         \"dataMessage\":{\"message\":\"My printer broke, can someone come by Tuesday?\"}}}\n",
    )
    .unwrap();

    // A fake `signal-cli`: `receive --json` prints the one scripted
    // message the first time it's called and nothing after (the plugin
    // polls in a loop), and `send` records what was sent, to whom, in
    // `$FORGE_PLUGIN_STATE/sent`.
    let signal_cli = tmp.path().join("signal-cli-fake.sh");
    std::fs::write(
        &signal_cli,
        r#"#!/bin/sh
set -u
shift
shift
cmd=$1
shift
case "$cmd" in
    send)
        msg=""
        dest=""
        while [ $# -gt 0 ]; do
            case "$1" in
                -m) msg=$2; shift 2 ;;
                -g) dest=$2; shift 2 ;;
                *) dest=$1; shift ;;
            esac
        done
        printf 'SEND %s %s\n' "$dest" "$msg" >>"$FORGE_PLUGIN_STATE/sent"
        ;;
    receive)
        flag="$FORGE_PLUGIN_STATE/received"
        if [ ! -f "$flag" ]; then
            touch "$flag"
            cat "$FAKE_MESSAGE_FILE"
        fi
        ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&signal_cli, std::fs::Permissions::from_mode(0o755)).unwrap();

    let signal_sh =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/signal/signal.sh");
    let claude_fake =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/concierge-request.sh");

    let mut cmd = Command::new("sh");
    cmd.arg(&signal_sh)
        .env("FORGE_BIN", env!("CARGO_BIN_EXE_forge"))
        .env("FORGE2_HOME", &e.home)
        .env("FORGE2_CLAUDE_BIN", &claude_fake)
        .env("FORGE2_SUPERVISOR", "0")
        .env("FORGE_PLUGIN_DIR", &plugin_dir)
        .env("FORGE_PLUGIN_STATE", &state_dir)
        .env("SIGNAL_CLI", &signal_cli)
        .env("FAKE_MESSAGE_FILE", &message)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if e.sandbox_disabled() {
        cmd.env("FORGE2_SANDBOX", "0");
    }

    let mut child = cmd.spawn().unwrap();

    let sent = state_dir.join("sent");
    assert!(
        wait_until(
            || std::fs::read_to_string(&sent)
                .unwrap_or_default()
                .contains("on it"),
            Duration::from_secs(30),
        ),
        "expected an acknowledgement to alice; sent so far: {:?}",
        std::fs::read_to_string(&sent)
    );

    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let _ = child.wait();

    let sent_text = std::fs::read_to_string(&sent).unwrap();
    assert!(sent_text.contains("SEND +15555550111 on it"), "{sent_text}");

    let (project, task, title): (String, String, Option<String>) = e
        .db()
        .query_row(
            "SELECT project, task, title FROM tasks WHERE project='demo' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(project, "demo");
    assert!(task.contains("usually same day"), "{task}");
    assert_eq!(
        title.as_deref(),
        Some("My printer broke, can someone come by Tuesday?")
    );
}
