//! Text-snapshot tests for the TUI: drive `forge_tui::App` against a fake
//! `forge` binary that answers fixed fixture JSON (the same technique
//! `web/tests/server.rs` uses to fake `forge` for the web server), render
//! each screen with ratatui's `TestBackend` at a fixed 100x40 size — no
//! real terminal involved — and compare the plain-text buffer to a
//! checked-in file under `tui/tests/snapshots/`.
//!
//! A snapshot is never written automatically. To update one deliberately
//! after a rendering change, re-run with `UPDATE_SNAPSHOTS=1`, e.g.:
//!
//!   UPDATE_SNAPSHOTS=1 cargo test -p forge-tui --test snapshots
//!
//! then read the diff in `git diff tui/tests/snapshots/` before committing
//! it — the point of a checked-in snapshot is that changing what the TUI
//! shows is a reviewable diff, not a silent side effect.

use crossterm::event::{KeyCode, KeyModifiers};
use forge_client::Forge;
use forge_tui::{App, Screen, draw, render_text};
use std::path::PathBuf;
use std::sync::Mutex;

/// The local zone the snapshots are rendered in: half an hour off the hour and
/// no daylight time, written as a POSIX rule so no zoneinfo database is needed.
const LOCAL_TZ: &str = "IST-5:30";

/// `TZ` is process-wide and the tests run on threads: whoever sets it holds this.
static TZ: Mutex<()> = Mutex::new(());

/// Runs `f` with `TZ` set to `tz` (or unset for `None`), then puts it back.
fn under_tz<T>(tz: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _held = TZ.lock().unwrap_or_else(|e| e.into_inner());
    let before = std::env::var_os("TZ");
    // SAFETY: the only code that reads the zone from C (`tzset`) runs on the
    // thread holding `TZ`, and std's own readers take std's environment lock.
    unsafe {
        match tz {
            Some(tz) => std::env::set_var("TZ", tz),
            None => std::env::remove_var("TZ"),
        }
    }
    let out = f();
    unsafe {
        match before {
            Some(v) => std::env::set_var("TZ", v),
            None => std::env::remove_var("TZ"),
        }
    }
    out
}

/// A fake `forge` binary: one `case` arm per verb the TUI calls while
/// running these scripts, each printing fixed fixture JSON. `trace`
/// answers with a task that has both a deploy and an assessment, so the
/// same fixture covers the "task view with a deploy and an assessment"
/// requirement whichever task id opens it.
const FAKE: &str = r#"#!/bin/bash
case "$1" in
  snapshot) cat <<'JSON'
{"events_offset":0,"tasks":[{"id":12,"state":"running","workflow":"tdd","attempts":2,"cost_usd":1.35,"repo":"/r","text":"add snapshot tests for the tui","task":"add snapshot tests for the tui","created_at":1,"created":"","trust":"operator","project":"forge"},{"id":11,"state":"succeeded","workflow":"direct","attempts":1,"cost_usd":0.1,"repo":"/r","text":"fix typo in docs","task":"fix typo in docs","created_at":1,"created":"","trust":"operator","project":"forge"}],"requests":[],"worker":{"running":true,"pid":555,"exe":"","stale_binary":false}}
JSON
  ;;
  log | requests) echo '[]' ;;
  stats) cat "$(dirname "$0")/stats.json" ;;
  doctor) cat "$(dirname "$0")/doctor.json"; exit 1 ;;
  gc) echo "removed 3 worktrees" ;;
  project)
    case "$2" in
      list) cat "$(dirname "$0")/projects.json" ;;
      deploy) cat "$(dirname "$0")/deploy_targets.json" ;;
      *) echo "unexpected project: $*" >&2; exit 2 ;;
    esac ;;
  deploy)
    if [ "$2" = log ]; then
      if [ "$4" = prod ]; then cat "$(dirname "$0")/deploy_log_prod.json"; else echo '[]'; fi
    else
      echo "deploy: $2 $3" >> "$(dirname "$0")/ran"; echo "deployed $2/$3"
    fi ;;
  events) exit 0 ;;
  initiative)
    case "$2" in
      list) cat <<'JSON'
