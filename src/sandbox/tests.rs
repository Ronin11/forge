use super::*;

/// `Sandbox::prepare`, as every launch awaits it before `command`.
fn prepared(sandbox: &Sandbox, worktree: &Path, env: &[(String, String)]) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(sandbox.prepare(worktree, env, Phase::Agent));
}

const CREDS: &str =
    r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":32503680000000}}"#;

const CODEX_AUTH: &str = r#"{"OPENAI_API_KEY":"sk-proj-abc","tokens":null}"#;

#[test]
fn review_provider_state_is_separate_and_discarded_with_the_coders() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("task");
    let coder = provider_dir_for(&worktree, Some(Contract::Code));
    let review = provider_dir_for(&worktree, Some(Contract::Review));
    assert_eq!(coder, root.path().join("task-provider"));
    assert_eq!(review, root.path().join("task-review-provider"));
    let mut sandbox = test_sandbox("api.anthropic.com");
    sandbox.config_dir = root.path().join("host-claude");
    std::fs::create_dir_all(&sandbox.config_dir).unwrap();
    std::fs::write(sandbox.config_dir.join("settings.json"), "settings").unwrap();
    let policy = Policy::new([]);
    prepared(&sandbox, &worktree, &[]);
    let _ = sandbox.command(&worktree, &[], &[], &policy, Phase::Agent);
    std::fs::create_dir_all(coder.join("claude/projects")).unwrap();
    std::fs::write(coder.join("claude/projects/session"), "coder transcript").unwrap();
    let env = vec![("FORGE_CONTRACT".into(), "review".into())];
    prepared(&sandbox, &worktree, &env);
    let cmd = sandbox.command(&worktree, &[], &env, &policy, Phase::Agent);
    assert!(args_of(&cmd).contains(&review.join("claude").display().to_string()));
    assert!(!args_of(&cmd).contains(&coder.join("claude").display().to_string()));
    assert!(!review.join("claude/projects").exists());
    assert!(!review.join("claude/settings.json").exists());
    discard_provider_state(&worktree);
    assert!(!coder.exists());
    assert!(!review.exists());
}

/// docs/REVIEW-4.md #1.9: a copy holding the only live refresh token is
/// kept, not discarded, when the host directory refuses the write-back
/// that would have made the host file catch up first. `_in` takes the
/// state directory and each shape's host directory as arguments instead
/// of reading the ambient environment, so the read-only host directory
/// is a temp directory of this test's own and no `std::env::set_var`
/// races the rest of the test binary (see
/// `git::tests::identity_falls_back_to_the_constant_when_unset`).
#[tokio::test]
async fn discard_provider_state_keeps_the_copy_when_its_write_back_fails() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-claude");
    std::fs::create_dir_all(&host).unwrap();
    let state = root.path().join("forge-home");
    let worktree = root.path().join("work/task");
    let provider = root.path().join("work/task-provider");
    let private = provider.join("claude").join(crate::login::CLAUDE.file);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    let far = crate::unix_now() * 1000 + 8 * 3600 * 1000;
    let login = |a: &str, r: &str, at: i64| {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-{a:x<32}","refreshToken":"sk-ant-ort01-{r:x<32}","expiresAt":{at}}}}}"#
        )
    };
    std::fs::write(host.join(crate::login::CLAUDE.file), login("a0", "r0", far)).unwrap();
    crate::login::CLAUDE
        .seed(&host, &state, &worktree, &private)
        .await;
    // The sandbox refreshed: the private copy is later than the host's.
    std::fs::write(&private, login("a1", "r1", far + 1000)).unwrap();
    // Nothing could write to the host directory: read-only, a full
    // disk, ownership by another user.
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o500)).unwrap();
    discard_provider_state_in(&worktree, Some(&state), |_| Some(host.clone()));
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(private.exists(), "the only live refresh token is kept");
    assert!(provider.exists());
    assert!(
        std::fs::read_to_string(host.join(crate::login::CLAUDE.file))
            .unwrap()
            .contains("r0"),
        "the host file was never written"
    );
}

