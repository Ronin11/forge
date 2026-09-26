//! `forge doctor`: is this machine able to run attempts, and is anything
//! stuck? Each check is OK, WARN, or FAIL with a hint. Exit 1 on any FAIL.

use crate::ctx::{Forge, Paths};
use crate::store::{Store, TaskState};
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

/// Beyond `name`/`status`/`detail`/`hint` (the CLI's own text rendering),
/// a handful of checks carry the same numbers structured, so a client can
/// draw a gauge instead of parsing prose: `rate_limit` sets `provider` and
/// the window fields, `spend` sets `spend_usd`/`spend_cap_usd`, `queue`
/// sets `queued`/`running`, `worktrees` sets `worktree_ids`. `forge doctor
/// --json` is explicitly not part of the stable contract (docs/CLIENT.md),
/// so these are additive and every other check simply leaves them `None`.
#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_resets_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_cap_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<i64>,
    /// `worktrees` sets this to the retained tasks' ids, the same list
    /// its prose detail already names, so a client can draw a gc control
    /// per id without parsing `Vec<i64>`'s `{:?}` out of the text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_ids: Option<Vec<i64>>,
}

fn check(name: &str, status: Status, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
        hint: hint.into(),
        provider: None,
        five_hour_pct: None,
        five_hour_resets_at: None,
        seven_day_pct: None,
        seven_day_resets_at: None,
        spend_usd: None,
        spend_cap_usd: None,
        queued: None,
        running: None,
        worktree_ids: None,
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

/// The agent, git, and the sandbox launcher.
fn check_binaries() -> Vec<Check> {
    let mut out = Vec::new();
    let agent_bin = agent::agent_bin();
    out.push(binary(&agent_bin, true, ""));
    out.push(binary("git", true, ""));
    let sandbox_off = config::env("SANDBOX").as_deref() == Ok("0");
    out.push(match (sandbox::resolve_binary("bwrap"), sandbox_off) {
        (Ok((_, p)), false) => match sandbox::bwrap_version(&p) {
            Some(v) if !sandbox::version_has_overlay(Some(v.clone())) => {
                check(
                    "sandbox",
                    Status::Warn,
                    format!(
                        "package caches are not shared with attempts: bwrap {v} has no overlay support; bwrap at {}",
                        p.display()
                    ),
                    "install bubblewrap >= 0.10",
                )
            }
            Some(v) => check(
                "sandbox",
                Status::Ok,
                format!("bwrap {v} at {}", p.display()),
                "",
            ),
            None => check(
                "sandbox",
                Status::Warn,
                format!(
                    "package caches are not shared with attempts: bwrap version unknown; bwrap at {}",
                    p.display()
                ),
                "install bubblewrap >= 0.10",
            ),
        },
        (Ok(_), true) => check(
            "sandbox",
            Status::Warn,
            "FORGE_SANDBOX=0: agents run on the host",
            "unset FORGE_SANDBOX",
        ),
        (Err(_), true) => check(
            "sandbox",
            Status::Warn,
            "no bwrap and FORGE_SANDBOX=0",
            "install bubblewrap",
        ),
        (Err(_), false) => check(
            "sandbox",
            Status::Warn,
            "bwrap not found: attempts run on the host backend, with no egress bound and no private home",
            "install bubblewrap (Linux) to sandbox attempts",
        ),
    });
    out
}

/// What an attempt can reach: whether bwrap can give it a network namespace
/// at all, the model endpoints that are always allowed, and each project's
/// declared `[sandbox] egress`. Unsandboxed, none of it is enforced.
fn check_egress(paths: &Paths, store: &Store) -> Vec<Check> {
    let mut out = Vec::new();
    let model: Vec<String> = match config::load_home(&paths.home) {
        Ok(c) => crate::egress::model_rules(&c.providers)
            .iter()
            .map(|r| r.to_string())
            .collect(),
        // `check_config` reports a config that does not load.
        Err(_) => Vec::new(),
    };
    let sandbox_off = config::env("SANDBOX").as_deref() == Ok("0");
    out.push(if sandbox_off {
        check(
            "egress",
            Status::Warn,
            "FORGE_SANDBOX=0: attempts have the host's network; no egress policy is enforced",
            "unset FORGE_SANDBOX",
        )
    } else if sandbox::resolve_binary("bwrap").is_err() {
        check(
            "egress",
            Status::Warn,
            "bwrap not found: attempts have the host's network; no egress policy is enforced",
            "install bubblewrap (Linux) to bound egress",
        )
    } else {
        match std::process::Command::new("bwrap")
            .args(["--unshare-net", "--ro-bind", "/", "/", "--dev", "/dev", "true"])
            .output()
        {
            Ok(o) if o.status.success() => check(
                "egress",
                Status::Ok,
                format!(
                    "attempts get a network namespace; the model endpoint is always allowed ({})",
                    model.join(", ")
                ),
                "",
            ),
            Ok(o) => check(
                "egress",
                Status::Fail,
                format!(
                    "bwrap cannot create a network namespace: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                "attempts would not start; enable unprivileged user namespaces, or set FORGE_SANDBOX=0 to run unsandboxed",
            ),
            Err(e) => check("egress", Status::Fail, format!("running bwrap: {e}"), ""),
        }
    });
    if let Some(refused) = refused_lately(store)
        && let Some(row) = out.last_mut()
    {
        row.detail
            .push_str(&format!("; refused in the last 24h: {refused}"));
    }
    if let Ok(c) = config::load_home(&paths.home) {
        let model_only = [&c.trust.operator, &c.trust.contact, &c.trust.public]
            .iter()
            .any(|p| p.egress == config::TrustEgress::Model);
        out.push(match &c.sandbox.dependency_cache {
            Some(d) if d.is_dir() => check(
                "dependency_cache",
                Status::Ok,
                format!("{} is bound read-only into attempts", d.display()),
                "",
            ),
            Some(d) => check(
                "dependency_cache",
                Status::Warn,
                format!("{} does not exist", d.display()),
                "warm it with the repository's dependencies; public work installs from it",
            ),
            None if model_only => check(
                "dependency_cache",
                Status::Warn,
                "no [sandbox] dependency_cache: public work reaches no registry, so its setup check would fail without a cache",
                "set dependency_cache in config.toml to a directory warmed with the repository's dependencies",
            ),
            None => check("dependency_cache", Status::Ok, "not configured", ""),
        });
    }
    let projects = match store.list_projects() {
        Ok(p) => p,
        Err(_) => return out,
    };
    for p in projects {
        let repos = store.project_repos(&p.name).unwrap_or_default();
        let mut allowed = Vec::new();
        let mut broken = None;
        for r in &repos {
            match config::load_working_egress(std::path::Path::new(&r.repo)) {
                Ok(rules) => allowed.extend(rules.iter().map(|r| r.to_string())),
                Err(e) => broken = Some(format!("{}: {e:#}", r.repo)),
            }
        }
        allowed.sort();
        allowed.dedup();
        out.push(match broken {
            Some(e) => check(
                &format!("egress.{}", p.name),
                Status::Warn,
                e,
                "fix [sandbox] egress in the repository's forge.toml",
            ),
            None if allowed.is_empty() => check(
                &format!("egress.{}", p.name),
                if sandbox_off { Status::Warn } else { Status::Ok },
                "the model endpoint only",
                "a repository whose checks install packages declares its registries: [sandbox] egress = [\"registry.npmjs.org\"]",
            ),
            None => check(
                &format!("egress.{}", p.name),
                if sandbox_off { Status::Warn } else { Status::Ok },
                format!("the model endpoint and {}", allowed.join(", ")),
                "",
            ),
        });
    }
    out
}

/// What the egress proxy refused attempts in the last day, by host and
/// repository (`host xN (repository)`), most refused first: what the
/// `[environment]` table and a repository's `[sandbox] egress` are tuned
/// from. `None` when nothing was refused.
fn refused_lately(store: &Store) -> Option<String> {
    let since = crate::unix_now() - 24 * 3600;
    let mut counts: std::collections::BTreeMap<(String, String), u64> = Default::default();
    for (repo, outputs) in store.attempt_outputs_since(since).ok()? {
        let Ok(o) = serde_json::from_str::<crate::audit::Outputs>(&outputs) else {
            continue;
        };
        for r in o.refused {
            *counts.entry((r.host, repo.clone())).or_default() += r.count;
        }
    }
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    (!rows.is_empty()).then(|| {
        rows.iter()
            .map(|((host, repo), n)| format!("{host} x{n} ({repo})"))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Whether FORGE_HOME is writable, once it has already been resolved.
fn check_home(paths: &Paths) -> Vec<Check> {
    let probe = paths.home.join(".doctor-write-probe");
    vec![
        match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
            Ok(()) => check("home", Status::Ok, paths.home.display().to_string(), ""),
            Err(e) => check(
                "home",
                Status::Fail,
                format!("{}: not writable: {e}", paths.home.display()),
                "fix permissions or set FORGE_HOME",
            ),
        },
    ]
}

/// Every `FORGE2_*` variable still set: the rename to `FORGE_*` is one
/// release old (`config::env` reads the new name first, the old one as a
/// fallback), so this names each old one still set and says what it is now.
fn check_legacy_env() -> Vec<Check> {
    let old = config::old_env_vars_set();
    if old.is_empty() {
        return Vec::new();
    }
    let renamed: Vec<String> = old
        .iter()
        .map(|k| format!("{k} -> FORGE_{}", &k["FORGE2_".len()..]))
        .collect();
    vec![check(
        "legacy_env",
        Status::Warn,
        format!("still set, read for now: {}", old.join(", ")),
        format!("rename: {}", renamed.join(", ")),
    )]
}

/// When nothing names a home explicitly and `Paths::resolve` fell back to
/// the pre-rename default (`~/.local/share/forge2`) because the new one
/// does not exist yet: the exact `mv` and the unit lines the operator
/// changes to make the new default permanent.
fn check_home_migration() -> Vec<Check> {
    let Some((new, old)) = crate::ctx::legacy_home_migration() else {
        return Vec::new();
    };
    vec![check(
        "home_migration",
        Status::Warn,
        format!(
            "using the old data directory {} ({} does not exist yet)",
            old.display(),
            new.display()
        ),
        format!(
            "mv {} {}; then in ~/.config/systemd/user/{{forge-worker,forge-web,forge-portal}}.service change Environment=FORGE_HOME=%h/.local/share/forge2 to Environment=FORGE_HOME=%h/.local/share/forge and run systemctl --user daemon-reload",
            old.display(),
            new.display(),
        ),
    )]
}

/// Every repository's own repomap cache (`ctx::Forge::cache_dir`, private
/// per repository so one cannot poison what another reads), each a
/// `<hash>/repomap` directory directly under `paths.home/cache`.
fn repo_repomap_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().join("repomap"))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn check_cache(paths: &Paths) -> Vec<Check> {
    let root = paths.home.join("cache");
    let repo_caches = repo_repomap_dirs(&root);
    vec![if repo_caches.is_empty() {
        check(
            "cache",
            Status::Warn,
            format!("{} does not exist", root.join("<repo>/repomap").display()),
            "it is created on the first repomap run; nothing to do yet",
        )
    } else {
        let probe = repo_caches[0].join(".doctor-write-probe");
        match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
            Ok(()) => {
                let (count, size) = repo_caches
                    .iter()
                    .map(|d| count_files(d))
                    .fold((0, 0), |(c, s), (fc, fs)| (c + fc, s + fs));
                check(
                    "cache",
                    Status::Ok,
                    format!(
                        "{count} blob file(s) totaling {} across {} repositor{}",
                        human_bytes(size),
                        repo_caches.len(),
                        if repo_caches.len() == 1 { "y" } else { "ies" }
                    ),
                    "",
                )
            }
            Err(e) => check(
                "cache",
                Status::Warn,
                format!("{}: not writable: {e}", repo_caches[0].display()),
                "fix permissions on the repomap cache directory",
            ),
        }
    }]
}

fn check_config(paths: &Paths) -> Vec<Check> {
    vec![match config::load_home(&paths.home) {
        Ok(c) => {
            let b = &c.budget;
            let present = |v: &[std::path::PathBuf]| v.iter().filter(|p| p.exists()).count();
            check(
                "config",
                Status::Ok,
                format!(
                    "windows 5h ≤ {:.0}% / 7d ≤ {:.0}%, per_task_usd {:.2}, per_day_usd {}; sandbox ro {}/{} present, rw {}/{} present",
                    b.five_hour_max * 100.0,
                    b.seven_day_max * 100.0,
                    b.per_task_usd,
                    b.per_day_usd
                        .map_or("none".to_string(), |d| format!("{d:.2}")),
                    present(&c.sandbox.ro),
                    c.sandbox.ro.len(),
                    present(&c.sandbox.rw),
                    c.sandbox.rw.len()
                ),
                "",
            )
        }
        Err(e) => check(
            "config",
            Status::Fail,
            format!("{e:#}"),
            format!("fix {}", paths.home.join("config.toml").display()),
        ),
    }]
}

/// The database's real schema version, from `PRAGMA user_version`, not
/// how many migrations this binary happens to ship.
fn check_schema(store: &Store) -> Vec<Check> {
    vec![match store.schema_version() {
        Ok(v) => check("schema", Status::Ok, format!("version {v}"), ""),
        Err(e) => check("schema", Status::Fail, format!("{e:#}"), ""),
    }]
}

fn check_workflows(paths: &Paths) -> Vec<Check> {
    vec![match workflows::check(&paths.home) {
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
            if blocking > 0 {
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
            }
        }
        Err(e) => check(
            "workflows",
            Status::Fail,
            format!("{e:#}"),
            "the workflows directory cannot be read",
        ),
    }]
}

/// Every plugin found across `<FORGE_HOME>/plugins` and the configured
/// `plugin_dirs`, any problem loading one (a broken `plugin.toml`, a
/// shadowed name, a configured root that does not exist), and each enabled
/// plugin's last-known supervision state (running, restarting, or stopped;
/// see `crate::plugins::Supervisor`). Never fails: a broken or crash-looping
/// plugin is a warning, not a reason to fail doctor.
fn check_plugins(paths: &Paths, store: &Store) -> Vec<Check> {
    let cfg = match config::load_home(&paths.home) {
        Ok(c) => c,
        Err(e) => return vec![check("plugins", Status::Fail, format!("{e:#}"), "")],
    };
    let cat = crate::plugins::load_catalog(&paths.home, &cfg.plugin_dirs);
    let mut detail = format!(
        "{} plugin(s) found, {} problem(s)",
        cat.plugins.len(),
        cat.problems.len()
    );
    if let Ok(enabled) = store.enabled_plugins()
        && !enabled.is_empty()
    {
        let states: Vec<String> = enabled
            .iter()
            .map(|name| {
                format!(
                    "{name} ({})",
                    crate::plugins::read_run_state(&paths.home, name).describe()
                )
            })
            .collect();
        detail = format!("{detail}; enabled: {}", states.join(", "));
    }
    vec![if cat.problems.is_empty() {
        check("plugins", Status::Ok, detail, "")
    } else {
        let first = &cat.problems[0];
        check(
            "plugins",
            Status::Warn,
            format!("{detail}: {} {}", first.file, first.what),
            "fix the plugin directory or its plugin.toml; other plugins still load",
        )
    }]
}

/// The lookback: workflows whose current version regressed against the
/// previous, and known workflows that are mostly failing. Each is measured
/// per (workflow, provider) and warned about per pair, naming the provider:
/// a workflow that lands on one provider and never on another is not
/// failing on average.
fn check_learning(paths: &Paths, store: &Store) -> Vec<Check> {
    let Ok(all) = workflows::load_all(&paths.home) else {
        return Vec::new();
    };
    let mut bad: Vec<String> = Vec::new();
    let mut known = 0;
    for w in &all {
        let by_provider =
            crate::profile::measure_by_provider(store, &w.name, &w.hash).unwrap_or_default();
        if by_provider.iter().any(|(_, m)| m.current.known) {
            known += 1;
        }
        for (provider, m) in &by_provider {
            let cur = &m.current;
            if let Some((prev_hash, prev)) = &m.previous
                && m.regressed
            {
                bad.push(format!(
                    "{} regressed on {provider} vs {} ({:.0}% vs {:.0}%)",
                    w.name,
                    &prev_hash[..8],
                    cur.rate * 100.0,
                    prev.rate * 100.0
                ));
            }
            if cur.known && cur.rate_hi < 0.5 {
                bad.push(format!(
                    "{} verifies {}/{} on {provider} (95% upper {:.0}%)",
                    w.name,
                    cur.succeeded,
                    cur.n,
                    cur.rate_hi * 100.0
                ));
            }
        }
    }
    vec![if bad.is_empty() {
        check(
            "learning",
            Status::Ok,
            format!(
                "{known} of {} workflow(s) measured; no regressions",
                all.len()
            ),
            "",
        )
    } else {
        check(
            "learning",
            Status::Warn,
            bad.join("; "),
            "revert the workflow or action file to the version with the good numbers, or retire the workflow",
        )
    }]
}

/// The worker, by its pid file: alive, and on the binary that is on disk.
fn check_worker(paths: &Paths) -> Vec<Check> {
    let Some(w) = worker::worker_status(paths) else {
        return Vec::new();
    };
    vec![match (w.running, w.stale) {
        (true, Some(true)) => check(
            "worker",
            Status::Warn,
            format!("pid {} runs a binary rebuilt since it started", w.pid),
            "restart the worker (one SIGTERM drains it, or systemctl --user restart forge-worker)",
        ),
        (true, Some(false)) => check("worker", Status::Ok, format!("pid {} running", w.pid), ""),
        (true, None) => check(
            "worker",
            Status::Ok,
            format!(
                "pid {} running (no /proc: stale-binary check skipped)",
                w.pid
            ),
            "",
        ),
        (false, _) => check(
            "worker",
            Status::Warn,
            format!("pid {} is gone", w.pid),
            "start it: forge work, or systemctl --user start forge-worker",
        ),
    }]
}

fn check_queue(store: &Store) -> Vec<Check> {
    let queued = match store.queued_count() {
        Ok(n) => n,
        Err(e) => return vec![check("queue", Status::Fail, format!("{e:#}"), "")],
    };
    let running = match store.running_ids() {
        Ok(r) => r,
        Err(e) => return vec![check("queue", Status::Fail, format!("{e:#}"), "")],
    };
    let orphans: Vec<i64> = match store.orphans(worker::pid_alive) {
        Ok(o) => o,
        Err(e) => return vec![check("queue", Status::Fail, format!("{e:#}"), "")],
    };
    let mut c = match (running.len(), orphans.len()) {
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
    };
    c.queued = Some(queued);
    c.running = Some(running.len() as i64);
    vec![c]
}

/// Every project still carrying the migration's placeholder purpose
/// (`Repository <path>.`, see `crate::store::is_placeholder_purpose`):
/// `forge project show` and the portal already hide it, but the operator
/// should still know it needs `forge project set --purpose`.
fn check_project_purposes(store: &Store) -> Vec<Check> {
    let projects = match store.list_projects() {
        Ok(p) => p,
        Err(e) => return vec![check("purposes", Status::Fail, format!("{e:#}"), "")],
    };
    let placeholder: Vec<String> = projects
        .into_iter()
        .filter(|p| crate::store::is_placeholder_purpose(&p.purpose))
        .map(|p| p.name)
        .collect();
    vec![if placeholder.is_empty() {
        check(
            "purposes",
            Status::Ok,
            "every project has a real purpose",
            "",
        )
    } else {
        check(
            "purposes",
            Status::Warn,
            format!(
                "{} project(s) still have the migration's placeholder purpose: {}",
                placeholder.len(),
                placeholder.join(", ")
            ),
            "forge project set <name> --purpose <text>",
        )
    }]
}

fn check_worktrees(store: &Store) -> Vec<Check> {
    let tasks = match store.tasks_with_worktrees() {
        Ok(t) => t,
        Err(e) => return vec![check("worktrees", Status::Fail, format!("{e:#}"), "")],
    };
    let held: Vec<_> = tasks
        .into_iter()
        .filter(|t| t.state != TaskState::Running && t.state != TaskState::Queued)
        .collect();
    let retained: Vec<i64> = held.iter().map(|t| t.id).collect();
    let oldest_days = held
        .iter()
        .filter_map(|t| t.finished_at)
        .min()
        .map(|fin| (unix_now() - fin).max(0) / 86_400);
    vec![if retained.is_empty() {
        check("worktrees", Status::Ok, "none retained", "")
    } else {
        let mut c = check(
            "worktrees",
            Status::Warn,
            format!(
                "{} retained: {:?}, oldest {}d",
                retained.len(),
                retained,
                oldest_days.unwrap_or(0)
            ),
            "forge gc removes the published ones and explains the rest",
        );
        c.worktree_ids = Some(retained);
        c
    }]
}

/// Every initiative currently held (see `worker::held_initiatives`, `view::
/// initiative_hold`): budget spent or a stop-rule streak, and how many of
/// its tasks sit queued behind it. Before this check the only place this
/// showed up was `forge initiative show <id>`, so a held initiative with a
/// quiet, otherwise-idle worker looked exactly like an empty queue (on
/// 2026-09-19 two initiatives sat held on budget this way for an hour).
fn check_initiatives(f: &Forge) -> Vec<Check> {
    let held = match worker::held_initiatives(f) {
        Ok(h) => h,
        Err(e) => return vec![check("initiatives", Status::Fail, format!("{e:#}"), "")],
    };
    let mut lines = Vec::new();
    let mut first_id = None;
    for id in held {
        let Ok(Some(ini)) = f.store.initiative(id) else {
            continue;
        };
        let queued = f
            .store
            .initiative_tasks(id)
            .map(|ts| ts.iter().filter(|t| t.state == TaskState::Queued).count())
            .unwrap_or(0);
        let Ok(Some(reason)) = crate::view::initiative_hold_reason(f, &ini) else {
            continue;
        };
        lines.push(format!(
            "initiative {id} ({}): {reason}, {queued} task(s) queued behind the hold",
            ini.project
        ));
        first_id.get_or_insert(id);
    }
    vec![if lines.is_empty() {
        check("initiatives", Status::Ok, "none held", "")
    } else {
        check(
            "initiatives",
            Status::Warn,
            lines.join("; "),
            format!(
                "forge initiative set {} --budget <usd> or --stop-after <n>",
                first_id.unwrap()
            ),
        )
    }]
}

fn check_logs(paths: &Paths) -> Vec<Check> {
    let events_path = paths.home.join("events.jsonl");
    let events_size = std::fs::metadata(&events_path)
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
    let dropped = crate::report::dropped_log_task_count(&events_path);
    let total = events_size + attempt_size;
    let detail = format!(
        "events.jsonl {}; {attempt_count} attempt log(s) totaling {}{}{}",
        human_bytes(events_size),
        human_bytes(attempt_size),
        oldest.map_or(String::new(), |o| format!(", oldest {}", ymd(o))),
        if dropped > 0 {
            format!("; {dropped} task(s) lost log lines")
        } else {
            String::new()
        },
    );
    const GIB: u64 = 1024 * 1024 * 1024;
    let disk_hint = format!(
        "{} of logs on disk; archive or delete old attempt logs under {} by hand",
        human_bytes(total),
        paths.logs.display()
    );
    let dropped_hint = format!(
        "check disk space and permissions for {}",
        events_path.display()
    );
    vec![match (total >= GIB, dropped > 0) {
        (false, false) => check("logs", Status::Ok, detail, ""),
        (true, false) => check("logs", Status::Warn, detail, disk_hint),
        (false, true) => check("logs", Status::Warn, detail, dropped_hint),
        (true, true) => check(
            "logs",
            Status::Warn,
            detail,
            format!("{disk_hint}; {dropped_hint}"),
        ),
    }]
}

fn check_spend(f: &Forge) -> Vec<Check> {
    let spent = match f.store.spent_since(unix_now() - 86_400) {
        Ok(s) => s,
        Err(e) => return vec![check("spend", Status::Fail, format!("{e:#}"), "")],
    };
    let mut c = match f.budget.per_day_usd {
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
            format!("${spent:.2} in the last 24h (no dollar cap; the rate windows are the limit)"),
            "",
        ),
    };
    c.spend_usd = Some(spent);
    c.spend_cap_usd = f.budget.per_day_usd;
    vec![c]
}

