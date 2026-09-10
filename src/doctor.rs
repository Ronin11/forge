//! `forge doctor`: is this machine able to run attempts, and is anything
//! stuck? Each check is OK, WARN, or FAIL with a hint. Exit 1 on any FAIL.

use crate::ctx::{Forge, Paths};
use crate::store::{MIGRATIONS, Store, TaskState};
use crate::{agent, config, sandbox, unix_now, worker};
use anyhow::Result;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

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

    match config::load_home(&paths.home) {
        Ok(c) => {
            let b = &c.budget;
            let present = |v: &[std::path::PathBuf]| v.iter().filter(|p| p.exists()).count();
            out.push(check(
                "config",
                Status::Ok,
                format!(
                    "per_task_usd {:.2}, per_day_usd {:.2}; sandbox ro {}/{} present, rw {}/{} present",
                    b.per_task_usd,
                    b.per_day_usd,
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

    let queued = store.queued_count()?;
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

    if let Ok(f) = Forge::open_with(paths, store) {
        let spent = f.store.spent_since(unix_now() - 86_400)?;
        out.push(if spent >= f.budget.per_day_usd {
            check(
                "spend",
                Status::Warn,
                format!(
                    "${spent:.2} of ${:.2} in the last 24h",
                    f.budget.per_day_usd
                ),
                "nothing new starts until the window rolls; raise per_day_usd to override",
            )
        } else {
            check(
                "spend",
                Status::Ok,
                format!(
                    "${spent:.2} of ${:.2} in the last 24h",
                    f.budget.per_day_usd
                ),
                "",
            )
        });
        out.push(match f.store.latest_rate_limit()? {
            None => check("rate_limit", Status::Warn, "no samples yet", "samples arrive with the first real attempt"),
            Some(s) => {
                let age = unix_now() - s.seen_at;
                let worst = s.five_hour.unwrap_or(0.0).max(s.seven_day.unwrap_or(0.0));
                let detail = format!(
                    "5h {}, 7d {} ({}m ago)",
                    s.five_hour.map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                    s.seven_day.map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                    age / 60
                );
                if worst >= 0.8 {
                    check("rate_limit", Status::Warn, detail, "the subscription window is nearly used; attempts will start failing with rate limits")
                } else {
                    check("rate_limit", Status::Ok, detail, "")
                }
            }
        });
    }
    Ok(out)
}
