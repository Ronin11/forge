use super::*;

// REVIEW-4 §6 finding 12: the leader exits successfully while its child
// ignores SIGTERM. Keep the guard alive until after checking the child so
// its Drop cannot hide a missing sweep on either normal completion path.
async fn exiting_leader_leaves_no_child(stop: bool) {
    let dir = tempfile::tempdir().unwrap();
    let script = concat!(
        "trap '' TERM; sleep 1000 & ",
        "echo $! > \"$FORGE_PLUGIN_STATE/child\"; ",
        "trap 'exit 0' TERM; touch \"$FORGE_PLUGIN_STATE/ready\"; ",
        "while [ ! -e \"$FORGE_PLUGIN_STATE/exit\" ]; do sleep 0.02; done"
    );
    let f = forge_enabling(dir.path(), &[("orphan", script)]);
    let catalog = load_catalog(dir.path(), &[]);
    let plugin = &catalog.plugins["orphan"];
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let lock = try_lock_plugin(dir.path(), "orphan").unwrap();
    let mut child = spawn_plugin(
        plugin,
        &f.paths.home,
        &state,
        &dir.path().join("plugin.log"),
        &lock,
    )
    .unwrap();
    let mut group = GroupGuard(child.id().unwrap() as libc::pid_t);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state.join("ready").exists() {
        assert!(Instant::now() < deadline, "plugin never became ready");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let kid: i32 = std::fs::read_to_string(state.join("child"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(kid, 0) }, 0);

    let started = Instant::now();
    if stop {
        stop_child(&mut child, &mut group).await;
        assert!(started.elapsed() < STOP_GRACE, "leader missed the grace");
    } else {
        std::fs::write(state.join("exit"), "").unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_and_sweep(&mut child, &mut group),
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert!(child.wait().await.unwrap().success());
    assert_eq!(group.0, 0, "guard must not signal a reaped leader's PID");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // An orphan zombie has exited too; its final removal depends on
        // the host's init/subreaper, not the plugin supervisor.
        match std::fs::read_to_string(format!("/proc/{kid}/stat")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with('Z') => break,
            Err(e) => panic!("reading child state: {e}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "sleep 1000 child {kid} survived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn stop_sweeps_children_when_the_leader_exits_during_sigterm_grace() {
    exiting_leader_leaves_no_child(true).await;
}

#[tokio::test]
async fn spontaneous_leader_exit_sweeps_children_before_restarting() {
    exiting_leader_leaves_no_child(false).await;
}

#[test]
fn concurrent_restart_requests_use_private_temporary_files() {
    let home = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let home = home.path();
            scope.spawn(move || {
                for _ in 0..20 {
                    request_restart(home, "example").unwrap();
                }
            });
        }
    });
    assert!(read_restart_gen(home.path(), "example") > 0);
    let path = restart_request_path(home.path(), "example");
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

/// Writes `<root>/<name>/plugin.toml`; `root` is a root directory
/// itself (what `load_catalog` scans directly, e.g. a `plugin_dirs`
/// entry), not `<FORGE_HOME>`.
fn write_plugin(root: &Path, name: &str, text: &str) {
    let plugin_dir = root.join(name);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.toml"), text).unwrap();
}

/// Writes `<home>/plugins/<name>/plugin.toml`, the built-in root
/// `load_catalog` always scans first.
fn write_manifest(home: &Path, name: &str, text: &str) {
    write_plugin(&home.join("plugins"), name, text);
}

#[test]
fn manifest_parses_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "notify",
        "name = \"notify\"\ndescription = \"posts a notification\"\nrun = [\"./notify.sh\"]\ncapabilities = [\"events\"]\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(cat.problems.is_empty(), "{:?}", cat.problems);
    let p = cat.plugins.get("notify").unwrap();
    assert_eq!(p.manifest.description, "posts a notification");
    assert_eq!(p.manifest.run, vec!["./notify.sh"]);
    assert_eq!(p.manifest.build, None);
    assert_eq!(
        p.manifest.capabilities,
        [Capability::Events].into_iter().collect()
    );
    assert_eq!(p.manifest.restart, Restart::OnFailure, "the default");
}

#[test]
fn manifest_parses_every_field() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "inbox",
        "name = \"inbox\"\ndescription = \"files tasks\"\nrun = [\"./inbox\"]\nbuild = [\"cargo\", \"build\", \"--release\"]\ncapabilities = [\"events\", \"intake\"]\nrestart = \"always\"\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(cat.problems.is_empty(), "{:?}", cat.problems);
    let p = cat.plugins.get("inbox").unwrap();
    assert_eq!(
        p.manifest.build,
        Some(vec!["cargo".into(), "build".into(), "--release".into()])
    );
    assert_eq!(
        p.manifest.capabilities,
        [Capability::Events, Capability::Intake]
            .into_iter()
            .collect()
    );
    assert_eq!(p.manifest.restart, Restart::Always);
}

#[test]
fn unknown_fields_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "bad",
        "name = \"bad\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\ntypo = true\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(!cat.plugins.contains_key("bad"));
    assert!(
        cat.problems
            .iter()
            .any(|p| p.blocking && p.what.contains("unknown field")),
        "{:?}",
        cat.problems
    );
}

