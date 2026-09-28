//! A catalog copy of a built-in action: a stale seed is ignored and
//! reported, an operator-edited copy still wins, and `forge workflows
//! refresh` removes the seed (docs/WORKFLOWS.md, "Authoring").

use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const FMT: &str = include_str!("../../src/builtins/operations/fmt.toml");

/// `deploy-command.toml` as an earlier release shipped it (its first
/// version; its blob is listed in src/builtins/history.tsv).
const OLD_DEPLOY_COMMAND: &str = include_str!("../fixtures/old-deploy-command.toml");

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
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    commit_copy(&cat, "deploy-command", OLD_DEPLOY_COMMAND, "Forge");
    let o = e.forge("ok.sh", &["workflows"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("ignoring stale seed actions/deploy-command.toml"),
        "{err}"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("shadowing"), "{out}");
    assert!(out.contains("stale seed"), "{out}");
    assert!(out.contains("forge workflows refresh"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/deploy-command.toml").exists());
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("stale seed"), "{out}");
}

#[test]
fn a_copy_equal_to_an_old_built_in_is_a_seed_whoever_committed_it() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    commit_copy(&cat, "deploy-command", OLD_DEPLOY_COMMAND, "operator");
    let o = e.forge("ok.sh", &["workflows"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("ignoring stale seed"), "{err}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/deploy-command.toml").exists());
}

#[test]
fn an_uncommitted_edit_survives_refresh_and_is_reported_as_an_edit() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    let edited = FMT.replacen("description = \"", "description = \"EDITED: ", 1);
    std::fs::write(cat.join("actions/fmt.toml"), &edited).unwrap();
    let o = e.forge("ok.sh", &["workflows"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("stale seed"), "{err}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("fmt.toml is an operator edit"), "{out}");
    assert_eq!(
        std::fs::read_to_string(cat.join("actions/fmt.toml")).unwrap(),
        edited
    );
    // Not even an explicit --take-builtin deletes what git has never seen.
    let o = e.forge("ok.sh", &["workflows", "refresh", "--take-builtin"]);
    assert!(!o.status.success());
    assert_eq!(
        std::fs::read_to_string(cat.join("actions/fmt.toml")).unwrap(),
        edited
    );
}

#[test]
fn forge_init_leaves_an_uncommitted_catalog_edit_uncommitted() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    let edited = FMT.replacen("description = \"", "description = \"EDITED: ", 1);
    std::fs::write(cat.join("actions/fmt.toml"), &edited).unwrap();
    let o = e
        .cmd("ok.sh")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .arg("init")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let status = git(&cat, &["status", "--porcelain", "-uall"]);
    assert!(status.contains("?? actions/fmt.toml"), "{status}");
    assert_eq!(
        std::fs::read_to_string(cat.join("actions/fmt.toml")).unwrap(),
        edited
    );
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
    assert!(
        !out.contains("fmt.toml"),
        "no diff lines, not listed: {out}"
    );
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/fmt.toml").exists());
}

/// Commit `text` as `actions/{name}.toml` by `who`.
fn commit_copy(cat: &std::path::Path, name: &str, text: &str, who: &str) {
    let rel = format!("actions/{name}.toml");
    std::fs::write(cat.join(&rel), text).unwrap();
    git(cat, &["add", &rel]);
    git(
        cat,
        &[
            "-c",
            &format!("user.name={who}"),
            "-c",
            "user.email=x@localhost",
            "commit",
            "-qm",
            "tune",
        ],
    );
}

#[test]
fn an_operator_committed_copy_that_does_not_differ_is_removed_without_a_flag() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    commit_copy(&cat, "fmt", FMT, "operator");
    let assess = include_str!("../../src/builtins/actions/assess.toml");
    let spaced = format!("# my note\n\n{assess}\n\n");
    commit_copy(&cat, "assess", &spaced, "operator");
    assert!(cat.join("actions/assess.toml").exists());
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("operator edit"), "{out}");
    assert!(!out.contains("0 diff line(s)"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!cat.join("actions/fmt.toml").exists());
    assert!(!cat.join("actions/assess.toml").exists());
}

