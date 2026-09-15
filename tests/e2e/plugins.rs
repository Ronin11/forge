use crate::support::*;
use std::os::unix::fs::PermissionsExt;

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