#[test]
fn command_binds_tmpfs_home_before_ro_dirs_before_the_worktree() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("work/tree");
    std::fs::create_dir_all(&worktree).unwrap();
    let config_dir = root.path().join("real/.claude");
    let codex_dir = root.path().join("real/.codex");
    let copilot_dir = root.path().join("real/.copilot");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::create_dir_all(&copilot_dir).unwrap();
    std::fs::write(
        copilot_dir.join("config.json"),
        r#"{"copilotTokens":{"github":"login"}}"#,
    )
    .unwrap();
    std::fs::write(config_dir.join(".credentials.json"), CREDS).unwrap();
    std::fs::write(config_dir.join("settings.json"), "settings").unwrap();
    std::fs::write(codex_dir.join("auth.json"), CODEX_AUTH).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "cfg").unwrap();
    let npm_cache = root.path().join("opt/npm-cache");
    std::fs::create_dir_all(&npm_cache).unwrap();
    let repo_cache = root.path().join("forge-home/cache/abc123");

    let sandbox = Sandbox {
        bwrap: PathBuf::from("/usr/bin/bwrap"),
        home: PathBuf::from("/home/attempt"),
        agent_dirs: vec![PathBuf::from("/opt/agent")],
        config_dir: config_dir.clone(),
        forge_home: root.path().join("forge-home"),
        codex_dir: codex_dir.clone(),
        copilot_dir: copilot_dir.clone(),
        extra_ro: vec![PathBuf::from("/opt/toolchain")],
        extra_rw: vec![npm_cache.clone()],
        overlay: true,
        dependency_cache: None,
        relay_exe: PathBuf::from("/opt/forge/forge"),
        proxies: Arc::new(Proxies::default()),
        declared: Mutex::new(BTreeMap::new()),
        provider_hosts: Mutex::new(BTreeMap::new()),
        caches: Mutex::new(BTreeMap::new()),
        granted: Mutex::new(BTreeMap::new()),
    };
    sandbox.set_provider_hosts(&worktree, &[Rule::parse("api.example.com").unwrap()]);
    sandbox.set_cache_dir(&worktree, repo_cache.clone());
    prepared(&sandbox, &worktree, &[]);
    let cmd = sandbox.command_for_worktree(&worktree, &["true".to_string()], &[]);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let pos = |flag: &str, value: &str| {
        args.windows(2)
            .position(|w| w[0] == flag && w[1] == value)
            .unwrap_or_else(|| panic!("missing `{flag} {value}` in {args:?}"))
    };

    let tmpfs_home = pos("--tmpfs", "/home/attempt");
    let ro_agent = pos("--ro-bind-try", "/opt/agent");
    let ro_extra = pos("--ro-bind-try", "/opt/toolchain");
    let worktree_bind = pos("--bind", worktree.to_str().unwrap());
    let overlay_src = pos("--overlay-src", npm_cache.to_str().unwrap());
    let tmp_overlay = pos("--tmp-overlay", npm_cache.to_str().unwrap());
    let cache_bind = pos("--bind-try", repo_cache.to_str().unwrap());

    assert!(tmpfs_home < ro_agent, "tmpfs $HOME must precede ro binds");
    assert!(tmpfs_home < ro_extra, "tmpfs $HOME must precede ro binds");
    assert!(
        ro_agent < worktree_bind,
        "agent ro bind must precede the worktree bind"
    );
    assert!(
        ro_extra < worktree_bind,
        "extra ro binds must precede the worktree bind"
    );
    assert!(
        worktree_bind < overlay_src,
        "worktree bind must precede the package cache overlay"
    );
    assert!(
        worktree_bind < tmp_overlay,
        "worktree bind must precede the package cache overlay"
    );
    assert!(
        worktree_bind < cache_bind,
        "worktree bind must precede the repository cache bind"
    );

    // A private, seeded copy of the claude and codex state is bound
    // writable at the paths the CLIs expect; the operator's real
    // directories are never a bind source.
    let provider_dir = provider_state_dir(&worktree);
    let claude_priv = provider_dir.join("claude");
    let codex_priv = provider_dir.join("codex");
    let provider_bind = |src: &Path, dest: &Path| {
        args.windows(3)
            .position(|w| {
                w[0] == "--bind" && w[1] == src.to_str().unwrap() && w[2] == dest.to_str().unwrap()
            })
            .unwrap_or_else(|| {
                panic!("missing private provider bind {src:?} -> {dest:?}: {args:?}")
            })
    };
    let copilot_priv = provider_dir.join("copilot");
    let claude_bind = provider_bind(&claude_priv, &config_dir);
    let codex_bind = provider_bind(&codex_priv, &codex_dir);
    let copilot_bind = provider_bind(&copilot_priv, &copilot_dir);
    assert!(worktree_bind < claude_bind && worktree_bind < codex_bind);
    assert!(worktree_bind < copilot_bind);
    assert!(
        !args.windows(3).any(|w| matches!(
            w[0].as_str(),
            "--bind" | "--ro-bind" | "--bind-try" | "--ro-bind-try"
        ) && (w[1] == config_dir.to_str().unwrap()
            || w[1] == codex_dir.to_str().unwrap()
            || w[1] == copilot_dir.to_str().unwrap())),
        "the operator's real claude/codex/copilot directories must never be a bind source: {args:?}"
    );
    assert_eq!(
        std::fs::read_to_string(copilot_priv.join("config.json")).unwrap(),
        r#"{"copilotTokens":{"github":"login"}}"#
    );
    assert_eq!(
        std::fs::read_to_string(claude_priv.join(".credentials.json")).unwrap(),
        CREDS
    );
    assert!(!claude_priv.join("settings.json").exists());
    assert_eq!(
        std::fs::read_to_string(codex_priv.join("auth.json")).unwrap(),
        CODEX_AUTH
    );
    assert_eq!(
        std::fs::read_to_string(codex_priv.join("config.toml")).unwrap(),
        ""
    );

    assert!(!args.iter().any(|a| a == "/home/attempt/.claude.json"));
    assert!(!args.iter().any(|a| a == "/home/real/.claude.json"));

    // The sandboxed command is a seed-then-exec wrapper around the real
    // argv, not the real argv directly: `true` must not appear as argv[0].
    let dash_dash = args
        .iter()
        .position(|a| a == "--")
        .expect("-- separates bwrap flags from the sandboxed command");
    let tail = &args[dash_dash + 1..];
    assert_eq!(tail[0], "/bin/sh");
    assert_eq!(tail[1], "-c");
    assert!(
        tail[2].contains(CLAUDE_JSON_SEED),
        "wrapper must write the kernel-built seed: {}",
        tail[2]
    );
    assert!(
        tail[2].contains("/home/attempt/.claude.json"),
        "wrapper must write to $HOME/.claude.json: {}",
        tail[2]
    );
    assert!(
        tail[2].contains("exec \"$@\""),
        "wrapper must exec the real argv after copying: {}",
        tail[2]
    );
    assert_eq!(tail[3], "sh", "argv[0] for the wrapper script is $0");
    assert_eq!(&tail[4..], &["true"], "the real argv follows the wrapper");

    discard_provider_state(&worktree);
    assert!(!provider_dir.exists(), "provider state must be discarded");
}

