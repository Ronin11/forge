//! `build.rs` must get the built-ins' history from the checked-in
//! `src/builtins/history.tsv`, not from `git log`: `forge deploy` builds a
//! release from a `git archive` tree that has no `.git`, and a binary with
//! an empty table can never tell that an operator's copy of a built-in is
//! out of date. This runs the build script exactly as such a build does:
//! in a directory with the table and no `.git`.

use std::path::Path;
use std::process::Command;

#[test]
fn a_build_script_run_without_git_history_still_writes_the_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("src/builtins")).unwrap();
    std::fs::copy(
        root.join("src/builtins/history.tsv"),
        tree.join("src/builtins/history.tsv"),
    )
    .unwrap();
    let bin = dir.path().join("build-script");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let built = Command::new(rustc)
        .args(["--edition", "2021", "--crate-name", "build_script", "-o"])
        .arg(&bin)
        .arg(root.join("build.rs"))
        .status()
        .unwrap();
    assert!(built.success());
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let ran = Command::new(&bin)
        .current_dir(&tree)
        .env("OUT_DIR", &out)
        .env("GIT_DIR", dir.path().join("no-such-git-dir"))
        .env_remove("FORGE_BUILD_SHA")
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(!tree.join(".git").exists());
    let table = std::fs::read_to_string(out.join("builtin_history.rs")).unwrap();
    assert!(table.contains("\"fmt.toml\""), "{table}");
    assert!(table.contains("\"deploy-self.toml\""), "{table}");
}
