//! Regression coverage for REVIEW-4 §1.18: host settings never seed attempts.
use super::*;

const SECRET: &str = "operator-config-secret-marker";

fn fixture() -> (tempfile::TempDir, Sandbox, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("host-home");
    let worktree = root.path().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    for cli in [".claude", ".codex", ".copilot"] {
        std::fs::create_dir_all(home.join(cli)).unwrap();
    }
    for file in [
        ".claude.json",
        ".claude/settings.json",
        ".codex/config.toml",
    ] {
        std::fs::write(home.join(file), SECRET).unwrap();
    }
    std::fs::write(
        home.join(".copilot/config.json"),
        format!(r#"{{"copilotTokens":{{"github":"login"}},"env":{{"SECRET":"{SECRET}"}}}}"#),
    )
    .unwrap();
    let sb = Sandbox::with_bwrap(PathBuf::from("/usr/bin/bwrap"), home);
    (root, sb, worktree)
}

fn prepare(sb: &Sandbox, worktree: &Path, env: &[(String, String)]) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(sb.prepare(worktree, env, Phase::Agent));
}

fn assert_no_secret(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_secret(&path);
        } else {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains(SECRET), "secret in {}", path.display());
        }
    }
}

#[test]
fn operator_config_secrets_never_enter_private_provider_directories() {
    let (_root, sb, worktree) = fixture();
    for contract in ["code", "review"] {
        let mut env = vec![("FORGE_CONTRACT".into(), contract.into())];
        let private = provider_dir_for(&worktree, contract_of(&env));
        std::fs::create_dir_all(private.join("claude")).unwrap();
        std::fs::create_dir_all(private.join("codex")).unwrap();
        // A task resumed from an older kernel must lose its settings too.
        std::fs::write(private.join("claude/settings.json"), SECRET).unwrap();
        std::os::unix::fs::symlink(
            sb.codex_dir.join("config.toml"),
            private.join("codex/config.toml"),
        )
        .unwrap();
        env.push((
            "FORGE_CODEX_CONFIG".into(),
            "model = \"chosen-model\"\n".into(),
        ));
        prepare(&sb, &worktree, &env);
        assert_no_secret(&private);
        assert!(!private.join("claude/settings.json").exists());
        assert_eq!(
            std::fs::read_to_string(private.join("codex/config.toml")).unwrap(),
            "model = \"chosen-model\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(sb.codex_dir.join("config.toml")).unwrap(),
            SECRET
        );
        assert_eq!(
            std::fs::read_to_string(private.join("copilot/config.json")).unwrap(),
            r#"{"copilotTokens":{"github":"login"}}"#
        );
        // A later launch without a Codex provider cannot retain its config.
        env.pop();
        prepare(&sb, &worktree, &env);
        assert_eq!(
            std::fs::read_to_string(private.join("codex/config.toml")).unwrap(),
            ""
        );
    }
}

#[test]
fn claude_wrapper_writes_only_the_kernel_seed_in_the_private_home() {
    let (root, mut sb, _worktree) = fixture();
    let host = sb.home.clone();
    sb.home = root.path().join("private-home");
    std::fs::create_dir_all(&sb.home).unwrap();
    let status = Command::new("/bin/sh")
        .args([
            "-c",
            &sb.wrapper_script(false, None, Phase::Agent),
            "sh",
            "/bin/true",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert_no_secret(&sb.home);
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.home.join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        serde_json::json!({"hasCompletedOnboarding": true, "theme": "dark"})
    );
    assert_eq!(
        std::fs::read_to_string(host.join(".claude.json")).unwrap(),
        SECRET
    );
}

#[test]
fn operator_config_secrets_are_absent_from_the_sandbox_home() {
    if std::env::var("FORGE_TEST_NO_SANDBOX").as_deref() == Ok("1") {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping real bwrap regression");
        return;
    }
    let (_root, sb, worktree) = fixture();
    prepare(&sb, &worktree, &[]);
    let script = format!(
        "set -eu; test -f \"$HOME/.claude.json\"; \
         test ! -e \"$HOME/.claude/settings.json\"; \
         test -f \"$HOME/.codex/config.toml\"; \
         test -f \"$HOME/.copilot/config.json\"; \
         if /bin/grep -r -F '{SECRET}' \"$HOME\"; then exit 1; \
         else test $? -eq 1; fi"
    );
    let output = sb
        .command(
            &worktree,
            &["/bin/sh".into(), "-c".into(), script],
            &[],
            None,
            Phase::Agent,
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_no_secret(&provider_state_dir(&worktree));
}
