use crate::support::*;

#[test]
fn finished_task_loses_build_caches_but_keeps_its_worktree() {
    let e = Env::new();
    std::fs::write(e.repo.join("forge.toml"), r#"
[checks]
cache = ["sh", "-c", "mkdir -p target node_modules/.cache .godot; echo data > target/built; test -f answer.txt"]
"#).unwrap();
    std::fs::write(
        e.repo.join(".gitignore"),
        "target/\nnode_modules/\n.godot/\n",
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "cache fixture"]);
    let out = e.run("ok.sh", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(e.task(1).0, "succeeded");
    let tree: String = e
        .db()
        .query_row("SELECT worktree FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let tree = std::path::Path::new(&tree);
    assert!(tree.join("answer.txt").exists());
    for cache in ["target", "node_modules/.cache", ".godot"] {
        assert!(!tree.join(cache).exists(), "{cache} survived completion");
    }
}

#[test]
fn shared_target_fixture_reuses_one_writable_directory_across_attempts_and_checks() {
    let e = Env::new();
    std::fs::write(e.repo.join("forge.toml"), r#"
[sandbox]
shared_target = true
[checks]
cache = ["sh", "-c", "test -n \"$CARGO_TARGET_DIR\" && test -f \"$CARGO_TARGET_DIR/agent\" && echo check >> \"$CARGO_TARGET_DIR/check\" && test -f answer.txt"]
"#).unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "shared target fixture"]);
    for _ in 0..2 {
        let out = e.run("shared-target.sh", &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let targets: Vec<_> = std::fs::read_dir(e.home.join("cache"))
        .unwrap()
        .flatten()
        .map(|e| e.path().join("target"))
        .filter(|p| p.exists())
        .collect();
    assert_eq!(targets.len(), 1);
    assert_eq!(
        std::fs::read_to_string(targets[0].join("agent"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        std::fs::read_to_string(targets[0].join("check"))
            .unwrap()
            .lines()
            .count()
            >= 2
    );
    for id in [1, 2] {
        assert_eq!(e.task(id).0, "succeeded");
        assert!(!e.home.join(format!("worktrees/{id}/target")).exists());
    }
}

#[test]
fn low_disk_holds_worker_claims_and_doctor_reports_failure() {
    let e = Env::new();
    let id = e.add(&[]);
    let config = e.home.join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[worker]\nmin_free_gb = 999999999\n");
    std::fs::write(config, text).unwrap();
    let out = e
        .cmd("ok.sh")
        .env_remove("FORGE_MIN_FREE_GB")
        .env_remove("FORGE2_MIN_FREE_GB")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(e.task(id).0, "queued");
    let out = e
        .cmd("ok.sh")
        .env_remove("FORGE_MIN_FREE_GB")
        .env_remove("FORGE2_MIN_FREE_GB")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "disk" && r["status"] == "fail")
    );
    // The process override must apply to both doctor and worker claims,
    // even when the home config requests a larger reserve.
    let out = e.forge("ok.sh", &["doctor", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "disk" && r["status"] == "ok")
    );
    let out = e.forge("ok.sh", &["work", "--once"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(e.task(id).0, "succeeded");
}

#[test]
fn invalid_disk_reserve_override_is_reported() {
    let e = Env::new();
    let out = e
        .cmd("ok.sh")
        .env("FORGE_MIN_FREE_GB", "-1")
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("FORGE_MIN_FREE_GB must be a non-negative integer")
    );
}

#[test]
fn gc_caches_reports_freed_bytes_and_preserves_dirty_sources() {
    let e = Env::new();
    let id = e.add(&[]);
    let tree = e.home.join(format!("worktrees/{id}"));
    std::fs::create_dir_all(tree.join("target")).unwrap();
    std::fs::write(tree.join("target/data"), vec![1; 8192]).unwrap();
    std::fs::write(tree.join("uncommitted.txt"), "keep").unwrap();
    e.db()
        .execute(
            "UPDATE tasks SET worktree=?1 WHERE id=?2",
            rusqlite::params![tree.to_string_lossy(), id],
        )
        .unwrap();
    let out = e.forge("ok.sh", &["gc", "--caches", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("would free"));
    assert!(tree.join("target/data").exists());
    let out = e.forge("ok.sh", &["gc", "--caches"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("freed"));
    assert!(!tree.join("target").exists());
    assert!(tree.join("uncommitted.txt").exists());
    assert_eq!(e.task(id).0, "queued");
}