/// One row per provider that has recorded a rate-limit sample: each has
/// its own window and its own cap, so a full Anthropic window says
/// nothing about a provider that has never been used near its own.
fn check_rate_limit(f: &Forge) -> Vec<Check> {
    let mut out = Vec::new();
    for name in f.providers.keys() {
        let sample = match f.store.latest_rate_limit(name) {
            Ok(s) => s,
            Err(e) => {
                out.push(check(
                    "rate_limit",
                    Status::Fail,
                    format!("{name}: {e:#}"),
                    "",
                ));
                continue;
            }
        };
        let Some(s) = sample else { continue };
        let age = unix_now() - s.seen_at;
        let worst = s.five_hour.unwrap_or(0.0).max(s.seven_day.unwrap_or(0.0));
        let detail = format!(
            "{name}: 5h {}, 7d {} (seen {}, {}m ago)",
            s.five_hour
                .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
            s.seven_day
                .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
            crate::render::utc(s.seen_at),
            age / 60
        );
        let mut c = match crate::worker::window_hold(f, name) {
            Ok(Some((msg, _))) => check(
                "rate_limit",
                Status::Warn,
                format!("{detail}; {msg}"),
                "the worker holds until the reset, then continues",
            ),
            Ok(None) if worst >= 0.8 => check(
                "rate_limit",
                Status::Warn,
                detail,
                "a window is nearly at its cap; the worker will hold when it reaches it",
            ),
            Ok(None) => check("rate_limit", Status::Ok, detail, ""),
            Err(e) => check("rate_limit", Status::Fail, format!("{e:#}"), ""),
        };
        c.provider = Some(name.clone());
        c.five_hour_pct = s.five_hour;
        c.five_hour_resets_at = s.five_hour_resets;
        c.seven_day_pct = s.seven_day;
        c.seven_day_resets_at = s.seven_day_resets;
        out.push(c);
    }
    if out.is_empty() {
        out.push(check(
            "rate_limit",
            Status::Warn,
            "no samples yet",
            "samples arrive with the first real attempt",
        ));
    }
    out
}