#[test]
fn empty_run_and_empty_capabilities_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "norun",
        "name = \"norun\"\nrun = []\ncapabilities = [\"events\"]\n",
    );
    write_manifest(
        dir.path(),
        "nocap",
        "name = \"nocap\"\nrun = [\"x\"]\ncapabilities = []\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(!cat.plugins.contains_key("norun"));
    assert!(!cat.plugins.contains_key("nocap"));
    assert!(
        cat.problems
            .iter()
            .any(|p| p.what.contains("`run` is empty"))
    );
    assert!(
        cat.problems
            .iter()
            .any(|p| p.what.contains("`capabilities` is empty"))
    );
}

#[test]
fn an_unknown_capability_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "weird",
        "name = \"weird\"\nrun = [\"x\"]\ncapabilities = [\"tools\"]\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(!cat.plugins.contains_key("weird"));
    assert!(cat.problems.iter().any(|p| p.blocking));
}

#[test]
fn the_directory_name_must_match_the_manifest_name() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "on-disk",
        "name = \"other\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
    );
    let cat = load_catalog(dir.path(), &[]);
    assert!(cat.plugins.is_empty());
    assert!(
        cat.problems
            .iter()
            .any(|p| p.what.contains("does not match the directory name")),
        "{:?}",
        cat.problems
    );
}

#[test]
fn an_earlier_root_wins_a_duplicate_name_and_the_shadowed_copy_is_a_warning() {
    let home = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    write_manifest(
        home.path(),
        "notify",
        "name = \"notify\"\ndescription = \"home copy\"\nrun = [\"./a\"]\ncapabilities = [\"events\"]\n",
    );
    write_plugin(
        extra.path(),
        "notify",
        "name = \"notify\"\ndescription = \"extra copy\"\nrun = [\"./b\"]\ncapabilities = [\"events\"]\n",
    );
    let cat = load_catalog(home.path(), &[extra.path().to_path_buf()]);
    let p = cat.plugins.get("notify").unwrap();
    assert_eq!(p.manifest.description, "home copy", "the earlier root wins");
    assert!(
        cat.problems
            .iter()
            .any(|p| !p.blocking && p.what.contains("shadowed")),
        "{:?}",
        cat.problems
    );
}

#[test]
fn a_missing_configured_root_is_a_non_blocking_problem() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("does-not-exist");
    let cat = load_catalog(home.path(), std::slice::from_ref(&missing));
    assert!(cat.plugins.is_empty());
    assert_eq!(cat.problems.len(), 1);
    assert!(!cat.problems[0].blocking);
    assert!(cat.problems[0].what.contains("does not exist"));
    assert_eq!(cat.problems[0].file, missing.display().to_string());
}

#[test]
fn a_missing_home_plugins_dir_is_not_a_problem() {
    let home = tempfile::tempdir().unwrap();
    let cat = load_catalog(home.path(), &[]);
    assert!(cat.plugins.is_empty());
    assert!(cat.problems.is_empty());
}

#[test]
fn one_broken_plugin_does_not_stop_the_others_loading() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "good",
        "name = \"good\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
    );
    write_manifest(dir.path(), "bad", "not valid toml [[[");
    let cat = load_catalog(dir.path(), &[]);
    assert!(cat.plugins.contains_key("good"));
    assert!(!cat.plugins.contains_key("bad"));
    assert!(cat.problems.iter().any(|p| p.blocking));
}

