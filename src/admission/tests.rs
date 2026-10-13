//! `admission::at_claim` against a fake Jev answering TypeSafe's shape on
//! a local port.

use super::*;
use crate::ctx::Paths;
use crate::store::Store;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) fn choice(label: &str, p: f64) -> Value {
    json!({"type": "choice", "choice": label, "confidence": p,
        "probabilities": {label: p}})
}

/// A request's answers: `admit` at probability `yes`, a `gap`, and the
/// `band` the request asked for.
fn answers(yes: f64, gap: &str, band: (&str, f64)) -> Value {
    json!({
        "admit": {"type": "choice", "choice": if yes >= 0.5 { "yes" } else { "no" },
            "confidence": yes.max(1.0 - yes),
            "probabilities": {"yes": yes, "no": 1.0 - yes}},
        "gap": choice(gap, 0.8),
        "band": choice(band.0, band.1),
    })
}

pub(crate) type Reply = Arc<dyn Fn(&Value) -> (u16, Value) + Send + Sync>;

/// A server answering every POST with `reply(body)`; returns its port and
/// the bodies it saw.
pub(crate) async fn fake(reply: Reply) -> (u16, Arc<std::sync::Mutex<Vec<Value>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut c, _) = l.accept().await.unwrap();
            let (reply, log) = (reply.clone(), log.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let body = loop {
                    let mut chunk = [0u8; 4096];
                    let n = c.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let want = head
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if rest.len() >= want {
                            break rest[..want].to_string();
                        }
                    }
                };
                let req: Value = serde_json::from_str(&body).unwrap();
                log.lock().unwrap().push(req.clone());
                let (code, v) = reply(&req);
                let out = json!({"model": "jev", "answers": v, "usage": {"input_tokens": 10}})
                    .to_string();
                let resp = format!(
                    "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                    out.len()
                );
                c.write_all(resp.as_bytes()).await.ok();
            });
        }
    });
    (port, seen)
}

pub(crate) fn forge(port: Option<u16>, extra: &str) -> (tempfile::TempDir, Forge) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let provider = port
        .map(|p| {
            format!(
                "[providers.jev]\nrunner = \"jev\"\nbackend = \"typesafe\"\nbase_url = \"http://127.0.0.1:{p}/v1/systemone\"\napi_key_env = \"PATH\"\n"
            )
        })
        .unwrap_or_default();
    std::fs::write(home.join("config.toml"), format!("{provider}{extra}")).unwrap();
    let cfg = crate::config::load_home(&home).unwrap();
    let paths = Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home,
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&paths.home.join("forge.db")).unwrap();
    let mut f = Forge::open_with(paths, store).unwrap();
    f.providers = cfg.providers;
    f.admission = cfg.admission;
    (dir, f)
}

/// A task claimed by this process, as `claim::task` hands it over.
fn claimed(f: &Forge, trust: Trust, text: &str) -> Task {
    let id = f
        .store
        .insert_task(&Task {
            repo: "repo".into(),
            task: text.into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Queued,
            created_at: crate::unix_now(),
            workflow: "direct".into(),
            trust,
            ..Default::default()
        })
        .unwrap();
    assert!(f.store.claim(id, std::process::id() as i64).unwrap());
    f.store.task(id).unwrap().unwrap()
}

fn kinds(f: &Forge, kind: &str) -> usize {
    f.store.decisions_of_kind_since(kind, 0).unwrap().len()
}

fn ok(yes: f64, gap: &'static str) -> Reply {
    Arc::new(move |_| (200, answers(yes, gap, ("judgment", 0.95))))
}

#[tokio::test]
async fn a_contact_task_at_the_threshold_is_admitted_and_its_band_recorded() {
    let (port, seen) = fake(ok(0.95, "none")).await;
    let (_d, f) = forge(Some(port), "");
    let t = claimed(&f, Trust::Contact, "rename the helper in src/util.rs");
    let t = at_claim(&f, t).await.unwrap().expect("admitted");
    assert_eq!(t.state, TaskState::Running);
    assert_eq!((t.band.as_str(), t.band_p), ("judgment", Some(0.95)));
    assert_eq!(kinds(&f, ADMIT), 1);
    assert_eq!(kinds(&f, BAND), 1);
    let sent = seen.lock().unwrap();
    assert_eq!(sent.len(), 1, "both questions in one request");
    assert_eq!(sent[0]["questions"]["admit"]["instructions"], QUESTION);
    assert!(sent[0]["questions"]["band"].is_object());
}

