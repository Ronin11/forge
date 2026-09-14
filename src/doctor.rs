//! `forge doctor`: is this machine able to run attempts, and is anything
//! stuck? Each check is OK, WARN, or FAIL with a hint. Exit 1 on any FAIL.

use crate::ctx::{Forge, Paths};
use crate::store::{MIGRATIONS, Store, TaskState};
use crate::{agent, config, sandbox, unix_now, worker, workflows};
use anyhow::Result;
use serde::Serialize;

#[derive(PartialEq, Eq, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub hint: String,
}

fn check(name: &str, status: Status, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
        hint: hint.into(),
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// The repomap blob cache: one small `.json` file per blob, nested under
/// two-character prefix directories (see `repomap::BlobCache`).
fn count_files(dir: &std::path::Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut size = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (count, size);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (c, s) = count_files(&entry.path());
            count += c;
            size += s;
        } else if meta.is_file() {
            count += 1;
            size += meta.len();
        }
    }
    (count, size)
}

/// A unix timestamp as a plain `YYYY-MM-DD`, with no timezone-database
/// dependency: Howard Hinnant's civil_from_days over UTC days.
fn ymd(unix_secs: i64) -> String {
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn binary(name: &str, required: bool, why_optional: &str) -> Check {
    match sandbox::resolve_binary(name) {
        Ok((_, path)) => check(
            &format!("binary.{name}"),
            Status::Ok,
            path.display().to_string(),
            "",
        ),
        Err(_) if required => check(
            &format!("binary.{name}"),
            Status::Fail,
            "not found on PATH",
            format!("install {name} and put it on PATH"),
        ),
        Err(_) => check(
            &format!("binary.{name}"),
            Status::Warn,
            "not found on PATH",
            why_optional,
        ),
    }
}

pub fn run() -> Result<Vec<Check>> {
    let mut out = Vec::new();
    let agent_bin = agent::agent_bin();
    out.push(binary(&agent_bin, true, ""));
    out.push(binary("git", true, ""));
    let sandbox_off = std::env::var("FORGE2_SANDBOX").as_deref() == Ok("0");
    out.push(match (sandbox::resolve_binary("bwrap"), sandbox_off) {
        (Ok((_, p)), false) => check(
            "sandbox",
            Status::Ok,
            format!("bwrap at {}", p.display()),
            "",
        ),
        (Ok(_), true) => check(
            "sandbox",
            Status::Warn,
            "FORGE2_SANDBOX=0: agents run on the host",
            "unset FORGE2_SANDBOX",
        ),
        (Err(_), true) => check(
            "sandbox",
            Status::Warn,
            "no bwrap and FORGE2_SANDBOX=0",
            "install bubblewrap",
        ),
        (Err(_), false) => check(
            "sandbox",
            Status::Fail,
            "bwrap not found",
            "install bubblewrap, or set FORGE2_SANDBOX=0 to run unsandboxed",
        ),
    });

    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            out.push(check(
                "home",
                Status::Fail,
                format!("{e:#}"),
                "set FORGE2_HOME to a writable directory",
            ));
            return Ok(out);
        }
    };
    let probe = paths.home.join(".doctor-write-probe");
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => out.push(check(
            "home",
            Status::Ok,
            paths.home.display().to_string(),
            "",
        )),
        Err(e) => out.push(check(
            "home",
            Status::Fail,
            format!("{}: not writable: {e}", paths.home.display()),
            "fix permissions or set FORGE2_HOME",
        )),
    }

    let cache_dir = paths.home.join("cache").join("repomap");
    out.push(if !cache_dir.exists() {
        check(
            "cache",
            Status::Warn,
            format!("{} does not exist", cache_dir.display()),
            "it is created on the first repomap run; nothing to do yet",
        )
    } else {
        let probe = cache_dir.join(".doctor-write-probe");
        match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
            Ok(()) => {
                let (count, size) = count_files(&cache_dir);
                check(
                    "cache",
                    Status::Ok,
                    format!("{count} blob file(s) totaling {}", human_bytes(size)),
                    "",
                )
            }
            Err(e) => check(
                "cache",
                Status::Warn,
                format!("{}: not writable: {e}", cache_dir.display()),
                "fix permissions on the repomap cache directory",
            ),
        }
    });

    match config::load_home(&paths.home) {
        Ok(c) => {
            let b = &c.budget;
            let present = |v: &[std::path::PathBuf]| v.iter().filter(|p| p.exists()).count();
            out.push(check(
                "config",
                Status::Ok,
                format!(
                    "windows 5h ≤ {:.0}% / 7d ≤ {:.0}%, per_task_usd {:.2}, per_day_usd {}; sandbox ro {}/{} present, rw {}/{} present",
                    b.five_hour_max * 100.0,
                    b.seven_day_max * 100.0,
                    b.per_task_usd,
                    b.per_day_usd.map_or("none".to_string(), |d| format!("{d:.2}")),
                    present(&c.sandbox.ro),
                    c.sandbox.ro.len(),
                    present(&c.sandbox.rw),
                    c.sandbox.rw.len()
                ),
                "",
            ))
        }
        Err(e) => out.push(check(
            "config",
            Status::Fail,
            format!("{e:#}"),
            format!("fix {}", paths.home.join("config.toml").display()),
        )),
    }

    let store = match Store::open(&paths.home.join("forge.db")) {
        Ok(s) => s,
        Err(e) => {
            out.push(check(
                "store",
                Status::Fail,
                format!("{e:#}"),
                "the database cannot be opened; back it up before anything else",
            ));
            return Ok(out);
        }
    };
    out.push(check(
        "schema",
        Status::Ok,
        format!("version {}", MIGRATIONS.len()),
        "",
    ));

    match workflows::check(&paths.home) {
        Ok(problems) => {
            let blocking = problems.iter().filter(|p| p.blocking).count();
            let n = workflows::load_all(&paths.home)
                .map(|w| w.len())
                .unwrap_or(0);
            let uncommitted = workflows::uncommitted(&paths.home).unwrap_or_default();
            let detail = format!(
                "{n} file(s), {blocking} blocking, {} warning(s), {} uncommitted",
                problems.len() - blocking,
                uncommitted.len()
            );
            out.push(if blocking > 0 {
                let first = problems.iter().find(|p| p.blocking).unwrap();
                check(
                    "workflows",
                    Status::Fail,
                    format!("{detail}: {} {}", first.file, first.what),
                    "fix the file; no task can be created while a workflow file is broken",
                )
            } else if !uncommitted.is_empty() || problems.len() > blocking {
                check(
                    "workflows",
                    Status::Warn,
                    detail,
                    "commit the workflows directory (it is a git repo) and fill in [meta]",
                )
            } else {
                check("workflows", Status::Ok, detail, "")
            });
        }
        Err(e) => out.push(check(
            "workflows",
            Status::Fail,
            format!("{e:#}"),
            "the workflows directory cannot be read",
        )),
    }

    // The lookback: workflows whose current version regressed against the
    // previous, and known workflows that are mostly failing.
    if let Ok(all) = workflows::load_all(&paths.home) {
        let mut bad: Vec<String> = Vec::new();
        let mut known = 0;
        for w in &all {
            let cur = crate::profile::profile(
                &store
                    .runs(&w.name, Some(&w.hash), crate::profile::LOOKBACK)
                    .unwrap_or_default(),
            );
            if cur.known {
                known += 1;
            }
            if let Some(prev_hash) = store
                .workflow_versions(&w.name)
                .unwrap_or_default()
                .into_iter()
                .find(|h| h != &w.hash)
            {
                let prev = crate::profile::profile(
                    &store
                        .runs(&w.name, Some(&prev_hash), crate::profile::LOOKBACK)
                        .unwrap_or_default(),
                );
                if crate::profile::regressed(&cur, &prev) {
                    bad.push(format!(
                        "{} regressed vs {} ({:.0}% vs {:.0}%)",
                        w.name,
                        &prev_hash[..8],
                        cur.rate * 100.0,
                        prev.rate * 100.0
                    ));
                }
            }
            if cur.known && cur.rate_hi < 0.5 {
                bad.push(format!(
                    "{} verifies {}/{} (95% upper {:.0}%)",
                    w.name,
                    cur.succeeded,
                    cur.n,
                    cur.rate_hi * 100.0
                ));
            }
        }
        out.push(if bad.is_empty() {
            check("learning", Status::Ok, format!("{known} of {} workflow(s) measured; no regressions", all.len()), "")
        } else {
            check("learning", Status::Warn, bad.join("; "), "revert the workflow or action file to the version with the good numbers, or retire the workflow")
        });
    }

    let queued = store.queued_count()?;
    // The worker, by its pid file: alive, and on the binary that is on disk.
    if let Ok(text) = std::fs::read_to_string(paths.home.join("worker.pid")) {
        let mut it = text.split_whitespace();
        let pid: i64 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let alive = pid > 0 && worker::pid_alive(pid);
        let stale = alive
            && std::fs::read_link(format!("/proc/{pid}/exe"))
                .map(|p| p.to_string_lossy().ends_with(" (deleted)"))
                .unwrap_or(false);
        out.push(match (alive, stale) {
            (true, true) => check("worker", Status::Warn, format!("pid {pid} runs a binary rebuilt since it started"), "restart the worker (one SIGTERM drains it, or systemctl --user restart forge2-worker)"),
            (true, false) => check("worker", Status::Ok, format!("pid {pid} running"), ""),
            (false, _) => check("worker", Status::Warn, format!("pid {pid} is gone"), "start it: forge work, or systemctl --user start forge2-worker"),
        });
    }
    let running = store.running_ids()?;
    let orphans: Vec<i64> = store.orphans(worker::pid_alive)?;
    out.push(match (running.len(), orphans.len()) {
        (_, o) if o > 0 => check(
            "queue",
            Status::Warn,
            format!(
                "{queued} queued, {} running, {o} left by a dead worker: {:?}",
                running.len(),
                orphans
            ),
            "forge work requeues them on start",
        ),
        (r, _) => check(
            "queue",
            Status::Ok,
            format!("{queued} queued, {r} running"),
            "",
        ),
    });

    let retained: Vec<i64> = store
        .tasks_with_worktrees()?
        .into_iter()
        .filter(|t| t.state != TaskState::Running && t.state != TaskState::Queued)
        .map(|t| t.id)
        .collect();
    out.push(if retained.is_empty() {
        check("worktrees", Status::Ok, "none retained", "")
    } else {
        check(
            "worktrees",
            Status::Warn,
            format!("{} retained: {:?}", retained.len(), retained),
            "forge gc removes the published ones and explains the rest",
        )
    });

    let events_size = std::fs::metadata(paths.home.join("events.jsonl"))
        .map(|m| m.len())
        .unwrap_or(0);
    let (mut attempt_count, mut attempt_size, mut oldest) = (0u64, 0u64, None::<i64>);
    if let Ok(entries) = std::fs::read_dir(&paths.logs) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            attempt_count += 1;
            attempt_size += meta.len();
            if let Ok(secs) = meta
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .duration_since(std::time::UNIX_EPOCH)
            {
                let secs = secs.as_secs() as i64;
                oldest = Some(oldest.map_or(secs, |o: i64| o.min(secs)));
            }
        }
    }
    let total = events_size + attempt_size;
    let detail = format!(
        "events.jsonl {}; {attempt_count} attempt log(s) totaling {}{}",
        human_bytes(events_size),
        human_bytes(attempt_size),
        oldest.map_or(String::new(), |o| format!(", oldest {}", ymd(o))),
    );
    const GIB: u64 = 1024 * 1024 * 1024;
    out.push(if total >= GIB {
        check(
            "logs",
            Status::Warn,
            detail,
            format!(
                "{} of logs on disk; archive or delete old attempt logs under {} by hand",
                human_bytes(total),
                paths.logs.display()
            ),
        )
    } else {
        check("logs", Status::Ok, detail, "")
    });

    if let Ok(f) = Forge::open_with(paths, store) {
        let spent = f.store.spent_since(unix_now() - 86_400)?;
        out.push(match f.budget.per_day_usd {
            Some(cap) if spent >= cap => check(
                "spend",
                Status::Warn,
                format!("${spent:.2} of ${cap:.2} in the last 24h"),
                "nothing new starts until the window rolls; raise per_day_usd to override",
            ),
            Some(cap) => check(
                "spend",
                Status::Ok,
                format!("${spent:.2} of ${cap:.2} in the last 24h"),
                "",
            ),
            None => check(
                "spend",
                Status::Ok,
                format!(
                    "${spent:.2} in the last 24h (no dollar cap; the rate windows are the limit)"
                ),
                "",
            ),
        });
        out.push(match f.store.latest_rate_limit()? {
            None => check(
                "rate_limit",
                Status::Warn,
                "no samples yet",
                "samples arrive with the first real attempt",
            ),
            Some(s) => {
                let age = unix_now() - s.seen_at;
                let worst = s.five_hour.unwrap_or(0.0).max(s.seven_day.unwrap_or(0.0));
                let detail = format!(
                    "5h {}, 7d {} ({}m ago)",
                    s.five_hour
                        .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                    s.seven_day
                        .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                    age / 60
                );
                match crate::worker::window_hold(&f)? {
                    Some((msg, _)) => check(
                        "rate_limit",
                        Status::Warn,
                        format!("{detail}; {msg}"),
                        "the worker holds until the reset, then continues",
                    ),
                    None if worst >= 0.8 => check(
                        "rate_limit",
                        Status::Warn,
                        detail,
                        "a window is nearly at its cap; the worker will hold when it reaches it",
                    ),
                    None => check("rate_limit", Status::Ok, detail, ""),
                }
            }
        });
    }
    Ok(out)
}
