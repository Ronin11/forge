//! A catalog copy of a built-in action: a stale seed is ignored and
//! reported, an operator-edited copy still wins, and `forge workflows
//! refresh` removes the seed (docs/WORKFLOWS.md, "Authoring").

use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const FMT: &str = include_str!("../../src/builtins/operations/fmt.toml");

/// Write an edited `actions/fmt.toml` into the catalog and commit it as `who`.
fn shadow_fmt(e: &Env, who: &str, message: &str) -> PathBuf {
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    assert!(!cat.join("actions/fmt.toml").exists(), "never seeded");
    let edited = FMT.replacen("description = \"", "description = \"EDITED: ", 1);
    std::fs::write(cat.join("actions/fmt.toml"), edited).unwrap();
    git(&cat, &["add", "actions/fmt.toml"]);
    git(
        &cat,
        &[
            "-c",
            &format!("user.name={who}"),
            "-c",
            "user.email=x@localhost",
            "commit",
            "-qm",
            message,
        ],
    );
    cat
}

#[test]
fn init_never_seeds_a_built_in_action_into_the_catalog() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let n = std::fs::read_dir(e.home.join("workflows/actions")).map_or(0, |d| d.count());
    assert_eq!(n, 0, "the catalog holds only the operator's own actions");
}

#[test]
fn a_stale_seed_is_ignored_reported_and_removed_by_refresh() {
    let e = Env::new();
    let cat = shadow_fmt(&e, "Forge", "catalog: built-in fmt");
    let o = e.forge("ok.sh", &["workflows"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("ignoring stale seed actions/fmt.toml"),
        "{err}"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("shadowing"), "{out}");
    assert!(out.contains("stale seed"), "{out}");
    assert!(out.contains("forge workflows refresh"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/fmt.toml").exists());
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("stale seed"), "{out}");
}

#[test]
fn an_operator_edited_copy_still_wins_and_refresh_refuses_without_a_flag() {
    let e = Env::new();
    let cat = shadow_fmt(&e, "operator", "tune fmt");
    let o = e.forge("ok.sh", &["workflows"]);
    assert!(
        !String::from_utf8_lossy(&o.stderr).contains("stale seed"),
        "an operator edit is not ignored"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("operator edit"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("+description = \"EDITED: "), "{out}");
    assert!(cat.join("actions/fmt.toml").exists());
    let o = e.forge("ok.sh", &["workflows", "refresh", "--keep"]);
    assert!(o.status.success());
    assert!(cat.join("actions/fmt.toml").exists());
    let o = e.forge("ok.sh", &["workflows", "refresh", "--take-builtin"]);
    assert!(o.status.success());
    assert!(!cat.join("actions/fmt.toml").exists());
}

#[test]
fn an_identical_seed_is_a_stale_seed_and_refresh_deletes_it() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    std::fs::write(cat.join("actions/fmt.toml"), FMT).unwrap();
    git(&cat, &["add", "actions/fmt.toml"]);
    git(
        &cat,
        &[
            "-c",
            "user.name=forge",
            "-c",
            "user.email=forge@localhost",
            "commit",
            "-qm",
            "catalog: built-in fmt",
        ],
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("fmt.toml (stale seed"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/fmt.toml").exists());
}

/// A `git diff --no-index` that dies (here, of a signal — the same shape of
/// failure, killed rather than exiting, that motivated `diff_of`'s
/// unique-per-call temp file after a real `git` died of `SIGBUS`) must make
/// the doctor's shadowing row state the failure, not `0 diff line(s)` —
/// indistinguishable from "no differences", the exact silent symptom this
/// guards against. The fake signals itself with `SIGTERM`, not `SIGBUS`:
/// both are reported by `diff_of` as "killed by signal", but `SIGBUS` is a
/// core-dump signal, and a passing suite should leave no core dumps behind.
#[test]
fn a_git_diff_that_dies_reports_the_failure_not_zero_diff_lines() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    std::fs::write(cat.join("actions/fmt.toml"), FMT).unwrap();

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let git_path = fakebin.join("git");
    std::fs::write(
        &git_path,
        "#!/bin/bash\nif [ \"$1\" = diff ] && [ \"$2\" = --no-index ]; then\n  kill -TERM $$\nfi\nexec /usr/bin/git \"$@\"\n",
    )
    .unwrap();
    let mut perm = std::fs::metadata(&git_path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&git_path, perm).unwrap();
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .args(["doctor"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let row = out.lines().find(|l| l.contains("shadowing")).unwrap_or("");
    assert!(row.contains("fmt.toml"), "{out}");
    assert!(row.contains("diff failed"), "{out}");
    assert!(!row.contains("0 diff line(s)"), "{out}");

    // Real git again: the failure must not have been cached into a stuck
    // placeholder that a later, healthy `git diff` can no longer replace.
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let row = out.lines().find(|l| l.contains("shadowing")).unwrap_or("");
    assert!(row.contains("diff line(s)"), "{out}");
    assert!(!row.contains("diff failed"), "{out}");
}