[{"id":3,"project":"forge","outcome":"cover the tui with text-snapshot tests","state":"open","held_rule":null,"queued":1,"running":1,"succeeded":3,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":4.2,"budget_usd":null,"stop_after_same_rule":3,"created_at":1,"settled_at":null},{"id":7,"project":"equitizr","outcome":"the billing page shows usage-based line items","state":"held","held_rule":"budget","queued":1,"running":0,"succeeded":1,"failed":1,"unverified":0,"blocked":1,"withdrawn":0,"cost_usd":10.5,"budget_usd":10.0,"stop_after_same_rule":3,"created_at":1,"settled_at":null},{"id":2,"project":"forge","outcome":"an older, settled initiative","state":"done","held_rule":null,"queued":0,"running":0,"succeeded":5,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":9.9,"budget_usd":20.0,"stop_after_same_rule":3,"created_at":1,"settled_at":2}]
JSON
      ;;
      set) echo "set: $*" >&2; [ "$3" = 7 ] && [ "$4" = --budget ] && [ "$5" = 25 ] && echo ok || exit 2 ;;
      report) cat <<'JSON'
{
  "id": 7,
  "project": "equitizr",
  "outcome": "the billing page shows usage-based line items",
  "state": "held",
  "held_rule": "budget",
  "budget_usd": 10.0,
  "stop_after_same_rule": 3,
  "tasks": [
    { "id": 40, "state": "succeeded", "reason": "", "retries": 0, "score": 8, "cost_usd": 3.2 },
    { "id": 41, "state": "failed", "reason": "cargo test failed", "retries": 2, "score": null, "cost_usd": 4.75 },
    { "id": 42, "state": "blocked", "reason": "needs input: which pricing tier?", "retries": 0, "score": null, "cost_usd": 2.55 },
    { "id": 43, "state": "queued", "reason": "", "retries": 0, "score": null, "cost_usd": 0.0 }
  ],
  "refused": [
    { "rule": "cargo clippy", "count": 3 },
    { "rule": "cargo test", "count": 1 }
  ],
  "rulings": [
    { "task_id": 41, "question": "is the retry budget worth raising?", "answer": "no, withdraw and refile narrower", "citations": "task 41, decision 12" }
  ],
  "questions": [
    { "task_id": 42, "question": "which pricing tier?", "answer": null }
  ],
  "deployed": [
    { "task_id": 40, "target": "prod", "sha": "cdcce8adba0629506cf8e6c6e8f3cb56479ce850", "check_ok": true, "rolled_back_to": null, "findings": [] }
  ],
  "cost_usd": 10.5,
  "elapsed_secs": 5400,
  "created_at": 1789760000,
  "settled_at": null,
  "proposal": null
}
JSON
      ;;
      *) echo "unexpected initiative: $*" >&2; exit 2 ;;
    esac ;;
  trace)
    id="$2"
    cat <<JSON
{"task":{"id":$id,"state":"succeeded","workflow":"tdd","reason":"","branch":"forge/$id-x","base_sha":"abcdef1234567890","project":"forge","initiative":3,"after":[],"retry_of":null,"text":"add snapshot tests for the tui"},"attempts":[{"attempt_no":1,"step":"code","step_seq":0,"state":"succeeded","started_at":1,"timed_out":false,"num_turns":9,"tool_calls":0,"cost_usd":0.42,"agent_ms":0,"commits":1,"files_changed":1,"dirty":false,"start_sha":"","end_sha":"","log_path":"","tokens":{},"rate_limits":{},"inputs":{},"outputs":{},"reason":"","verdict":[],"envelope":{}}],"ops":[{"id":1,"seq":0,"name":"clone","kernel":true,"started_at":0,"ms":0,"ok":true,"detail":"ok","output":""},{"id":2,"seq":1,"name":"verify","kernel":true,"started_at":0,"ms":0,"ok":true,"detail":"ok","output":""}],"resolved":null,"diagnosis":[],"deploys":[{"id":1,"project":"forge","target":"prod","sha":"abcdef1234567890","started_at":1,"finished_at":2,"check_ok":true,"check_output":"ok","rolled_back_to":null,"reason":""}],"assessment":{"score":82,"findings":[{"path":"tui/src/lib.rs","finding":"missing coverage of the initiative screen","severity":"minor"}],"model":"claude-sonnet-5","provider":"anthropic","cost_usd":0.05,"created_at":1}}
JSON
  ;;
  job)
    case "$2" in
      list) cat <<'JSON'
