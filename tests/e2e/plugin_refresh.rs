//! `forge plugin list` and doctor say whether an installed plugin still
//! matches the repo copy it was installed from; `forge plugin refresh`
//! brings it up to date without touching config, keeps a backup, restarts
//! the plugin, and never overwrites an operator edit without `--force`.

use crate::support::*;
use std::path::{Path, PathBuf};

const MANIFEST: &str = "name = \"demo\"\nrun = [\"./demo.sh\"]\ncapabilities = [\"events\"]\n";

fn repo_plugin(e: &Env, script: &str) -> PathBuf {
    let dir = e.repo.join("plugins/demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), MANIFEST).unwrap();
    std::fs::write(dir.join("demo.sh"), script).unwrap();
    dir
}

fn list_row(e: &Env) -> serde_json::Value {
    let o = e.forge("ok.sh", &["plugin", "list", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    rows.into_iter().find(|r| r["name"] == "demo").unwrap()
}

fn installed(e: &Env) -> PathBuf {
    e.home.join("plugins/demo")
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn backups(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".bak-"))
        .collect();
    v.sort();
    v
}

#[test]
fn list_and_doctor_show_an_installed_plugin_falling_behind_and_refresh_catches_it_up() {
    let e = Env::new();
    let src = repo_plugin(&e, "#!/bin/sh\necho one\n");
    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(list_row(&e)["sync"], "current");
    let listing = text(&e.forge("ok.sh", &["plugin", "list"]));
    assert!(listing.contains("matches the repo copy"), "{listing}");

    // The operator's own files: config and plugin state.
    std::fs::write(installed(&e).join("config"), "TOKEN=secret\n").unwrap();
    std::fs::create_dir_all(e.home.join("plugins-state/demo")).unwrap();
    std::fs::write(e.home.join("plugins-state/demo/cursor"), "1:2\n").unwrap();

    // The repo moves on: one line becomes two.
    std::fs::write(src.join("demo.sh"), "#!/bin/sh\necho two\necho three\n").unwrap();
    let row = list_row(&e);
    assert_eq!(row["sync"], "behind");
    assert_eq!(row["diff_lines"], 3);
    assert_eq!(
        row["install_source"],
        src.canonicalize().unwrap().to_str().unwrap()
    );
    let listing = text(&e.forge("ok.sh", &["plugin", "list"]));
    assert!(
        listing.contains("behind the repo copy (3 diff line(s))"),
        "{listing}"
    );
    let doctor = text(&e.forge("ok.sh", &["doctor"]));
    assert!(doctor.contains("WARN plugins"), "{doctor}");
    assert!(doctor.contains("demo behind (3 diff line(s))"), "{doctor}");

    let o = e.forge("ok.sh", &["plugin", "refresh", "demo"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        std::fs::read_to_string(installed(&e).join("demo.sh")).unwrap(),
        "#!/bin/sh\necho two\necho three\n"
    );
    let baks = backups(&installed(&e));
    assert_eq!(baks.len(), 1, "{baks:?}");
    assert!(baks[0].starts_with("demo.sh.bak-"), "{baks:?}");
    assert_eq!(
        std::fs::read_to_string(installed(&e).join(&baks[0])).unwrap(),
        "#!/bin/sh\necho one\n"
    );
    assert_eq!(
        std::fs::read_to_string(installed(&e).join("config")).unwrap(),
        "TOKEN=secret\n"
    );
    assert_eq!(
        std::fs::read_to_string(e.home.join("plugins-state/demo/cursor")).unwrap(),
        "1:2\n"
    );
    assert_eq!(list_row(&e)["sync"], "current");
    let doctor = text(&e.forge("ok.sh", &["doctor"]));
    assert!(!doctor.contains("WARN plugins"), "{doctor}");

    let o = e.forge("ok.sh", &["plugin", "refresh", "demo"]);
    assert!(o.status.success());
    assert!(text(&o).contains("already matches"), "{}", text(&o));
    assert_eq!(backups(&installed(&e)).len(), 1, "no second backup");
}

#[test]
fn refresh_reports_an_operator_edit_and_only_overwrites_it_with_force() {
    let e = Env::new();
    let src = repo_plugin(&e, "#!/bin/sh\necho one\n");
    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );
    std::fs::write(installed(&e).join("demo.sh"), "#!/bin/sh\necho mine\n").unwrap();
    assert_eq!(list_row(&e)["sync"], "operator-edit");

    std::fs::write(src.join("demo.sh"), "#!/bin/sh\necho two\n").unwrap();
    let o = e.forge("ok.sh", &["plugin", "refresh", "--all"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("operator edit"), "{}", text(&o));
    assert_eq!(
        std::fs::read_to_string(installed(&e).join("demo.sh")).unwrap(),
        "#!/bin/sh\necho mine\n",
        "never overwritten without --force"
    );
    assert!(backups(&installed(&e)).is_empty());

    let o = e.forge("ok.sh", &["plugin", "refresh", "demo", "--force"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        std::fs::read_to_string(installed(&e).join("demo.sh")).unwrap(),
        "#!/bin/sh\necho two\n"
    );
    let baks = backups(&installed(&e));
    assert_eq!(baks.len(), 1);
    assert_eq!(
        std::fs::read_to_string(installed(&e).join(&baks[0])).unwrap(),
        "#!/bin/sh\necho mine\n",
        "the operator's edit survives as the backup"
    );
    assert_eq!(list_row(&e)["sync"], "current");
}

#[test]
fn refresh_of_an_enabled_plugin_requests_a_restart_and_all_refreshes_every_installed_one() {
    let e = Env::new();
    let src = repo_plugin(&e, "#!/bin/sh\necho one\n");
    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "demo"])
            .status
            .success()
    );
    std::fs::write(src.join("demo.sh"), "#!/bin/sh\necho two\n").unwrap();

    let o = e.forge("ok.sh", &["plugin", "refresh", "--all"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("restart requested"), "{}", text(&o));
    assert_eq!(
        std::fs::read_to_string(e.home.join("plugins-run/demo.restart")).unwrap(),
        "1"
    );
}

#[test]
fn a_copy_installed_before_records_existed_needs_from_and_force() {
    let e = Env::new();
    let src = repo_plugin(&e, "#!/bin/sh\necho new\n");
    let dest = installed(&e);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("plugin.toml"), MANIFEST).unwrap();
    std::fs::write(dest.join("demo.sh"), "#!/bin/sh\necho old\n").unwrap();
    assert_eq!(list_row(&e)["sync"], "unrecorded");

    let o = e.forge("ok.sh", &["plugin", "refresh", "demo"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("--from"), "{}", text(&o));

    let from = src.to_str().unwrap();
    let o = e.forge("ok.sh", &["plugin", "refresh", "demo", "--from", from]);
    assert!(!o.status.success(), "nothing says the copy was unedited");

    let o = e.forge(
        "ok.sh",
        &["plugin", "refresh", "demo", "--from", from, "--force"],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(list_row(&e)["sync"], "current");
}
