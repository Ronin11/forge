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
{"events_offset":0,"tasks":[{"id":12,"state":"running","workflow":"tdd","attempts":2,"cost_usd":1.35,"project":"forge","task":"add snapshot tests for the tui"},{"id":11,"state":"succeeded","workflow":"direct","attempts":1,"cost_usd":0.1,"project":"forge","task":"fix typo in docs"}],"requests":[],"worker":{"running":true,"pid":555,"exe":"","stale_binary":false}}
JSON
  ;;
  events) exit 0 ;;
  initiative)
    case "$2" in
      list) cat <<'JSON'
[{"id":3,"project":"forge","outcome":"cover the tui with text-snapshot tests","state":"open","held_rule":null,"queued":1,"running":1,"succeeded":3,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":4.2,"budget_usd":null,"stop_after_same_rule":3,"created_at":1,"settled_at":null},{"id":2,"project":"forge","outcome":"an older, settled initiative","state":"done","held_rule":null,"queued":0,"running":0,"succeeded":5,"failed":0,"unverified":0,"blocked":0,"withdrawn":0,"cost_usd":9.9,"budget_usd":20.0,"stop_after_same_rule":3,"created_at":1,"settled_at":2}]
JSON
      ;;
      *) echo "unexpected initiative: $*" >&2; exit 2 ;;
    esac ;;
  trace)
    id="$2"
    cat <<JSON
{"task":{"id":$id,"state":"succeeded","workflow":"tdd","reason":"","branch":"forge/$id-x","base_sha":"abcdef1234567890","project":"forge","initiative":3,"after":[],"retry_of":null,"text":"add snapshot tests for the tui"},"attempts":[{"attempt_no":1,"step":"code","state":"succeeded","num_turns":9,"cost_usd":0.42,"reason":"","verdict":[]}],"ops":[{"name":"clone","ok":true,"detail":"ok"},{"name":"verify","ok":true,"detail":"ok"}],"deploys":[{"id":1,"project":"forge","target":"prod","sha":"abcdef1234567890","started_at":1,"finished_at":2,"check_ok":true,"check_output":"ok","rolled_back_to":null,"reason":""}],"assessment":{"score":82,"findings":[{"path":"tui/src/lib.rs","finding":"missing coverage of the initiative screen","severity":"minor"}],"model":"claude-sonnet-5","provider":"anthropic","cost_usd":0.05,"created_at":1}}
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
{"id":$id,"project":"forge","workflow":"nightly-cleanup","workflow_hash":"h1","landed_sha":"deadbeef","trigger_kind":"schedule","trigger_ref":"0 3 * * *","state":"ok","workflow_source":"repo","dry_run":false,"started_at":1790000000,"finished_at":1790000050,"cost_usd":0.12,"verdict_json":"[]","due_at":1790003600,"steps":[{"id":1,"job_id":$id,"seq":1,"action":"run-checks","kind":"directive","provider":"anthropic","model":"claude-sonnet-5","cost_usd":0.05,"started_at":1000,"finished_at":1020,"exit_code":0,"output_ref":""},{"id":2,"job_id":$id,"seq":2,"action":"notify","kind":"shell","provider":"","model":"","cost_usd":null,"started_at":1020,"finished_at":1050,"exit_code":0,"output_ref":""}],"effects":[{"id":1,"job_id":$id,"seq":1,"kind":"message","target":"ops-channel","summary":"posted the nightly summary","dry_run":false},{"id":2,"job_id":$id,"seq":2,"kind":"file","target":"reports/nightly.md","summary":"wrote the report","dry_run":false}]}
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
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("forge");
    std::fs::write(&bin, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let forge = Forge {
        bin: bin.to_string_lossy().into_owned(),
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
