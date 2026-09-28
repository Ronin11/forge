//! The per-plugin lock follows the plugin's process group, not the worker.

use crate::plugins::{pid_gone_within, plugin_status_json};
use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// The per-plugin lock follows the plugin's process group, not the worker: a
/// worker killed with SIGKILL whose plugin outlives it (this one ignores the
/// parent-death SIGTERM) leaves the lock held, so the next supervisor does not
/// start a second copy until the group is gone.
#[test]
fn a_successor_does_not_start_a_plugin_whose_orphaned_group_is_still_alive() {
    let e = Env::new();
    let plugin_dir = e.home.join("plugins").join("stubborn");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "name = \"stubborn\"\nrun = [\"./run.sh\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("run.sh"),
        "#!/bin/bash\ntrap '' TERM\necho $$ >> \"$FORGE_PLUGIN_STATE/starts\"\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(
        plugin_dir.join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "stubborn"])
            .status
            .success()
    );
    let starts_file = e.home.join("plugins-state/stubborn/starts");
    let starts = || {
        std::fs::read_to_string(&starts_file)
            .unwrap_or_default()
            .lines()
            .count()
    };

    let first = Worker::spawn(e.cmd("ok.sh").args(["work", "--poll", "1"]));
    assert!(
        wait_until(|| starts() == 1, Duration::from_secs(30)),
        "the first worker never started the plugin"
    );
    let pid = plugin_status_json(&e, "stubborn")["pid"].as_i64().unwrap() as i32;
    unsafe {
        libc::kill(first.id() as i32, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !pid_gone_within(pid, Duration::from_millis(500)),
        "the plugin was expected to outlive its killed worker"
    );

    // Longer than one reconcile tick of the successor.
    let mut second = Worker::spawn(e.cmd("ok.sh").args(["work", "--poll", "1"]));
    std::thread::sleep(Duration::from_secs(13));
    assert_eq!(
        starts(),
        1,
        "the successor started a second copy beside the orphan"
    );

    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    assert!(
        wait_until(|| starts() == 2, Duration::from_secs(30)),
        "the successor never started the plugin once the group was gone"
    );
    second.stop();
    drop(first);
}