#[test]
fn refresh_flags_take_action_names_to_decide_one_copy_at_a_time() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    let edit = |text: &str| text.replacen("description = \"", "description = \"EDITED: ", 1);
    let deploy = include_str!("../../src/builtins/operations/deploy-self.toml");
    commit_copy(&cat, "fmt", &edit(FMT), "operator");
    commit_copy(&cat, "deploy-self", &edit(deploy), "operator");
    let path = |n: &str| cat.join(format!("actions/{n}.toml"));

    // Neither named: both refused, both kept.
    assert!(!e.forge("ok.sh", &["workflows", "refresh"]).status.success());
    // Naming one drops only that one; the other is still undecided.
    let o = e.forge(
        "ok.sh",
        &["workflows", "refresh", "--take-builtin", "deploy-self"],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "fmt is still undecided");
    assert!(err.contains("fmt.toml"), "{err}");
    assert!(!err.contains("deploy-self.toml"), "{err}");
    assert!(!path("deploy-self").exists());
    assert!(path("fmt").exists());
    // A name that is not a shadowing copy is refused and removes nothing.
    let o = e.forge("ok.sh", &["workflows", "refresh", "--take-builtin", "nope"]);
    assert!(!o.status.success());
    assert!(path("fmt").exists());
    // Keeping one by name settles it.
    let o = e.forge("ok.sh", &["workflows", "refresh", "--keep", "fmt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(path("fmt").exists());
    // A bare flag still means every copy, and `.toml` is accepted.
    let o = e.forge(
        "ok.sh",
        &["workflows", "refresh", "--take-builtin", "fmt.toml"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!path("fmt").exists());
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
    let edited = FMT.replacen("description = \"", "description = \"EDITED: ", 1);
    std::fs::write(cat.join("actions/fmt.toml"), edited).unwrap();

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

#[test]
fn a_hash_line_added_inside_a_string_is_an_operator_edit_refresh_keeps() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    let hetzner = include_str!("../../src/builtins/operations/provision-hetzner.toml");
    let edited = hetzner.replacen(
        "\n    User root\n",
        "\n    User root\n#StrictHostKeyChecking no\n",
        1,
    );
    assert_ne!(edited, hetzner);
    commit_copy(&cat, "provision-hetzner", &edited, "operator");
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let row = out.lines().find(|l| l.contains("shadowing")).unwrap_or("");
    assert!(row.contains("provision-hetzner"), "{out}");
    assert!(row.contains("operator edit"), "{out}");
    let o = e.forge("ok.sh", &["workflows", "refresh"]);
    assert!(!o.status.success());
    assert!(cat.join("actions/provision-hetzner.toml").exists());
}

fn shadowing_row(e: &Env) -> serde_json::Value {
    let o = e.forge("ok.sh", &["doctor", "--json", "--only", "shadowing"]);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    rows.as_array()
        .and_then(|a| a.iter().find(|r| r["name"] == "shadowing"))
        .cloned()
        .unwrap_or_else(|| panic!("no shadowing row in {rows}"))
}

/// The table behind the Warn is compiled from the checked-in
/// `src/builtins/history.tsv`, not read from `git log` at build time, so a
/// release built from a `git archive` tree (no `.git`) warns too.
#[test]
fn an_operator_edit_of_a_built_in_that_changed_since_is_a_warning_not_ok() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let cat = e.home.join("workflows");
    let edited = FMT.replacen("description = \"", "description = \"EDITED: ", 1);
    std::fs::write(cat.join("actions/fmt.toml"), edited).unwrap();
    git(&cat, &["add", "actions/fmt.toml"]);
    let committed = std::process::Command::new("git")
        .arg("-C")
        .arg(&cat)
        .args([
            "-c",
            "user.name=operator",
            "-c",
            "user.email=x@localhost",
            "commit",
            "-qm",
            "tune",
        ])
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
        .status()
        .unwrap();
    assert!(committed.success());
    let row = shadowing_row(&e);
    assert_eq!(row["status"], "warn", "{row}");
    assert!(
        row["detail"]
            .as_str()
            .is_some_and(|d| d.contains("the built-in changed after this copy's last commit")),
        "{row}"
    );
}