[{"id":21,"project":"forge","workflow":"nightly-cleanup","workflow_hash":"h1","landed_sha":"deadbeef","trigger_kind":"schedule","trigger_ref":"0 3 * * *","state":"ok","workflow_source":"repo","dry_run":false,"started_at":1790000000,"finished_at":1790000050,"cost_usd":0.12,"verdict_json":"[]","due_at":null},{"id":20,"project":"forge","workflow":"weekly-report","workflow_hash":"h2","landed_sha":"deadbeef","trigger_kind":"manual","trigger_ref":"","state":"running","workflow_source":"catalog","dry_run":false,"started_at":1789999900,"finished_at":null,"cost_usd":null,"verdict_json":"","due_at":null}]
JSON
      ;;
      show)
        id="$3"
        cat <<JSON
{"id":$id,"project":"forge","workflow":"nightly-cleanup","workflow_hash":"h1","landed_sha":"deadbeef","trigger_kind":"schedule","trigger_ref":"0 3 * * *","state":"ok","workflow_source":"repo","dry_run":false,"started_at":1790000000,"finished_at":1790000050,"cost_usd":0.12,"verdict_json":"[]","due_at":1790003600,"steps":[{"id":1,"job_id":$id,"seq":1,"action":"run-checks","kind":"directive","provider":"anthropic","model":"claude-sonnet-5","cost_usd":0.05,"started_at":1000,"finished_at":1020,"exit_code":0,"output_ref":"","tail":""},{"id":2,"job_id":$id,"seq":2,"action":"notify","kind":"shell","provider":"","model":"","cost_usd":null,"started_at":1020,"finished_at":1050,"exit_code":0,"output_ref":"","tail":""}],"effects":[{"id":1,"job_id":$id,"seq":1,"kind":"message","target":"ops-channel","summary":"posted the nightly summary","dry_run":false},{"id":2,"job_id":$id,"seq":2,"kind":"file","target":"reports/nightly.md","summary":"wrote the report","dry_run":false}]}
JSON
      ;;
      *) echo "unexpected job: $*" >&2; exit 2 ;;
    esac ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

struct Fake {
    _dir: tempfile::TempDir,
    forge: Forge,
}

fn fake_forge() -> Fake {
    fake_forge_with_stats(include_str!("fixtures/stats.json"))
}

/// A fake `forge` whose `stats --json` prints `stats`.
fn fake_forge_with_stats(stats: &str) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("stats.json"), stats).unwrap();
    for (name, text) in [
        ("projects.json", include_str!("fixtures/projects.json")),
        (
            "deploy_targets.json",
            include_str!("fixtures/deploy_targets.json"),
        ),
        (
            "deploy_log_prod.json",
            include_str!("fixtures/deploy_log_prod.json"),
        ),
        ("doctor.json", include_str!("fixtures/doctor.json")),
    ] {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let bin = dir.path().join("forge");
    std::fs::write(&bin, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let forge = Forge {
        bin: bin.to_string_lossy().into_owned(),
        ..Forge::new()
    };
    Fake { _dir: dir, forge }
}

/// One frame, 100x40, as plain text — no real terminal — with times in the
/// local zone `LOCAL_TZ`.
fn frame(app: &App) -> String {
    frame_in(app, Some(LOCAL_TZ))
}

/// The same frame with the environment's zone set to `tz`.
fn frame_in(app: &App, tz: Option<&str>) -> String {
    under_tz(tz, || {
        let backend = ratatui::backend::TestBackend::new(100, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        render_text(terminal.backend())
    })
}

/// Compares `actual` to `tui/tests/snapshots/<name>.txt`. With
/// `UPDATE_SNAPSHOTS` set, writes it instead — the only way a snapshot
/// file changes; nothing here writes one on a plain test run.
fn assert_snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run with UPDATE_SNAPSHOTS=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        expected,
        actual,
        "{} differs from the rendered screen; if the new rendering is correct, \
         re-run with UPDATE_SNAPSHOTS=1 and review the diff before committing it",
        path.display()
    );
}

#[test]
fn the_task_list_renders_the_queue() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    let text = frame(&app);
    app.shutdown();
    assert_snapshot("task_list", &text);
}