#[tokio::test]
async fn under_the_threshold_the_task_blocks_with_the_probability_and_the_gap() {
    let (port, _) = fake(ok(0.62, "decision")).await;
    let (_d, f) = forge(Some(port), "");
    let t = claimed(&f, Trust::Public, "make checkout nicer");
    assert!(at_claim(&f, t.clone()).await.unwrap().is_none());
    let after = f.store.task(t.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Blocked);
    assert!(after.reason.contains("0.62"), "{}", after.reason);
    assert!(after.reason.contains("decision"), "{}", after.reason);
    assert_eq!(kinds(&f, BLOCK), 1);
    assert_eq!(kinds(&f, ADMIT), 0);
    // The question is the operator's to answer like any other.
    let last = f.store.attempts(t.id).unwrap().pop().unwrap();
    assert_eq!(last.state, AttemptState::NeedsInput);
    assert_eq!(last.step, STEP);
    // The band was still recorded.
    assert_eq!(after.band, "judgment");
}

#[tokio::test]
async fn an_operator_task_is_admitted_as_it_is_and_only_the_band_is_asked() {
    let (port, seen) = fake(ok(0.01, "vague")).await;
    let (_d, f) = forge(Some(port), "");
    let t = claimed(&f, Trust::Operator, "do the thing");
    let t = at_claim(&f, t).await.unwrap().expect("admitted");
    assert_eq!(t.state, TaskState::Running);
    assert_eq!(t.band, "judgment");
    assert_eq!(kinds(&f, ADMIT) + kinds(&f, BLOCK), 0);
    assert!(seen.lock().unwrap()[0]["questions"].get("admit").is_none());
    // Asked once: a second claim of the same task asks nothing.
    let again = f.store.task(t.id).unwrap().unwrap();
    at_claim(&f, again).await.unwrap().unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_exclude_list_blocks_without_asking_jev() {
    let (port, seen) = fake(ok(0.99, "none")).await;
    let (_d, f) = forge(Some(port), "");
    let t = claimed(
        &f,
        Trust::Contact,
        "Rotate the API key used by the importer",
    );
    assert!(at_claim(&f, t.clone()).await.unwrap().is_none());
    assert_eq!(seen.lock().unwrap().len(), 0);
    assert_eq!(
        f.store.task(t.id).unwrap().unwrap().state,
        TaskState::Blocked
    );
    assert_eq!(kinds(&f, EXCLUDE), 1);
    // An operator's own task with the same words is not excluded.
    let own = claimed(
        &f,
        Trust::Operator,
        "Rotate the API key used by the importer",
    );
    assert!(at_claim(&f, own).await.unwrap().is_some());
}

#[tokio::test]
async fn the_daily_cap_is_counted_from_the_decision_rows() {
    let (port, seen) = fake(ok(0.97, "none")).await;
    let (_d, f) = forge(Some(port), "[admission]\nper_day = 2\n");
    for i in 0..2 {
        let t = claimed(&f, Trust::Contact, &format!("fix typo number {i}"));
        assert!(!capped(&f, &t));
        let t = at_claim(&f, t).await.unwrap().expect("admitted");
        // A re-run over the admitted task asks nothing and admits nothing.
        at_claim(&f, t).await.unwrap().unwrap();
    }
    assert_eq!(kinds(&f, ADMIT), 2);
    assert_eq!(seen.lock().unwrap().len(), 2);
    let third = claimed(&f, Trust::Public, "fix typo number 3");
    assert!(capped(&f, &third), "the cap is spent: left queued");
    // A task that was admitted is never capped, and an operator's is not gated.
    assert!(!capped(&f, &claimed(&f, Trust::Operator, "anything")));
    // Reached anyway (a racing worker): it blocks rather than over-admit.
    assert!(at_claim(&f, third).await.unwrap().is_none());
    assert_eq!(kinds(&f, ADMIT), 2);
}

#[tokio::test]
async fn a_jev_error_admits_an_operator_task_and_parks_any_other() {
    let (port, _) = fake(Arc::new(|_| (500, json!({})))).await;
    let (_d, f) = forge(Some(port), "");
    let own = claimed(&f, Trust::Operator, "do it");
    let own = at_claim(&f, own).await.unwrap().expect("admitted");
    assert_eq!((own.band.as_str(), own.band_p), ("", None));
    let other = claimed(&f, Trust::Contact, "do it too");
    assert!(at_claim(&f, other.clone()).await.unwrap().is_none());
    let after = f.store.task(other.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Blocked);
    assert!(
        after.reason.contains("Jev could not be asked"),
        "{}",
        after.reason
    );
    assert_eq!(kinds(&f, PARK), 1);
}

#[tokio::test]
async fn with_no_jev_provider_the_step_is_off() {
    let (_d, f) = forge(None, "");
    let t = claimed(&f, Trust::Public, "Rotate the API key");
    let t = at_claim(&f, t).await.unwrap().expect("untouched");
    assert_eq!(t.state, TaskState::Running);
    assert!(!capped(&f, &t));
    assert_eq!(kinds(&f, EXCLUDE), 0);
    // A claude lane is never asked: only a `jev` runner counts.
    let (port, _) = fake(ok(0.99, "none")).await;
    let (_d, mut f) = forge(Some(port), "");
    f.providers.get_mut("jev").unwrap().runner = Runner::ClaudeCli;
    assert!(provider(&f).is_none());
}

/// A size model answering `outcome` with `large` at 0.83.
fn size_reply() -> Reply {
    Arc::new(|_| {
        (
            200,
            json!({"outcome": {"type": "score",
                "probabilities": {"small": 0.05, "medium": 0.12, "large": 0.83}}}),
        )
    })
}

/// The operator config naming a size provider on `port` with `extra`
/// under `[admission]`.
fn size_config(port: u16, extra: &str) -> String {
    format!(
        "[providers.indecis-size]\nrunner = \"jev\"\nbackend = \"typesafe\"\nbase_url = \"http://127.0.0.1:{port}/v1/systemone\"\napi_key_env = \"PATH\"\n[admission]\nprovider = \"jev\"\nsize_provider = \"indecis-size\"\n{extra}"
    )
}

/// A repository the claim can load the configuration of.
fn repo(dir: &tempfile::TempDir) -> String {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("forge.toml"), "[checks]\nok = [\"true\"]\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@x",
            "commit",
            "-qm",
            "init",
            "--allow-empty",
        ],
    ] {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    repo.display().to_string()
}