#[test]
fn bwrap_versions_parse_and_gate_overlay() {
    let v = |a, b, c| {
        Some(BwrapVersion {
            major: a,
            minor: b,
            patch: c,
            pre: None,
        })
    };
    let pre = |a, b, c, p: &str| {
        Some(BwrapVersion {
            major: a,
            minor: b,
            patch: c,
            pre: Some(p.to_string()),
        })
    };
    assert_eq!(parse_bwrap_version("bubblewrap 0.9.0\n"), v(0, 9, 0));
    assert_eq!(parse_bwrap_version("bubblewrap 0.10.1"), v(0, 10, 1));
    assert_eq!(parse_bwrap_version("bubblewrap 0.11"), v(0, 11, 0));
    assert_eq!(parse_bwrap_version("no version"), None);
    let rc = parse_bwrap_version("bubblewrap 0.10.0-rc.1");
    assert_eq!(rc, pre(0, 10, 0, "rc.1"));
    assert_eq!(rc.clone().unwrap().to_string(), "0.10.0-rc.1");
    assert!(!version_has_overlay(rc));
    assert!(version_has_overlay(v(0, 10, 0)));
    assert!(version_has_overlay(pre(0, 10, 1, "rc.1")));
    assert!(!version_has_overlay(v(0, 9, 0)));
    assert!(version_has_overlay(v(1, 0, 0)));
    assert!(!version_has_overlay(None));
}

