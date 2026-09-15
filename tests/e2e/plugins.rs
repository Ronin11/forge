use crate::support::*;

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

    // `forge plugin status` reports enabled/not, with supervision unimplemented.
    let o = e.forge("ok.sh", &["plugin", "status", "notify", "--json"]);
    assert!(o.status.success());
    let status: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(status["name"], "notify");
    assert_eq!(status["enabled"], false);
    assert!(
        status["supervision"]
            .as_str()
            .unwrap()
            .contains("not yet implemented"),
        "{status:?}"
    );

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