#[tokio::test]
async fn the_size_is_predicted_once_and_routes_a_default_workflow() {
    let (jev, _) = fake(ok(0.95, "none")).await;
    let (size_port, seen) = fake(size_reply()).await;
    let (dir, f) = forge(
        Some(jev),
        &size_config(size_port, "size_workflows = { large = \"planned\" }\n"),
    );
    let mut t = claimed(&f, Trust::Operator, "rework the importer end to end");
    t.repo = repo(&dir);
    f.store.update_task(&t).unwrap();
    let t = at_claim(&f, t).await.unwrap().expect("runs");
    assert_eq!(t.predicted_size, "large");
    assert_eq!(t.predicted_size_p, Some(0.83));
    assert_eq!(
        (t.workflow.as_str(), t.workflow_source.as_str()),
        ("planned", "size")
    );
    assert!(!t.workflow_hash.is_empty() && t.workflow_text.contains("investigate"));
    let row = f.store.task(t.id).unwrap().unwrap();
    assert_eq!(
        (row.workflow.as_str(), row.workflow_source.as_str()),
        ("planned", "size")
    );
    assert_eq!(row.predicted_size, "large");
    let d = f
        .store
        .decisions_of_kind_since(crate::admission::size::KIND, 0)
        .unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].answered_by, "indecis-size");
    assert_eq!(d[0].answer, "large (p 0.83)");
    assert!(
        d[0].citations.ends_with("routed direct -> planned"),
        "{}",
        d[0].citations
    );
    {
        let sent = seen.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let q = &sent[0]["questions"]["outcome"];
        assert_eq!(q["type"], "score");
        assert_eq!(q["instructions"], crate::admission::size::instructions());
        assert_eq!(q["criteria"], json!(["small", "medium", "large"]));
    }
    // Asked once: a second claim of the same task asks nothing.
    at_claim(&f, row).await.unwrap().unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_flag_named_workflow_a_low_probability_and_an_unmapped_size_keep_the_workflow() {
    let (jev, _) = fake(ok(0.95, "none")).await;
    let (size_port, _) = fake(size_reply()).await;
    let (dir, f) = forge(
        Some(jev),
        &size_config(size_port, "size_workflows = { large = \"planned\" }\n"),
    );
    let mut t = claimed(&f, Trust::Operator, "rework the importer end to end");
    t.repo = repo(&dir);
    t.workflow_source = "flag".into();
    f.store.update_task(&t).unwrap();
    let t = at_claim(&f, t).await.unwrap().unwrap();
    assert_eq!(
        (t.workflow.as_str(), t.predicted_size.as_str()),
        ("direct", "large")
    );
    let d = f
        .store
        .decisions_of_kind_since(crate::admission::size::KIND, 0)
        .unwrap();
    assert!(
        d[0].citations.contains("kept direct: the request named it"),
        "{}",
        d[0].citations
    );

    let (dir, f) = forge(
        Some(jev),
        &size_config(
            size_port,
            "size_workflows = { large = \"planned\" }\nsize_threshold = 0.9\n",
        ),
    );
    let mut t = claimed(&f, Trust::Operator, "rework the importer end to end");
    t.repo = repo(&dir);
    f.store.update_task(&t).unwrap();
    let t = at_claim(&f, t).await.unwrap().unwrap();
    assert_eq!(t.workflow, "direct");
    let d = f
        .store
        .decisions_of_kind_since(crate::admission::size::KIND, 0)
        .unwrap();
    assert!(
        d[0].citations.contains("under the routing threshold 0.90"),
        "{}",
        d[0].citations
    );

    let (_dir, f) = forge(
        Some(jev),
        &size_config(size_port, "size_workflows = { small = \"cheap\" }\n"),
    );
    let t = claimed(&f, Trust::Operator, "rework the importer end to end");
    let t = at_claim(&f, t).await.unwrap().unwrap();
    assert_eq!(t.workflow, "direct");
    let d = f
        .store
        .decisions_of_kind_since(crate::admission::size::KIND, 0)
        .unwrap();
    assert!(
        d[0].citations.contains("no workflow is mapped for large"),
        "{}",
        d[0].citations
    );
}

#[tokio::test]
async fn a_size_model_error_or_no_size_provider_leaves_the_task_as_it_was() {
    let (jev, _) = fake(ok(0.95, "none")).await;
    let (size_port, seen) = fake(Arc::new(|_| (500, json!({})))).await;
    let (_dir, f) = forge(Some(jev), &size_config(size_port, ""));
    let t = claimed(&f, Trust::Operator, "do the thing");
    let t = at_claim(&f, t).await.unwrap().unwrap();
    assert_eq!(t.state, TaskState::Running);
    assert!(t.predicted_size.is_empty() && t.predicted_size_p.is_none());
    assert_eq!(
        f.store
            .decisions_of_kind_since(crate::admission::size::KIND, 0)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(seen.lock().unwrap().len(), 1);

    let (_dir, f) = forge(Some(jev), "");
    let t = claimed(&f, Trust::Operator, "do the thing");
    let t = at_claim(&f, t).await.unwrap().unwrap();
    assert!(t.predicted_size.is_empty());
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "no size provider: nothing asked"
    );
}