#[test]
fn a_fake_bwrap_version_is_probed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("bwrap");
    std::fs::write(&fake, "#!/bin/sh\necho 'bubblewrap 0.9.0'\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(bwrap_version(&fake).unwrap().to_string(), "0.9.0");
}

#[test]
fn without_overlay_support_caches_are_not_bound() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("npm");
    std::fs::create_dir_all(&cache).unwrap();
    let worktree = root.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut sb = test_sandbox("api.example.com");
    sb.extra_rw = vec![cache.clone()];
    sb.overlay = false;
    let args: Vec<String> = sb
        .command_for_worktree(&worktree, &["true".to_string()], &[])
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(!args.iter().any(|a| a.contains("overlay")));
    assert!(!args.iter().any(|a| a == cache.to_str().unwrap()));
    sb.overlay = true;
    let args: Vec<String> = sb
        .command_for_worktree(&worktree, &["true".to_string()], &[])
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(args.iter().any(|a| a == "--overlay-src"));
}

/// `model` is the one host `/work/1` (the worktree most of these tests
/// use) resolves to via `set_provider_hosts`; a worktree this helper's
/// caller never names gets none.
fn test_sandbox(model: &str) -> Sandbox {
    let sb = Sandbox {
        bwrap: PathBuf::from("/usr/bin/bwrap"),
        home: PathBuf::from("/home/attempt"),
        agent_dirs: vec![],
        config_dir: PathBuf::from("/home/attempt/.claude"),
        forge_home: PathBuf::from("/home/attempt/forge-home"),
        codex_dir: PathBuf::from("/home/attempt/.codex"),
        copilot_dir: PathBuf::from("/home/attempt/.copilot"),
        extra_ro: vec![],
        extra_rw: vec![],
        overlay: true,
        dependency_cache: None,
        relay_exe: PathBuf::from("/opt/forge/forge"),
        proxies: Arc::new(Proxies::default()),
        declared: Mutex::new(BTreeMap::new()),
        provider_hosts: Mutex::new(BTreeMap::new()),
        caches: Mutex::new(BTreeMap::new()),
        granted: Mutex::new(BTreeMap::new()),
    };
    sb.set_provider_hosts(Path::new("/work/1"), &[Rule::parse(model).unwrap()]);
    sb
}

#[tokio::test]
async fn a_missing_proxy_socket_error_names_the_socket() {
    let sb = test_sandbox("api.example.com");
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path();
    let socket = sb.proxies.socket_for(&sb.policy_for(worktree)).unwrap();
    sb.check_socket(worktree).unwrap();
    std::fs::remove_file(&socket).unwrap();
    let error = sb.check_socket(worktree).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("egress proxy socket {} is missing", socket.display())
    );
}

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn provider_dirs_read_their_environment_the_way_claude_config_dir_is_read() {
    let home = Path::new("/home/u");
    let env = |set: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            set.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    };
    let (c, x, p) = provider_dirs(home, env(&[]));
    assert_eq!(
        (c, x, p),
        (
            PathBuf::from("/home/u/.claude"),
            PathBuf::from("/home/u/.codex"),
            PathBuf::from("/home/u/.copilot")
        )
    );
    let (c, x, p) = provider_dirs(
        home,
        env(&[
            ("CLAUDE_CONFIG_DIR", "/srv/claude"),
            ("CODEX_HOME", "/srv/codex"),
            ("COPILOT_HOME", ""),
        ]),
    );
    assert_eq!(c, Path::new("/srv/claude"));
    assert_eq!(x, Path::new("/srv/codex"));
    assert_eq!(p, Path::new("/home/u/.copilot"));
}