/// Every automatic environment grant of the last 7 days (docs/OPS.md,
/// "Environment needs"): what the kernel opened for a repository without
/// asking, so a person can see what the policy is doing.
fn check_environment_grants(store: &Store) -> Vec<Check> {
    let since = unix_now() - 7 * 86_400;
    let grants = match store.decisions_of_kind_since(crate::environment::DECISION_KIND, since) {
        Ok(g) => g,
        Err(e) => {
            return vec![check("environment", Status::Warn, format!("{e:#}"), "")];
        }
    };
    if grants.is_empty() {
        return vec![check(
            "environment",
            Status::Ok,
            "no automatic grants in the last 7 days",
            "",
        )];
    }
    let lines: Vec<String> = grants
        .iter()
        .map(|d| {
            format!(
                "task {}: {}{}",
                d.task_id.map_or("-".to_string(), |t| t.to_string()),
                d.question.trim_start_matches("Environment need: "),
                if d.answered_by == "supervisor" {
                    " (approved by the supervisor)"
                } else {
                    ""
                }
            )
        })
        .collect();
    vec![check(
        "environment",
        Status::Ok,
        format!(
            "{} automatic grant(s) in the last 7 days\n{}",
            grants.len(),
            lines.join("\n")
        ),
        "the [environment] table in config.toml decides what is granted",
    )]
}