/// docs/REVIEW-3.md §3.1 item 8: a shell plugin's pipeline (`sleep 1000
/// | cat`) must not outlive `Supervisor::stop`. `spawn_plugin` puts the
/// plugin in its own process group and `stop_child` signals the group,
/// so stopping the leader (the shell) also reaches the children it
/// forked for the pipeline, which never install a SIGTERM trap of
/// their own.
#[tokio::test]
async fn stop_kills_a_shell_plugins_whole_pipeline_not_just_its_leader() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        "pipeline",
        "name = \"pipeline\"\nrun = [\"sh\", \"-c\", \"sleep 1000 | cat\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n",
    );
    let store = crate::store::Store::open(&dir.path().join("forge.db")).unwrap();
    let paths = crate::ctx::Paths {
        home: dir.path().to_path_buf(),
        worktrees: dir.path().join("worktrees"),
        logs: dir.path().join("logs"),
    };
    let f = crate::ctx::Forge::open_with(paths, store).unwrap();
    f.store
        .set_plugin_enabled("pipeline", true, crate::unix_now())
        .unwrap();
    let f = Arc::new(f);
    let sup = Supervisor::start(f.clone());

    let deadline = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        if let RunState::Running { pid, .. } = read_run_state(&f.paths.home, "pipeline") {
            break pid;
        }
        assert!(Instant::now() < deadline, "the plugin never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // `sleep` and `cat`, the shell's own direct children for the
    // pipeline: waited for by pid, not group, so the assertion below
    // is not itself relying on the fix under test (`process_group(0)`)
    // to find them.
    let deadline = Instant::now() + Duration::from_secs(10);
    let children = loop {
        let out = std::process::Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .unwrap();
        let kids: Vec<i32> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        if kids.len() >= 2 {
            break kids;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never forked its pipeline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    sup.stop().await;

    // `kill(pid, 0)` still finds a just-killed process while it is a
    // zombie awaiting reaping by whatever it was reparented to (pid 1,
    // usually), which a busy machine can be slow to get to; a short
    // poll instead of one immediate check keeps this test about the
    // group having been signalled, not about reaper scheduling.
    let deadline = Instant::now() + Duration::from_secs(5);
    for kid in children {
        loop {
            let alive = unsafe { libc::kill(kid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected pipeline child {kid} reaped after stop"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn forge_enabling(dir: &Path, plugins: &[(&str, &str)]) -> Arc<crate::ctx::Forge> {
    let store = crate::store::Store::open(&dir.join("forge.db")).unwrap();
    let paths = crate::ctx::Paths {
        home: dir.to_path_buf(),
        worktrees: dir.join("worktrees"),
        logs: dir.join("logs"),
    };
    let f = crate::ctx::Forge::open_with(paths, store).unwrap();
    for (name, run) in plugins {
        let run = run.replace('\\', "\\\\").replace('"', "\\\"");
        write_manifest(
            dir,
            name,
            &format!(
                "name = \"{name}\"\nrun = [\"sh\", \"-c\", \"{run}\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n"
            ),
        );
        f.store
            .set_plugin_enabled(name, true, crate::unix_now())
            .unwrap();
    }
    Arc::new(f)
}

async fn wait_running(home: &Path, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !matches!(read_run_state(home, name), RunState::Running { .. }) {
        assert!(Instant::now() < deadline, "{name} never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// docs/REVIEW-3.md §3.1 item 9: a draining worker's plugins are stopped
/// together. Each plugin, on SIGTERM, waits for the other's SIGTERM to
/// arrive before exiting, so a stop that waited on one plugin before
/// signalling the next would leave both waiting out their own bound.
#[tokio::test]
async fn stop_signals_every_plugin_before_waiting_on_any_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let script = |other: &str| {
        format!(
            "trap 'touch \"$FORGE_PLUGIN_STATE/term\"; i=0; while [ ! -e \"$FORGE_PLUGIN_STATE/../{other}/term\" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; [ -e \"$FORGE_PLUGIN_STATE/../{other}/term\" ] && touch \"$FORGE_PLUGIN_STATE/saw_other\"; exit 0' TERM; sleep 1000 & wait"
        )
    };
    let (a, b) = (script("b"), script("a"));
    let f = forge_enabling(dir.path(), &[("a", &a), ("b", &b)]);
    let sup = Supervisor::start(f.clone());
    wait_running(dir.path(), "a").await;
    wait_running(dir.path(), "b").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    sup.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "stop was sequential"
    );
    for name in ["a", "b"] {
        let seen = dir
            .path()
            .join("plugins-state")
            .join(name)
            .join("saw_other");
        assert!(seen.exists(), "{name} was signalled before the other was");
    }
}

/// docs/REVIEW-3.md §3.1 item 9: a supervisor that finds a plugin's
/// `plugins-run/<name>.lock` held leaves the plugin alone; once the
/// holder lets go, a supervisor starts it.
#[tokio::test]
async fn a_plugin_whose_lock_is_held_is_skipped_until_it_is_released() {
    let dir = tempfile::tempdir().unwrap();
    let f = forge_enabling(dir.path(), &[("held", "sleep 1000")]);
    let lock = try_lock_plugin(dir.path(), "held").expect("first lock");
    assert!(try_lock_plugin(dir.path(), "held").is_none());

    let enabled = enabled_plugins_now(&f).unwrap();
    let mut running = BTreeMap::new();
    reconcile(&f, &mut running, &enabled).await;
    assert!(
        running.is_empty(),
        "a completed tick must skip the held lock"
    );
    assert!(matches!(
        read_run_state(dir.path(), "held"),
        RunState::Stopped { last_exit: None }
    ));

    drop(lock);
    let sup = Supervisor::start(f.clone());
    wait_running(dir.path(), "held").await;
    assert!(
        try_lock_plugin(dir.path(), "held").is_none(),
        "the lock is held while the plugin runs"
    );
    sup.stop().await;
    // A sibling test's fork can hold a copy of the lock's descriptor
    // for the instant before it execs; the lock is free right after.
    let mut released = false;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if try_lock_plugin(dir.path(), "held").is_some() {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(released, "the lock is released once the plugin is stopped");
}

fn running_pid(home: &Path, name: &str) -> Option<i64> {
    match read_run_state(home, name) {
        RunState::Running { pid, .. } => Some(pid),
        _ => None,
    }
}

/// docs/REVIEW-4.md §6 item 13: an invalid `config.toml` during a
/// reconcile tick leaves the running set unchanged. The plugin lives
/// under a `plugin_dirs` root, so a supervisor that re-read the file
/// would lose the root and stop it; it scans the worker's validated
/// config instead.
#[tokio::test]
async fn an_invalid_config_toml_during_a_tick_leaves_the_running_set_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let extra = dir.path().join("extra");
    write_plugin(
        &extra,
        "steady",
        "name = \"steady\"\nrun = [\"sleep\", \"1000\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n",
    );
    std::fs::write(
        dir.path().join("config.toml"),
        format!("plugin_dirs = [{:?}]\n", extra.display().to_string()),
    )
    .unwrap();
    let f = forge_enabling(dir.path(), &[]);
    assert_eq!(f.plugin_dirs, vec![extra.clone()]);
    f.store
        .set_plugin_enabled("steady", true, crate::unix_now())
        .unwrap();
    let sup = Supervisor::start_every(f.clone(), Duration::from_millis(50));
    wait_running(dir.path(), "steady").await;
    let pid = running_pid(dir.path(), "steady").unwrap();

    std::fs::write(dir.path().join("config.toml"), "plugin_dirs = [\n").unwrap();
    assert!(crate::config::load_home(dir.path()).is_err());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        running_pid(dir.path(), "steady"),
        Some(pid),
        "the plugin was stopped or replaced over an invalid config.toml"
    );
    sup.stop().await;
}

/// docs/REVIEW-4.md §6 item 13: a plugin whose `plugin.toml` is being
/// rewritten is broken, not absent, and keeps running; one whose
/// directory is gone is absent and is stopped.
#[tokio::test]
async fn a_broken_manifest_keeps_its_plugin_running_and_an_absent_one_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let f = forge_enabling(
        dir.path(),
        &[("broken", "sleep 1000"), ("gone", "sleep 1000")],
    );
    let sup = Supervisor::start_every(f.clone(), Duration::from_millis(50));
    wait_running(dir.path(), "broken").await;
    wait_running(dir.path(), "gone").await;
    let pid = running_pid(dir.path(), "broken").unwrap();

    std::fs::write(
        dir.path().join("plugins/broken/plugin.toml"),
        "name = \"broken\"\nrun = [",
    )
    .unwrap();
    let cat = load_catalog(dir.path(), &[]);
    assert!(cat.broken.contains("broken"));
    assert!(!cat.plugins.contains_key("broken"));
    std::fs::remove_dir_all(dir.path().join("plugins/gone")).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while running_pid(dir.path(), "gone").is_some() {
        assert!(
            Instant::now() < deadline,
            "an absent plugin was not stopped"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        running_pid(dir.path(), "broken"),
        Some(pid),
        "a plugin mid-rewrite was stopped as if absent"
    );
    sup.stop().await;
}