#[test]
fn a_task_view_renders_a_deploy_and_an_assessment() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.open_task(12);
    assert_eq!(app.screen(), Screen::Task);
    let text = frame(&app);
    app.shutdown();
    assert_snapshot("task_view_deploy_assessment", &text);
}

#[test]
fn the_initiative_list_renders_every_initiative() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Initiatives);
    let text = frame(&app);
    app.shutdown();
    assert_snapshot("initiative_list", &text);
}

#[test]
fn a_held_initiative_renders_its_report() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.open_initiative(7);
    assert_eq!(app.screen(), Screen::Initiative);
    let text = frame(&app);
    app.shutdown();
    assert!(text.contains("held: budget"), "{text}");
    assert!(text.contains("█"), "{text}");
    assert_snapshot("initiative_held", &text);
}

#[test]
fn refresh_re_reads_the_open_initiative_report() {
    let fake = fake_forge();
    let bin = fake._dir.path().join("forge");
    let replacement = fake._dir.path().join("forge.new");
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.open_initiative(7);
    let original = "the billing page shows usage-based line items";
    assert!(frame(&app).contains(original));

    for outcome in ["UPDATED OUTCOME", "SNAPSHOT OUTCOME"] {
        std::fs::write(&replacement, FAKE.replace(original, outcome)).unwrap();
        std::fs::set_permissions(&replacement, std::fs::metadata(&bin).unwrap().permissions())
            .unwrap();
        std::fs::rename(&replacement, &bin).unwrap();
        if outcome == "UPDATED OUTCOME" {
            app.refresh();
        } else {
            app.snapshot();
        }
        assert!(frame(&app).contains(outcome));
        assert_eq!(app.screen(), Screen::Initiative);
    }

    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.refresh();
    assert_eq!(app.screen(), Screen::Initiatives);
    app.snapshot();
    assert_eq!(app.screen(), Screen::Initiatives);
    app.shutdown();
}

#[test]
fn the_budget_and_stop_after_keys_call_initiative_set() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.open_initiative(7);
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    for c in "25".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let typing = frame(&app);
    assert!(
        typing.contains("Budget USD for initiative 7: 25"),
        "{typing}"
    );
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let after = frame(&app);
    app.shutdown();
    assert!(after.contains("initiative 7 updated"), "{after}");
}

#[test]
fn the_jobs_list_renders_every_job() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Jobs);
    let text = frame(&app);
    app.shutdown();
    assert_snapshot("jobs_list", &text);
}

#[test]
fn a_job_view_renders_its_steps_and_effects() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    app.open_job(21);
    assert_eq!(app.screen(), Screen::JobView);
    let text = frame(&app);
    app.shutdown();
    assert_snapshot("job_view", &text);
}

#[test]
fn the_jobs_list_and_view_render_in_utc_when_the_zone_is_utc_or_unknown() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    for _ in 0..3 {
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(app.screen(), Screen::Jobs);
    let list = frame_in(&app, Some("UTC"));
    assert_snapshot("jobs_list_utc", &list);
    app.open_job(21);
    let view = frame_in(&app, Some("UTC"));
    assert_snapshot("job_view_utc", &view);
    app.shutdown();
    // A zone the system cannot place is UTC, not an error and not a guess.
    assert_eq!(frame_in(&app, Some("Nowhere/Nothing")), view);
}

#[test]
fn a_time_follows_the_local_zone_and_its_daylight_rule() {
    use forge_tui::time::fmt_time;
    // 2026-09-21 14:13:20 UTC and 2027-01-15 08:00:00 UTC.
    let (summer, winter) = (1_790_000_000, 1_800_000_000);
    let eastern = Some("EST5EDT,M3.2.0,M11.1.0");
    under_tz(eastern, || {
        assert_eq!(fmt_time(summer), "2026-09-21 10:13");
        assert_eq!(fmt_time(winter), "2027-01-15 03:00");
    });
    under_tz(Some(LOCAL_TZ), || {
        assert_eq!(fmt_time(summer), "2026-09-21 19:43");
        assert_eq!(fmt_time(winter), "2027-01-15 13:30");
    });
    under_tz(Some("UTC"), || {
        assert_eq!(fmt_time(summer), "2026-09-21 14:13");
        assert_eq!(fmt_time(winter), "2027-01-15 08:00");
    });
}