#[test]
fn codex_home_is_where_the_private_codex_copy_is_bound() {
    let mut sb = test_sandbox("api.example.com");
    sb.codex_dir = PathBuf::from("/srv/codex-home");
    let root = tempfile::tempdir().unwrap();
    let args = args_of(&sb.command_for_worktree(root.path(), &["true".to_string()], &[]));
    assert!(
        args.windows(3)
            .any(|w| w[0] == "--bind" && w[1].ends_with("/codex") && w[2] == "/srv/codex-home"),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "/home/attempt/.codex"),
        "{args:?}"
    );
}

#[test]
fn an_agent_under_the_claude_dir_is_bound_after_the_private_binds() {
    let mut sb = test_sandbox("api.example.com");
    let local = PathBuf::from("/home/attempt/.claude/local");
    sb.agent_dirs = vec![local.clone(), PathBuf::from("/opt/agent")];
    sb.extra_ro = vec![PathBuf::from("/home/attempt/.codex/tools")];
    let root = tempfile::tempdir().unwrap();
    let args = args_of(&sb.command_for_worktree(root.path(), &["true".to_string()], &[]));
    let at = |flag: &str, value: &str| {
        args.iter()
            .enumerate()
            .filter(|(i, a)| *a == flag && args[i + 1] == value)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let dest = |d: &str| {
        args.windows(3)
            .position(|w| w[0] == "--bind" && w[2] == d)
            .unwrap_or_else(|| panic!("no private bind at {d}: {args:?}"))
    };
    let claude_bind = dest("/home/attempt/.claude");
    let codex_bind = dest("/home/attempt/.codex");
    let local_binds = at("--ro-bind-try", local.to_str().unwrap());
    assert_eq!(local_binds.len(), 1, "{args:?}");
    assert!(claude_bind < local_binds[0], "{args:?}");
    let tools = at("--ro-bind-try", "/home/attempt/.codex/tools");
    assert_eq!(tools.len(), 1, "{args:?}");
    assert!(codex_bind < tools[0], "{args:?}");
    // An entry outside every provider directory keeps its early place.
    let agent = at("--ro-bind-try", "/opt/agent");
    assert_eq!(agent.len(), 1);
    assert!(agent[0] < claude_bind, "{args:?}");
}

#[test]
fn an_entry_that_is_a_provider_dir_is_refused_by_name() {
    let claude = PathBuf::from("/home/u/.claude");
    let codex = PathBuf::from("/home/u/.codex");
    let copilot = PathBuf::from("/home/u/.copilot");
    let ok = [PathBuf::from("/home/u/.claude/local")];
    refuse_provider_dir_binds(ok.iter(), [&claude, &codex, &copilot]).unwrap();
    let bad = [PathBuf::from("/opt"), codex.clone()];
    let e = refuse_provider_dir_binds(bad.iter(), [&claude, &codex, &copilot]).unwrap_err();
    assert!(e.to_string().contains("/home/u/.codex"), "{e}");
}

#[test]
fn a_worktrees_policy_is_the_model_endpoint_plus_what_its_repository_declared() {
    let sb = test_sandbox("api.example.com");
    let names = |p: &Policy| p.rules().iter().map(|r| r.to_string()).collect::<Vec<_>>();
    let wt = PathBuf::from("/work/1");
    assert_eq!(names(&sb.policy_for(&wt)), ["api.example.com"]);
    sb.set_egress(&wt, &[Rule::parse("registry.npmjs.org").unwrap()]);
    assert_eq!(
        names(&sb.policy_for(&wt)),
        ["api.example.com", "registry.npmjs.org"]
    );
    // A directory below the worktree shares its policy; a sibling does
    // not, and gets no model endpoint at all unless its own provider
    // resolved one for it.
    assert_eq!(names(&sb.policy_for(&wt.join("scratch"))).len(), 2);
    assert!(names(&sb.policy_for(Path::new("/work/2"))).is_empty());
}

#[test]
fn a_claude_worktrees_policy_has_no_openai_host_when_a_codex_provider_exists() {
    let mut providers = BTreeMap::new();
    providers.insert("anthropic".to_string(), crate::agent::Provider::default());
    providers.insert(
        "codex".to_string(),
        crate::agent::Provider {
            runner: crate::agent::Runner::CodexCli,
            ..crate::agent::Provider::default()
        },
    );
    let sb = test_sandbox("api.example.com");
    let wt = PathBuf::from("/work/claude");
    let claude = providers.get("anthropic").unwrap();
    sb.set_provider_hosts(&wt, &egress::provider_rules(claude));
    let names: Vec<String> = sb
        .policy_for(&wt)
        .rules()
        .iter()
        .map(|r| r.to_string())
        .collect();
    assert!(names.iter().any(|h| h.contains("anthropic")), "{names:?}");
    assert!(!names.iter().any(|h| h.contains("openai")), "{names:?}");
}

#[test]
fn a_chat_providers_url_is_in_no_sandbox_policy() {
    let mut providers = BTreeMap::new();
    providers.insert("anthropic".to_string(), crate::agent::Provider::default());
    providers.insert(
        "chat".to_string(),
        crate::agent::Provider {
            runner: crate::agent::Runner::Chat,
            base_url: Some("http://chat.lan:8080/v1".into()),
            ..crate::agent::Provider::default()
        },
    );
    let sb = test_sandbox("api.example.com");
    for (name, wt) in [("anthropic", "/work/claude"), ("chat", "/work/chat")] {
        let p = providers.get(name).unwrap();
        sb.set_provider_hosts(Path::new(wt), &egress::provider_rules(p));
    }
    for wt in ["/work/claude", "/work/chat", "/work/1"] {
        let names: Vec<String> = sb
            .policy_for(Path::new(wt))
            .rules()
            .iter()
            .map(|r| r.to_string())
            .collect();
        assert!(!names.iter().any(|h| h.contains("chat.lan")), "{names:?}");
    }
}

#[tokio::test]
async fn the_command_has_a_namespace_of_its_own_and_one_route_out() {
    let sb = test_sandbox("api.example.com");
    let cmd = sb.command_for_worktree(Path::new("/work/1"), &["true".to_string()], &[]);
    let args = args_of(&cmd);
    assert!(args.iter().any(|a| a == "--unshare-net"), "{args:?}");
    let bind = args
        .windows(3)
        .find(|w| w[0] == "--bind" && w[2] == "/run/forge/egress.sock")
        .unwrap_or_else(|| panic!("the proxy socket is bound in: {args:?}"));
    assert!(
        Path::new(&bind[1]).exists(),
        "the socket exists on the host"
    );
    let script = &args[args.iter().position(|a| a == "--").unwrap() + 3];
    assert!(
        script.contains("'/opt/forge/forge' egress-relay"),
        "{script}"
    );
    assert!(
        script.find("egress-relay").unwrap() < script.find("exec \"$@\"").unwrap(),
        "the relay starts before the command: {script}"
    );
    let env = |k: &str| {
        cmd.get_envs()
            .find(|(n, _)| *n == k)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    };
    for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        assert_eq!(env(k).as_deref(), Some("http://127.0.0.1:3128"), "{k}");
    }
    assert_eq!(env("NO_PROXY").as_deref(), Some("localhost,127.0.0.1,::1"));
}