pub fn run() -> Result<Vec<Check>> {
    match Paths::resolve() {
        Ok(paths) => run_at(paths),
        Err(e) => {
            let mut out = check_binaries();
            out.extend(check_legacy_env());
            out.extend(check_home_migration());
            out.push(check(
                "home",
                Status::Fail,
                format!("{e:#}"),
                "set FORGE_HOME to a writable directory",
            ));
            Ok(out)
        }
    }
}

fn check_executors(store: &Store, paths: &Paths) -> Vec<Check> {
    use crate::executor::Backend;
    let mut backends = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for project in store.list_projects().unwrap_or_default() {
        for repo in store.project_repos(&project.name).unwrap_or_default() {
            match config::load_working_execution(std::path::Path::new(&repo.repo)) {
                Ok(execution) => {
                    backends.insert(execution.backend());
                    if execution.backend() == Backend::Ssh {
                        let destination = execution.ssh_destination().unwrap();
                        if let Ok(home) = config::load_home(&paths.home) {
                            for (name, provider) in home.providers {
                                let binary = match provider.runner {
                                    agent::Runner::ClaudeCli => "claude",
                                    agent::Runner::CodexCli => "codex",
                                    agent::Runner::CopilotCli => "copilot",
                                    agent::Runner::Chat => continue,
                                };
                                let result = std::process::Command::new("ssh")
                                    .args([
                                        "-o",
                                        "BatchMode=yes",
                                        "-o",
                                        "ConnectTimeout=5",
                                        &destination,
                                        binary,
                                        "--version",
                                    ])
                                    .output();
                                let ok = result.as_ref().is_ok_and(|o| o.status.success());
                                out.push(check(
                                    &format!("executors.ssh.{name}"),
                                    if ok { Status::Ok } else { Status::Warn },
                                    format!(
                                        "ssh {destination} {binary} --version: {}",
                                        if ok { "answers" } else { "did not answer" }
                                    ),
                                    "install and configure the CLI on the remote host",
                                ));
                                if provider.api_key_env.is_none() {
                                    out.push(check(&format!("executors.ssh.{name}.credentials"), Status::Warn,
                                        format!("{name}: subscription login credentials do not travel to {destination}"),
                                        "use a provider with api_key_env or configure a login on the remote host"));
                                }
                            }
                        }
                    }
                }
                Err(e) => out.push(check(
                    "executors",
                    Status::Fail,
                    format!("{}: {e:#}", repo.repo),
                    "fix [execution] backend",
                )),
            }
        }
    }
    if backends.is_empty() {
        backends.insert(crate::executor::default_backend());
    }
    if config::env("SANDBOX").as_deref() == Ok("0") {
        backends.insert(Backend::Host);
    }
    for backend in backends {
        let available = backend != Backend::Bwrap
            || std::process::Command::new("bwrap")
                .args([
                    "--unshare-net",
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                    "true",
                ])
                .output()
                .is_ok_and(|o| o.status.success());
        let guarantees = if available {
            backend.guarantees()
        } else {
            Default::default()
        };
        out.push(check(
            &format!("executors.{}", backend.as_str()),
            if !available { Status::Fail } else if !guarantees.egress_bounded { Status::Warn } else { Status::Ok },
            format!("{}: worktree_private={}, egress_bounded={}, credentials_seeded={}, checks_under_kernel_control={}{}",
                backend.as_str(), guarantees.worktree_private, guarantees.egress_bounded,
                guarantees.credentials_seeded, guarantees.checks_under_kernel_control,
                if !available { "; unavailable on this machine" } else if !guarantees.egress_bounded { "; egress is unbounded" } else { "" }),
            if backend == Backend::Host { "use backend = \"bwrap\" to bound egress" } else { "" },
        ));
    }
    out
}