/// A scripted interaction: down, enter, back — a key event at a time,
/// snapshotting the screen it produces after each one.
#[test]
fn a_scripted_interaction_moves_down_opens_a_task_and_goes_back() {
    let fake = fake_forge();
    let mut app = App::new(fake.forge);
    app.snapshot();
    assert_snapshot("scripted_1_queue", &frame(&app));

    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_snapshot("scripted_2_queue_down", &frame(&app));

    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Task);
    assert_snapshot("scripted_3_task", &frame(&app));

    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Queue);
    assert_snapshot("scripted_4_back_to_queue", &frame(&app));

    app.shutdown();
}

fn inbox_fake() -> Fake {
    let fake = fake_forge();
    let script = FAKE.replace("  log | requests) echo '[]' ;;", r#"
  log) echo '[{"id":44,"state":"unverified","workflow":"direct","attempts":1,"cost_usd":0.1,"repo":"/r","text":"Ready to land","task":"Ready to land","created_at":1,"created":"","trust":"operator"}]' ;;
  requests) cat "$(dirname "$0")/requests.json" ;;
  answer | withdraw | land)
    printf '%s\n' "$@" > "$(dirname "$0")/action"
    if [ -f "$(dirname "$0")/fail" ]; then echo refused >&2; exit 1; fi
    echo done ;;
"#).replace(r#""reason":"","verdict":[]"#, r#""reason":"fallback summary","outputs":{"summary":"Checked the settings; need a product decision."},"verdict":[]"#);
    std::fs::write(&fake.forge.bin, script).unwrap();
    std::fs::write(
        fake._dir.path().join("requests.json"),
        include_str!("fixtures/inbox.json"),
    )
    .unwrap();
    fake
}