#[test]
fn without_a_runtime_there_is_no_route_but_the_network_is_still_unshared() {
    let sb = test_sandbox("api.example.com");
    let cmd = sb.command_for_worktree(Path::new("/work/1"), &["true".to_string()], &[]);
    let args = args_of(&cmd);
    assert!(args.iter().any(|a| a == "--unshare-net"), "{args:?}");
    assert!(
        !args.iter().any(|a| a == "/run/forge/egress.sock"),
        "{args:?}"
    );
    assert!(
        cmd.get_envs().all(|(k, _)| k != "HTTPS_PROXY"),
        "no proxy is named when there is none"
    );
}

#[tokio::test]
async fn check_phase_never_seeds_or_binds_agent_state() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("work");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut sandbox = test_sandbox("api.example.com");
    sandbox.config_dir = root.path().join("operator-claude");
    std::fs::create_dir_all(&sandbox.config_dir).unwrap();
    std::fs::write(
        sandbox.config_dir.join("settings.json"),
        "operator-settings",
    )
    .unwrap();
    let provider = provider_state_dir(&worktree);
    sandbox.prepare(&worktree, &[], Phase::Check).await;
    assert!(
        !provider.exists(),
        "checks must not even create provider state"
    );

    // A check after an agent launch must not reuse or alter its private state.
    std::fs::create_dir_all(provider.join("claude")).unwrap();
    let login = provider.join("claude/.credentials.json");
    std::fs::write(&login, "agent-login").unwrap();
    sandbox.prepare(&worktree, &[], Phase::Check).await;
    let argv = vec!["/bin/sh".into(), "-c".into(), "env; ls ~/.claude".into()];
    let command = sandbox.command(&worktree, &argv, &[], &Policy::new([]), Phase::Check);
    let args = args_of(&command);
    for dir in [
        &sandbox.config_dir,
        &sandbox.codex_dir,
        &sandbox.copilot_dir,
        &sandbox.home.join(".claude"),
    ] {
        assert!(
            args.windows(2)
                .any(|a| a[0] == "--tmpfs" && a[1] == dir.to_string_lossy())
        );
    }
    assert!(
        !args
            .iter()
            .any(|a| a.contains(&provider.to_string_lossy().to_string()))
    );
    assert!(!args.iter().any(|a| a.contains(CLAUDE_JSON_SEED)));
    assert!(!args.iter().any(|a| a.contains("cp -f")));
    assert_eq!(std::fs::read_to_string(&login).unwrap(), "agent-login");
    assert!(!provider.join("claude/settings.json").exists());
}

