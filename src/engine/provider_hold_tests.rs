use super::*;
use crate::ctx::Paths;
use crate::store::Store;

fn fixture() -> (tempfile::TempDir, Forge, Task) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::create_dir(home.join("workflows")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[providers.openai]\nrunner = \"codex-cli\"\nmodel = \"test\"\n",
    )
    .unwrap();
    std::fs::write(
        home.join("workflows/experiment.toml"),
        "[factors.code]\nopenai = 0.5\nanthropic = 0.5\n\
         [factors.review]\nopenai = 0.5\nanthropic = 0.5\n",
    )
    .unwrap();
    let store = Store::open(&home.join("forge.db")).unwrap();
    let f = Forge::open_with(
        Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        },
        store,
    )
    .unwrap();
    let mut t = Task {
        repo: "repo".into(),
        task: "direct run".into(),
        state: TaskState::Running,
        explore: BTreeMap::from([
            ("code".into(), "openai".into()),
            ("review".into(), "openai".into()),
        ]),
        ..Default::default()
    };
    t.id = f.store.insert_task(&t).unwrap();
    (dir, f, t)
}

fn hold(f: &Forge, t: &Task, provider: &str, until: i64) {
    rusqlite::Connection::open(f.paths.home.join("forge.db"))
        .unwrap()
        .execute(
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, finished_at,
         provider, rl_five_hour, rl_five_hour_resets)
         VALUES (?1, 1, 'agent_failed', ?2, ?2, ?3, 1.0, ?4)",
            rusqlite::params![t.id, unix_now(), provider, until],
        )
        .unwrap();
}

#[tokio::test]
async fn run_redraws_each_roles_held_arm_and_persists_the_note() {
    let (_dir, f, mut t) = fixture();
    hold(&f, &t, "openai", unix_now() + 3600);
    for role in ["code", "review"] {
        assert!(
            before_attempt(&f, &mut t, role, true)
                .await
                .map_err(anyhow::Error::from)
                .unwrap()
                .is_none()
        );
        assert_eq!(f.effective_provider(&t, role).unwrap().name, "anthropic");
        let saved = f.store.task(t.id).unwrap().unwrap();
        assert_eq!(saved.state, TaskState::Running);
        assert_eq!(saved.explore[role], "anthropic");
        assert!(saved.explore[&redraw::note_key(role)].contains("re-drawn anthropic"));
    }
}

#[tokio::test]
async fn run_redraw_uses_the_roles_renormalised_open_weights() {
    let (_dir, mut f, original) = fixture();
    let mut alternate = f.providers["openai"].clone();
    alternate.name = "alternate".into();
    f.providers.insert("alternate".into(), alternate);
    std::fs::write(
        f.paths.home.join("workflows/experiment.toml"),
        "[factors.review]\nopenai = 0.5\nanthropic = 0.4\nalternate = 0.1\n",
    )
    .unwrap();
    hold(&f, &original, "openai", unix_now() + 3600);
    let open = BTreeMap::from([("anthropic".into(), 0.8), ("alternate".into(), 0.2)]);
    for _ in 0..32 {
        let mut t = original.clone();
        t.id = f.store.insert_task(&t).unwrap();
        let expected = crate::experiment::draw_level(t.id, "review", &open).unwrap();
        assert!(
            before_attempt(&f, &mut t, "review", true)
                .await
                .map_err(anyhow::Error::from)
                .unwrap()
                .is_none()
        );
        let saved = f.store.task(t.id).unwrap().unwrap();
        assert_eq!(saved.explore["review"], expected);
        assert_eq!(saved.explore["code"], "openai");
        assert_eq!(saved.state, TaskState::Running);
        assert!(saved.explore[&redraw::note_key("review")].contains(&expected));
    }
}

#[tokio::test]
async fn no_wait_keeps_the_original_arm_and_requeues() {
    let (_dir, f, mut t) = fixture();
    hold(&f, &t, "openai", unix_now() + 3600);
    assert!(matches!(
        before_attempt(&f, &mut t, "review", false)
            .await
            .map_err(anyhow::Error::from)
            .unwrap(),
        Some(StepFlow::Requeue(_))
    ));
    assert_eq!(t.explore["review"], "openai");
    assert!(!t.explore.contains_key("redraw:review"));
}

#[tokio::test]
async fn all_arms_held_waits_for_the_earliest_reset_without_requeuing() {
    let (_dir, f, mut t) = fixture();
    let now = unix_now();
    hold(&f, &t, "openai", now + 3600);
    hold(&f, &t, "anthropic", now + 1);
    assert!(
        matches!(routing(&f, &t, "review").map_err(anyhow::Error::from).unwrap(),
        Routing::Held { provider, until, .. } if provider == "anthropic" && until == now + 1)
    );
    let flow = tokio::time::timeout(
        Duration::from_secs(3),
        before_attempt(&f, &mut t, "review", true),
    )
    .await
    .unwrap()
    .map_err(anyhow::Error::from)
    .unwrap();
    assert!(flow.is_none());
    assert_eq!(t.explore["review"], "anthropic");
    assert_eq!(
        f.store.task(t.id).unwrap().unwrap().state,
        TaskState::Running
    );
    let log = std::fs::read_to_string(f.paths.home.join("events.jsonl")).unwrap();
    assert!(log.contains("waiting in foreground"));
}
