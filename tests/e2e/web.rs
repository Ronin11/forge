use crate::support::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn forge_web_serve_runs_forge_web_from_path_with_the_bind_flag() {
    let e = Env::new();
    let dir = tempfile::tempdir().unwrap();
    // A copy of forge in a directory of its own, so no real forge-web sits beside it.
    let forge = dir.path().join("forge");
    std::fs::copy(env!("CARGO_BIN_EXE_forge"), &forge).unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let out = dir.path().join("args");
    let fake = bin.join("forge-web");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"$@\" > {}\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = std::process::Command::new(&forge)
        .args(["web", "serve", "--bind", "127.0.0.1:9"])
        .env("FORGE_HOME", &e.home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        "--bind 127.0.0.1:9"
    );

    let o = std::process::Command::new(&forge)
        .args(["web", "serve"])
        .env("FORGE_HOME", &e.home)
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("forge-web not found"));
}