#[test]
fn relay_detect_strips_deleted_suffix() {
    // Other tests change provider binary overrides to temporary executables.
    // Detect reads those overrides, so run it with a private environment rather
    // than racing their cleanup or changing the parent test process's state.
    const CHILD: &str = "FORGE_TEST_RELAY_DETECT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let bwrap = bin.join("bwrap");
        std::fs::write(&bwrap, "#!/bin/sh\necho 'bubblewrap 0.9.0'\n").unwrap();
        std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sandbox::tests::relay_detect_strips_deleted_suffix",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", bin)
            .env("FORGE_BIN", "/opt/forge/forge (deleted)")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated detection failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let sb = Sandbox::detect(
        "/bin/sh",
        &crate::config::SandboxPaths {
            ro: Vec::new(),
            rw: Vec::new(),
            dependency_cache: None,
        },
        root.path().to_path_buf(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(sb.relay_exe, Path::new("/opt/forge/forge"));
    assert!(sb.extra_ro.contains(&PathBuf::from("/opt/forge")));
}

#[test]
fn relay_missing_executable_stops_before_launching_agent() {
    let root = tempfile::tempdir().unwrap();
    let mut sb = test_sandbox("api.example.com");
    sb.relay_exe = root.path().join("missing-relay");
    // Exercise the actual wrapper with a private run directory, without
    // requiring permission to create a network namespace on the test host.
    let script = sb
        .wrapper_script(true, None, Phase::Check)
        .replace("/run/forge", &root.path().to_string_lossy());
    let began = std::time::Instant::now();
    let output = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            &script,
            "wrapper",
            "/bin/sh",
            "-c",
            "echo agent-started",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(RELAY_START_FAILED), "{stderr}");
    assert!(stderr.contains("missing-relay"), "relay stderr: {stderr}");
    assert!(output.stdout.is_empty(), "the agent must not run");
    assert!(began.elapsed() >= std::time::Duration::from_secs(5));
    assert!(began.elapsed() < std::time::Duration::from_secs(15));
}
