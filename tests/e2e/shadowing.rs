//! A catalog copy of a built-in action: a stale seed is ignored and
//! reported, an operator-edited copy still wins, and `forge workflows
//! refresh` removes the seed (docs/WORKFLOWS.md, "Authoring").

use crate::support::*;
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