#[test]
fn inbox_lists_two_questions_and_supports_cli_actions() {
    let fake = inbox_fake();
    let mut app = App::new(fake.forge.clone());
    app.refresh();
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_snapshot("inbox", &frame(&app));
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!fake._dir.path().join("action").exists());
    for c in "Use \"blue\"; $HOME".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_snapshot("inbox_answer", &frame(&app));
    // Refreshing while typing must not redirect the answer to another task.
    app.apply(serde_json::from_str(r#"{"type":"task_blocked"}"#).unwrap());
    app.pump();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(fake._dir.path().join("action")).unwrap(),
        "answer\n41\nUse \"blue\"; $HOME\n"
    );
    app.down();
    app.handle_key(KeyCode::Char('w'), KeyModifiers::NONE);
    for c in "Superseded".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    std::fs::write(fake._dir.path().join("fail"), "").unwrap();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(frame(&app).contains("refused"));
    std::fs::remove_file(fake._dir.path().join("fail")).unwrap();
    assert_eq!(
        std::fs::read_to_string(fake._dir.path().join("action")).unwrap(),
        "withdraw\n42\n--reason\nSuperseded\n"
    );
    app.down();
    app.down();
    app.down();
    app.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(fake._dir.path().join("action")).unwrap(),
        "land\n44\n"
    );
    std::fs::write(fake._dir.path().join("requests.json"), "[]").unwrap();
    app.apply(serde_json::from_str(r#"{"type":"task_blocked"}"#).unwrap());
    app.pump();
    assert!(!frame(&app).contains("Which color"));
    std::fs::write(
        fake._dir.path().join("requests.json"),
        include_str!("fixtures/inbox.json"),
    )
    .unwrap();
    app.apply(
        serde_json::from_str(r#"{"type":"task_done","task":41,"state":"blocked","text":""}"#)
            .unwrap(),
    );
    app.pump();
    assert!(frame(&app).contains("Which color"));
    app.shutdown();
}

/// Opens the stats screen and steps `tab` tabs right, then presses `keys`.
fn stats_frame(fake: Fake, tab: usize, keys: &[KeyCode]) -> String {
    let mut app = App::new(fake.forge);
    app.snapshot();
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(app.screen(), Screen::Stats);
    for _ in 0..tab {
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    }
    for key in keys {
        app.handle_key(*key, KeyModifiers::NONE);
    }
    let text = frame(&app);
    app.shutdown();
    text
}

#[test]
fn the_stats_workflows_tab_draws_the_rate_interval_as_a_bar() {
    let text = stats_frame(fake_forge(), 0, &[]);
    assert!(text.contains("REGRESSION"));
    assert_snapshot("stats_workflows", &text);
}

#[test]
fn the_stats_quality_tab_renders() {
    assert_snapshot("stats_quality", &stats_frame(fake_forge(), 1, &[]));
}

#[test]
fn the_stats_by_role_tab_renders() {
    assert_snapshot("stats_by_role", &stats_frame(fake_forge(), 2, &[]));
}

#[test]
fn the_stats_human_attention_tab_renders() {
    assert_snapshot("stats_human_attention", &stats_frame(fake_forge(), 3, &[]));
}

#[test]
fn the_stats_time_to_live_tab_renders() {
    assert_snapshot("stats_time_to_live", &stats_frame(fake_forge(), 4, &[]));
}

#[test]
fn the_stats_factors_tab_renders() {
    assert_snapshot("stats_factors", &stats_frame(fake_forge(), 5, &[]));
}

#[test]
fn a_stats_table_sorts_by_a_key_and_reverses() {
    let by_landed = |keys: &[KeyCode]| {
        let text = stats_frame(fake_forge(), 0, keys);
        let at = |name: &str| text.find(name).unwrap();
        (at("tdd"), at("direct"), text)
    };
    // 11 steps of `>` reach the landed column (11th of 12).
    let mut keys = vec![KeyCode::Char('>'); 11];
    let (tdd, direct, text) = by_landed(&keys);
    assert!(tdd < direct, "landed sorts descending first:\n{text}");
    assert!(text.contains("landed ▼"));
    keys.push(KeyCode::Char('v'));
    let (tdd, direct, text) = by_landed(&keys);
    assert!(direct < tdd, "v reverses:\n{text}");
    assert!(text.contains("landed ▲"));
    assert_snapshot("stats_workflows_sorted", &text);
}

#[test]
fn the_factors_tab_is_hidden_when_the_field_is_absent() {
    let mut doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/stats.json")).unwrap();
    doc.as_object_mut().unwrap().remove("factors");
    let fake = fake_forge_with_stats(&doc.to_string());
    let text = stats_frame(fake, 0, &[]);
    assert!(!text.contains("Factors"));
    assert!(text.contains("Time to live"));
    let fake = fake_forge_with_stats(&doc.to_string());
    let stepped = stats_frame(fake, 5, &[]);
    assert!(
        stepped.contains("Workflows"),
        "five tabs wrap back to the first"
    );
    assert_snapshot("stats_no_factors", &text);
}

/// Opens the screen `tabs` Tab presses from the queue, then presses `keys`.
fn ops_app(fake: &Fake, tabs: usize, keys: &[KeyCode]) -> App {
    let mut app = App::new(fake.forge.clone());
    app.snapshot();
    for _ in 0..tabs {
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    }
    for key in keys {
        app.handle_key(*key, KeyModifiers::NONE);
    }
    app
}

#[test]
fn the_deploys_screen_lists_targets_with_verdicts_and_the_selected_log() {
    let fake = fake_forge();
    let mut app = ops_app(&fake, 5, &[]);
    assert_eq!(app.screen(), Screen::Deploys);
    let text = frame(&app);
    assert!(text.contains("look FAILED") && text.contains("never deployed"));
    assert!(text.contains("rolled back to aaaa1111"));
    assert_snapshot("deploys", &text);
    app.shutdown();
}

#[test]
fn deploy_now_asks_first_and_only_y_runs_it() {
    let fake = fake_forge();
    let ran = fake._dir.path().join("ran");
    let mut app = ops_app(&fake, 5, &[KeyCode::Char('d')]);
    let text = frame(&app);
    assert!(text.contains("Deploy forge/prod now? y confirm  n cancel"));
    assert_snapshot("deploys_confirm", &text);
    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!ran.exists(), "n must not deploy");
    app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(
        std::fs::read_to_string(&ran).unwrap(),
        "deploy: forge prod\n"
    );
    assert!(frame(&app).contains("deployed forge/prod"));
    app.shutdown();
}

#[test]
fn j_moves_the_deploy_selection_to_the_next_targets_log() {
    let fake = fake_forge();
    let mut app = ops_app(&fake, 5, &[KeyCode::Char('j')]);
    let text = frame(&app);
    assert!(text.contains("log: forge/staging") && text.contains("no deploys yet"));
    app.shutdown();
}

#[test]
fn the_doctor_screen_shows_checks_fixes_worktrees_windows_and_spend() {
    let fake = fake_forge();
    let mut app = ops_app(&fake, 6, &[]);
    assert_eq!(app.screen(), Screen::Doctor);
    let text = frame(&app);
    assert!(text.contains("fix: start it with forge worker"));
    assert!(text.contains("5h 82%") && text.contains("$4.20 of $20.00"));
    assert_snapshot("doctor", &text);
    app.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(frame(&app).contains("removed 3 worktrees"));
    app.shutdown();
}

fn events(lines: &[&str]) -> Vec<forge_client::Event> {
    lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_fixture_event_stream_renders_its_kinds() {
    let fake = fake_forge();
    let mut app = ops_app(&fake, 7, &[]);
    assert_eq!(app.screen(), Screen::Activity);
    for e in events(&[
        r#"{"type":"task_started","task":12,"ts":1790000000,"text":"started"}"#,
        r#"{"type":"attempt_started","task":12,"n":2,"of":3,"ts":1790000001,"text":"attempt 2 of 3"}"#,
        r#"{"type":"tool_call","task":12,"name":"Edit","ts":1790000002,"text":"Edit x"}"#,
        r#"{"type":"tool_call","task":12,"name":"Bash","ts":1790000003,"text":"Bash y"}"#,
        r#"{"type":"agent_done","task":12,"turns":9,"cost_usd":0.42,"ts":1790000004,"text":"done"}"#,
        r#"{"type":"check","task":11,"ts":1790000005,"text":"cargo test ok"}"#,
        r#"{"type":"pushed","task":11,"ts":1790000006,"text":"pushed forge/11"}"#,
        r#"{"type":"task_done","task":11,"state":"succeeded","ts":1790000007,"text":"task 11 succeeded"}"#,
        r#"{"type":"task_done","task":13,"state":"blocked","reason":"needs input: which tier?","ts":1790000008,"text":"task 13 blocked"}"#,
        r#"{"type":"job_finished","task":0,"job_id":4,"ts":1790000009,"text":"job 4 ok"}"#,
    ]) {
        app.apply(e);
    }
    let text = frame(&app);
    for kind in [
        "task started",
        "attempt started",
        "check",
        "pushed",
        "task done",
        "question",
        "job",
    ] {
        assert!(text.contains(kind), "missing {kind}");
    }
    assert!(text.contains("attempt 2 of 3"));
    assert_snapshot("activity_feed", &text);

    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    let text = frame(&app);
    assert!(text.contains("kind: check") && !text.contains("pushed forge/11"));
    app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(frame(&app).contains("project: forge"));
    app.shutdown();
}

#[test]
fn a_query_renders_the_verb_call_and_pages_by_before() {
    let fake = fake_forge();
    let script = FAKE.replace("  log | requests) echo '[]' ;;", &format!("{}  requests) echo '[]' ;;", r#"  log)
    echo "$@" >> "$(dirname "$0")/log_calls"
    case "$*" in
      *--before*) echo '[{"id":9,"state":"failed","workflow":"tdd","attempts":3,"cost_usd":2.5,"repo":"/r","text":"older broken thing","task":"older broken thing","created_at":1,"created":"","trust":"operator","project":"forge"}]' ;;
      *--grep*) echo '[{"id":10,"state":"failed","workflow":"tdd","attempts":3,"cost_usd":2.5,"repo":"/r","text":"broken parser","task":"broken parser","created_at":1,"created":"","trust":"operator","project":"forge"}]' ;;
      *) echo '[]' ;;
    esac ;;
"#));
    std::fs::write(&fake.forge.bin, script).unwrap();
    let mut app = App::new(fake.forge.clone());
    app.snapshot();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "state:failed project:forge broken parser".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let text = frame(&app);
    assert!(text.contains(
        "forge log --json --limit 60 --state failed --project forge --grep broken parser"
    ));
    assert!(text.contains("broken parser"));
    assert_snapshot("task_list_query", &text);
    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
    let calls = std::fs::read_to_string(fake._dir.path().join("log_calls")).unwrap();
    assert!(
        calls.contains("--grep broken parser --before 10"),
        "{calls}"
    );
    assert!(frame(&app).contains("older broken thing"));
    app.shutdown();
}
