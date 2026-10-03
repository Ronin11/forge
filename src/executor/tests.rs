use super::*;
#[test]
fn guarantees_for_follows_the_declared_backend_and_the_fallback() {
    let execution = Execution {
        bwrap: Err("bwrap not found".into()),
        fallback: Backend::Host,
        backends: Mutex::new(BTreeMap::new()),
        remotes: Mutex::new(BTreeMap::new()),
    };
    let (backend, g) = execution.guarantees_for(&config::Execution::default());
    assert_eq!(backend, Backend::Host);
    assert!(!g.egress_bounded);
    let declared = config::Execution {
        declared: Some(Backend::Bwrap),
        ..Default::default()
    };
    let (backend, g) = execution.guarantees_for(&declared);
    assert_eq!(backend, Backend::Bwrap);
    assert!(!g.egress_bounded && !g.worktree_private);
}

#[tokio::test]
async fn execution_config_defaults_and_rejects_unknown_backends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forge.toml");
    let defaults = "[defaults]\nbase_branch = \"main\"\n";
    std::fs::write(&path, defaults).unwrap();
    assert_eq!(
        config::load_working(dir.path())
            .await
            .unwrap()
            .execution
            .declared,
        None
    );
    std::fs::write(
        &path,
        format!("{defaults}[execution]\nbackend = \"host\"\n"),
    )
    .unwrap();
    assert_eq!(
        config::load_working(dir.path())
            .await
            .unwrap()
            .execution
            .declared,
        Some(Backend::Host)
    );
    std::fs::write(&path, format!("{defaults}[execution]\nbackend = \"ssh\"\n")).unwrap();
    assert!(config::load_working(dir.path()).await.is_err());
}

#[test]
fn host_preserves_argv_cwd_and_explicit_environment() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s:%s' \"$VALUE\" \"$1\"; test \"$PWD\" = \"$EXPECTED\"".into(),
        "sh".into(),
        "two words".into(),
    ];
    let env = vec![
        ("VALUE".into(), "a value".into()),
        ("EXPECTED".into(), dir.path().display().to_string()),
    ];
    let output = Host
        .command(dir.path(), &argv, &env, &Policy::new([]), Phase::Agent)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"a value:two words");
}

#[test]
fn unavailable_bwrap_never_falls_back_to_host() {
    let execution = Execution {
        bwrap: Err("bwrap unavailable".into()),
        fallback: Backend::Bwrap,
        backends: Mutex::new(BTreeMap::new()),
        remotes: Mutex::new(BTreeMap::new()),
    };
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["/bin/true".into()];
    assert!(
        execution
            .command(dir.path(), &argv, &[], Phase::Agent)
            .is_err()
    );
    execution.set_backend(dir.path(), Backend::Host);
    assert!(
        execution
            .command(dir.path(), &argv, &[], Phase::Agent)
            .unwrap()
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(execution.backend(&dir.path().join("child")), Backend::Host);
}

#[test]
fn without_bwrap_an_undeclared_repository_runs_on_the_host() {
    let execution = Execution {
        bwrap: Err("bwrap not found".into()),
        fallback: Backend::Host,
        backends: Mutex::new(BTreeMap::new()),
        remotes: Mutex::new(BTreeMap::new()),
    };
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
    execution.configure(dir.path(), &config::Execution::default());
    assert_eq!(execution.backend(dir.path()), Backend::Host);
    assert!(!execution.guarantees(dir.path()).egress_bounded);
    assert!(
        execution
            .command(dir.path(), &argv, &[], Phase::Agent)
            .unwrap()
            .output()
            .unwrap()
            .status
            .success()
    );
    // Declaring bwrap still fails closed.
    execution.configure(
        dir.path(),
        &config::Execution {
            declared: Some(Backend::Bwrap),
            ..Default::default()
        },
    );
    assert!(
        execution
            .command(dir.path(), &argv, &[], Phase::Agent)
            .is_err()
    );
}
/// Use the launch harness so route failures are tested even without bwrap privileges.
fn bwrap_in(dir: PathBuf) -> Execution {
    let (mut execution, _) = crate::agent::fake_bwrap(dir.parent().unwrap(), 0);
    execution.bwrap.as_mut().unwrap().proxies =
        std::sync::Arc::new(crate::egress::Proxies::in_dir(dir));
    execution
}

#[tokio::test]
async fn a_removed_proxy_directory_is_rebuilt_before_the_next_check() {
    let root = tempfile::tempdir().unwrap();
    let proxies = root.path().join("proxies");
    let execution = bwrap_in(proxies.clone());
    let wt = root.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    let argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
    execution.command(&wt, &argv, &[], Phase::Check).unwrap();
    assert!(proxies.is_dir());
    // A tmp cleaner sweeps the directory under the running proxies.
    std::fs::remove_dir_all(&proxies).unwrap();
    let timeout = std::time::Duration::from_secs(60);
    let r = crate::checks::run_one("L1", "t", &argv, &wt, Some(&execution), timeout, &[])
        .await
        .unwrap();
    let socket = std::fs::read_dir(&proxies).unwrap().flatten().next();
    assert!(socket.is_some(), "the proxy was not rebuilt");
    assert!(r.ok, "{}", r.tail);
}

#[tokio::test]
async fn an_unbindable_proxy_directory_is_an_env_fault_naming_it() {
    let root = tempfile::tempdir().unwrap();
    // A file where the directory should be: nothing can be made there.
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, "").unwrap();
    let proxies = blocker.join("proxies");
    let mut execution = bwrap_in(root.path().join("unused"));
    execution.bwrap.as_mut().unwrap().proxies =
        std::sync::Arc::new(crate::egress::Proxies::in_dir(proxies.clone()));
    let argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
    let timeout = std::time::Duration::from_secs(60);
    let error = crate::checks::run_one(
        "L1",
        "t",
        &argv,
        root.path(),
        Some(&execution),
        timeout,
        &[],
    )
    .await
    .expect_err("launched without a route out");
    assert!(
        format!("{error:#}").contains(&proxies.display().to_string()),
        "{error:#}"
    );
    use crate::engine::{Classify, Fault};
    assert!(matches!(Err::<(), _>(error).task(), Err(Fault::Env(_))));
}
