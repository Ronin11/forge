//! These tests use only a temporary file store, never the operator's keychain.
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn run(home: &std::path::Path, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
        .env("FORGE_HOME", home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn secret_cli_round_trip_does_not_emit_values_or_create_logs() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("secrets")).unwrap();
    std::fs::write(home.path().join("secrets/config.toml"), "backend = 'file'").unwrap();
    let sentinel = "SENTINEL-secret-cli-never-print";
    let set = run(
        home.path(),
        &["secret", "set", "TEST_KEY"],
        Some(format!("{sentinel}\n").as_bytes()),
    );
    assert!(set.status.success());
    let list = run(home.path(), &["secret", "list"], None);
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("TEST_KEY\t"));
    assert!(String::from_utf8_lossy(&list.stdout).contains("Unix seconds"));
    let rm = run(home.path(), &["secret", "rm", "TEST_KEY"], None);
    assert!(rm.status.success());
    let missing = run(home.path(), &["secret", "rm", "TEST_KEY"], None);
    assert!(!missing.status.success());
    let invalid = run(
        home.path(),
        &["secret", "set", "TEST_KEY"],
        Some(&[0xff, 0xfe]),
    );
    assert!(!invalid.status.success());
    for output in [set, list, rm, missing, invalid] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(sentinel));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(sentinel));
    }
    assert!(
        run(home.path(), &["secret", "list"], None)
            .stdout
            .is_empty()
    );
    assert!(!home.path().join("logs").exists());
}