/// The same checks as `run`, against an already-resolved `paths` rather
/// than re-resolving `FORGE_HOME`: what `forge init` calls so its closing
/// doctor pass looks at the exact home it just set up, even with `--home`.
pub fn run_at(paths: Paths) -> Result<Vec<Check>> {
    let mut out = check_binaries();
    out.extend(check_legacy_env());
    out.extend(check_home_migration());
    out.extend(check_home(&paths));
    out.extend(check_cache(&paths));
    out.extend(check_config(&paths));

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
    out.extend(check_schema(&store));
    out.extend(check_executors(&store, &paths));
    out.extend(check_project_purposes(&store));
    out.extend(check_egress(&paths, &store));
    out.extend(check_environment_grants(&store));
    out.extend(check_workflows(&paths));
    out.extend(check_plugins(&paths, &store));
    out.extend(check_learning(&paths, &store));
    out.extend(check_worker(&paths));
    out.extend(check_queue(&store));
    out.extend(check_worktrees(&store));
    out.extend(check_logs(&paths));

    if let Ok(f) = Forge::open_with(paths, store) {
        out.extend(check_initiatives(&f));
        out.extend(check_spend(&f));
        out.extend(check_rate_limit(&f));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_worktrees_is_ok_with_no_retained_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let checks = check_worktrees(&store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "worktrees");
        assert!(checks[0].status == Status::Ok);
        assert_eq!(checks[0].detail, "none retained");
    }

    /// A finished task whose worktree is still on disk: WARN, and the
    /// structured `worktree_ids` a client (the doctor page's gc control)
    /// reads instead of parsing the `{:?}`-formatted list out of `detail`.
    #[test]
    fn check_worktrees_warns_and_carries_the_retained_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let mut t = fixture_task(TaskState::Failed, "", 0, Some(crate::unix_now()));
        t.worktree = "/wt/1".into();
        t.id = store.insert_task(&t).unwrap();
        store.update_task(&t).unwrap();

        let checks = check_worktrees(&store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "worktrees");
        assert!(checks[0].status == Status::Warn);
        assert_eq!(checks[0].worktree_ids, Some(vec![t.id]));
        assert!(
            checks[0].detail.contains(&t.id.to_string()),
            "{}",
            checks[0].detail
        );
    }

    /// A `Forge` over a fresh, empty store in a throwaway home (the same
    /// fixture shape `view.rs`'s tests use).
    fn fixture() -> (tempfile::TempDir, Forge) {
        use crate::ctx::Paths;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    fn fixture_task(
        state: TaskState,
        reason: &str,
        initiative: i64,
        finished_at: Option<i64>,
    ) -> crate::store::Task {
        crate::store::Task {
            repo: "/repo".into(),
            task: "do the thing".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state,
            reason: reason.into(),
            finished_at,
            created_at: crate::unix_now(),
            workflow: "direct".into(),
            project: Some("demo".into()),
            initiative: Some(initiative),
            ..Default::default()
        }
    }

    /// One workflow measured on two providers, 0/10 on one and 8/10 on the
    /// other: the learning check warns once, for the first, naming it, and
    /// says nothing about the second (averaged, 8/20 would have read as
    /// failing for both).
    #[test]
    fn check_learning_warns_per_provider_not_on_the_average() {
        let (_dir, f) = fixture();
        let hash = workflows::load_all(&f.paths.home)
            .unwrap()
            .into_iter()
            .find(|w| w.name == "direct")
            .unwrap()
            .hash;
        for (provider, landed) in [("local", 0), ("anthropic", 8)] {
            for i in 0..10 {
                let state = if i < landed {
                    TaskState::Succeeded
                } else {
                    TaskState::Failed
                };
                let mut t = fixture_task(state, "", 0, Some(crate::unix_now()));
                t.provider = provider.into();
                t.workflow_hash = hash.clone();
                t.started_at = Some(crate::unix_now());
                t.id = f.store.insert_task(&t).unwrap();
                f.store.update_task(&t).unwrap();
            }
        }

        let checks = check_learning(&f.paths, &f.store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "learning");
        assert!(checks[0].status == Status::Warn, "{}", checks[0].detail);
        let warned: Vec<&str> = checks[0].detail.split("; ").collect();
        assert_eq!(warned.len(), 1, "{}", checks[0].detail);
        assert!(
            warned[0].starts_with("direct verifies 0/10 on local"),
            "{}",
            checks[0].detail
        );
        assert!(
            !checks[0].detail.contains("anthropic"),
            "{}",
            checks[0].detail
        );
    }

    #[test]
    fn check_initiatives_is_ok_with_none_held() {
        let (_dir, f) = fixture();
        let checks = check_initiatives(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "initiatives");
        assert!(checks[0].status == Status::Ok);
        assert_eq!(checks[0].detail, "none held");
    }

    /// A held initiative (its trailing same-rule failures reached its
    /// stop rule) with one task still queued behind the hold: WARN,
    /// naming the initiative, the rule and streak, the queued count, and
    /// a fix line naming `forge initiative set <id>`.
    #[test]
    fn check_initiatives_warns_for_a_held_initiative_and_names_it() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&crate::store::Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let ini_id = f
            .store
            .create_initiative(&crate::store::Initiative {
                project: "demo".into(),
                outcome: "o".into(),
                stop_after_same_rule: 2,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        for _ in 0..2 {
            let mut t = fixture_task(
                TaskState::Failed,
                "L0 failed: has-commits (after 1 attempt(s))",
                ini_id,
                Some(crate::unix_now()),
            );
            t.id = f.store.insert_task(&t).unwrap();
            f.store.update_task(&t).unwrap();
        }
        let mut queued = fixture_task(TaskState::Queued, "", ini_id, None);
        queued.id = f.store.insert_task(&queued).unwrap();
        f.store.update_task(&queued).unwrap();

        let checks = check_initiatives(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "initiatives");
        assert!(checks[0].status == Status::Warn);
        assert!(
            checks[0].detail.contains(&format!("initiative {ini_id}")),
            "{}",
            checks[0].detail
        );
        assert!(
            checks[0].detail.contains("stop rule: has-commits"),
            "{}",
            checks[0].detail
        );
        assert!(
            checks[0].detail.contains("1 task(s) queued"),
            "{}",
            checks[0].detail
        );
        assert!(
            checks[0]
                .hint
                .contains(&format!("forge initiative set {ini_id}")),
            "{}",
            checks[0].hint
        );
    }
}
