use super::*;

pub(super) struct Fixture {
    pub dir: tempfile::TempDir,
    pub f: Forge,
    pub t: Task,
    pub cfg: config::Config,
    pub resolved: workflows::Resolved,
    pub run: Run,
    pub attempt_no: i64,
    pub remote_url: Option<String>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::ctx::Paths::for_home(dir.path().to_path_buf()).unwrap();
        let store = crate::store::Store::open(&dir.path().join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        let actions = workflows::load_actions(dir.path()).unwrap();
        let resolved = workflows::Resolved {
            steps: ["tests", "code"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| workflows::ResolvedStep {
                    action: actions[name].clone(),
                    model: None,
                    max_turns: None,
                    timeout_secs: None,
                    via: vec![],
                    node: format!("{i}-{name}"),
                })
                .collect(),
            ..Default::default()
        };
        let mut t = Task {
            task: "step fixture".into(),
            state: TaskState::Running,
            max_attempts: 2,
            max_turns: 10,
            worktree: dir.path().join("repo").display().to_string(),
            ..Default::default()
        };
        t.id = f.store.insert_task(&t).unwrap();
        let run = Run {
            hash: "workflow".into(),
            idx: 1,
            seq: 2,
            used: HashMap::from([(1, 1), (2, 1)]),
            owed: HashMap::new(),
            done: HashSet::from([1, 2, 3]),
        };
        let repo_path = dir.path().to_path_buf();
        Self {
            dir,
            f,
            t,
            cfg: config::Config {
                shared_target: false,
                repo_path,
                namespace: vec!["tests/acceptance/".into()],
                build_env: Default::default(),
                execution: Default::default(),
                checks: Default::default(),
                fixable: Default::default(),
                base_branch: "main".into(),
                push_remote: None,
                check_timeout_secs: 60,
                protected: vec![],
                egress: vec![],
                environment_deny: vec![],
                config_path: "forge.toml".into(),
            },
            resolved,
            run,
            attempt_no: 1,
            remote_url: None,
        }
    }

    pub fn args(&mut self) -> RunDirectiveStep<'_> {
        RunDirectiveStep {
            f: &self.f,
            t: &mut self.t,
            cfg: &self.cfg,
            resolved: &self.resolved,
            run: &mut self.run,
            step: &self.resolved.steps[1],
            seq: 2,
            attempt_no: &mut self.attempt_no,
            task_cap: 0.0,
            repo: self.dir.path(),
            wt: self.dir.path(),
            wait: false,
            remote_url: &self.remote_url,
        }
    }

    pub fn attempt(&self, state: AttemptState) -> crate::store::Attempt {
        let mut a = crate::store::Attempt {
            task_id: self.t.id,
            attempt_no: 1,
            step_seq: 2,
            state,
            start_sha: "start".into(),
            reason: "attempt reason".into(),
            ..Default::default()
        };
        a.id = self.f.store.insert_attempt(&a).unwrap();
        a
    }
}

pub(super) fn verdict() -> verify::Verdict {
    verify::Verdict::open(&verify::GitFacts::default())
}

pub(super) fn question(text: &str) -> crate::envelope::Envelope {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "summary": "the plan", "needs_input": {"question": text}
    }))
    .unwrap()
}

pub(super) fn check(tail: &str) -> CheckResult {
    CheckResult {
        level: "L1".into(),
        name: "typecheck".into(),
        ok: false,
        exit: Some(2),
        ms: 1,
        timed_out: false,
        tail: tail.into(),
        failing_tests: vec![],
        log_path: String::new(),
        stdout: String::new(),
    }
}
