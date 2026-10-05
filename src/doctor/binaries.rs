//! The `binary.*` and sandbox rows: each required binary under the shell's
//! PATH and under the PATH the worker unit declares.

use super::{Check, Status, check};
use crate::{agent, config, sandbox, unit_path};

/// FAIL when the worker unit's own PATH cannot find `name`, though the
/// shell's can: the worker would crash-loop on a check that passes here.
fn missing_under_unit(name: &str) -> Option<Check> {
    let (file, path) = unit_path::worker_unit_path()?;
    if unit_path::find_in(&path, name).is_some() {
        return None;
    }
    Some(check(
        &format!("binary.{name}"),
        Status::Fail,
        format!(
            "{name} is not on the PATH {} declares: {path}",
            file.display()
        ),
        format!(
            "run `forge init --relink` from a shell where {name} resolves, or add its directory to Environment=PATH= in {}, then `systemctl --user daemon-reload` and restart {}",
            file.display(),
            unit_path::WORKER_UNIT
        ),
    ))
}

fn binary(name: &str, required: bool, why_optional: &str) -> Check {
    if required && let Some(fail) = missing_under_unit(name) {
        return fail;
    }
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
pub(super) fn check_binaries() -> Vec<Check> {
    let mut out = Vec::new();
    let agent_bin = agent::agent_bin();
    out.push(binary(&agent_bin, true, ""));
    out.push(binary("git", true, ""));
    let sandbox_off = config::env("SANDBOX").as_deref() == Ok("0");
    if !sandbox_off && let Some(fail) = missing_under_unit("bwrap") {
        out.push(fail);
    }
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

pub(super) fn check_resource_limits(paths: &config::SandboxPaths) -> Check {
    if config::env("SANDBOX").as_deref() == Ok("0") || sandbox::resolve_binary("bwrap").is_err() {
        return check(
            "sandbox.resources",
            Status::Warn,
            "host backend: sandbox resource limits are not in force",
            "install bubblewrap and enable the sandbox",
        );
    }
    let limits = paths.limits;
    let scope = limits.scope_runner().is_some();
    let detail = format!(
        "bwrap launches: tmpfs /dev/shm={} bytes, HOME=268435456 bytes, /run=67108864 bytes; core=0, file={} bytes, nofile=4096; /tmp and cache uppers on disk; {}",
        limits.tmp_bytes,
        limits.memory_max / 512 * 512,
        if scope {
            format!(
                "user scope MemoryMax={} TasksMax={}",
                limits.memory_max, limits.tasks_max
            )
        } else {
            "user scope unavailable: aggregate memory and process limits are NOT in force".into()
        }
    );
    check(
        "sandbox.resources",
        if scope { Status::Ok } else { Status::Warn },
        detail,
        if scope {
            ""
        } else {
            "enable a systemd user manager with memory and pids controllers"
        },
    )
}
