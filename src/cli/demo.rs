//! `forge demo`: a newcomer's first run. A scratch repository with a bare
//! origin beside it under `FORGE_HOME/demo`, registered as project `demo`,
//! one small task run to completion in the foreground through `forge run`
//! itself, and a closing note on where to look.

use super::*;
use std::process::Command;

const PROJECT: &str = "demo";
const TASK: &str = "write the answer, 42, to answer.txt and commit";
const FORGE_TOML: &str = "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt\"]\n";
/// The e2e suite's own fake agent: writes 42 to answer.txt, commits, reports.
const FAKE_AGENT: &str = include_str!("../../tests/fakes/ok.sh");

fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Forge Demo",
            "-c",
            "user.email=demo@forge.invalid",
        ])
        .args(args)
        .output()
        .context("running git")?;
    if !o.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Whether a failing `doctor` check should still refuse the demo: `--fake`
/// does not need the agent binary it names, every other failure still does.
fn doctor_check_blocks(status: doctor::Status, check_name: &str, fake: bool, agent: &str) -> bool {
    status == doctor::Status::Fail && !(fake && check_name == agent)
}

/// Whether the demo needs bubblewrap and doesn't have it: no binary found,
/// the sandbox hasn't been turned off, and the host isn't macOS (where the
/// demo runs on the host regardless).
fn sandbox_unavailable(have_bwrap: bool, sandbox_off: bool, is_macos: bool) -> bool {
    !have_bwrap && !sandbox_off && !is_macos
}

/// Refuse, naming the fix, when `forge doctor` would fail or attempts have
/// nowhere to run. `--fake` needs no agent CLI.
fn preflight(fake: bool) -> Result<()> {
    let agent = format!("binary.{}", crate::agent::agent_bin());
    for c in doctor::run()? {
        if doctor_check_blocks(c.status, &c.name, fake, &agent) {
            bail!("forge doctor fails on {}: {}; {}", c.name, c.detail, c.hint);
        }
    }
    if fake {
        let home = crate::ctx::Paths::resolve()?.home;
        let cfg = config::load_home(&home)?;
        let claude = cfg
            .providers
            .get("anthropic")
            .is_some_and(|p| p.runner == crate::agent::Runner::ClaudeCli);
        if !claude {
            bail!(
                "demo --fake needs the built-in anthropic provider on the claude CLI; remove the [providers.anthropic] runner override from config.toml, or run the demo without --fake"
            );
        }
    }
    let sandbox_off = config::env("SANDBOX").as_deref() == Ok("0");
    if sandbox_unavailable(
        crate::sandbox::resolve_binary("bwrap").is_ok(),
        sandbox_off,
        cfg!(target_os = "macos"),
    ) {
        bail!(
            "no bwrap and no host backend chosen: install bubblewrap, or set FORGE_SANDBOX=0 to run the demo on the host"
        );
    }
    Ok(())
}

/// The scratch repository and its bare origin, the layout this box uses,
/// with the first commit already pushed so landing has somewhere to go.
fn scaffold(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let (repo, origin) = (dir.join("repo"), dir.join("origin.git"));
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(&origin)?;
    git_in(&origin, &["init", "-q", "--bare", "-b", "main"])?;
    git_in(&repo, &["init", "-q", "-b", "main"])?;
    git_in(&repo, &["config", "user.name", "Forge Demo"])?;
    git_in(&repo, &["config", "user.email", "demo@forge.invalid"])?;
    std::fs::write(
        repo.join("README.md"),
        "# demo\n\nA scratch repository made by `forge demo`.\n",
    )?;
    std::fs::write(repo.join("forge.toml"), FORGE_TOML)?;
    git_in(&repo, &["add", "-A"])?;
    git_in(&repo, &["commit", "-qm", "the demo repository"])?;
    git_in(
        &repo,
        &["remote", "add", "origin", &origin.display().to_string()],
    )?;
    git_in(&repo, &["push", "-q", "origin", "main"])?;
    Ok((repo, origin))
}

fn register(repo: &Path) -> Result<()> {
    let f = Forge::open(false, false)?;
    if f.store.project(PROJECT)?.is_none() {
        f.store.create_project(&crate::store::Project {
            name: PROJECT.into(),
            purpose: "Forge's first-run demo: one small task on a scratch repository.".into(),
            created_at: unix_now(),
            ..Default::default()
        })?;
    }
    let repo = repo.canonicalize()?.display().to_string();
    f.store.register_repo(PROJECT, &repo, None)
}

/// Whether an inherited binary-override variable must be stripped before a
/// `--fake` run: the task's own provider is the only one that may reach the
/// fake agent, never a per-role override for claude, codex or copilot.
fn strip_binary_env(key: &str) -> bool {
    ["FORGE_CLAUDE_BIN_", "FORGE_CODEX_BIN", "FORGE_COPILOT_BIN"]
        .iter()
        .any(|p| key.starts_with(p))
}

/// `forge run` on the demo repository, in the foreground; its exit is ours to judge.
fn run_task(repo: &Path, dir: &Path, fake: bool) -> Result<bool> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("run")
        .arg(repo)
        .arg(TASK)
        .args(["--project", PROJECT, "--retries", "0"]);
    if fake {
        let bin = dir.join("fake-agent.sh");
        std::fs::write(&bin, FAKE_AGENT)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
        // Every step runs the claude-cli runner against the fake: the task's
        // own provider beats the roles tables, and no other binary override
        // (per-role claude, codex, copilot) may reach the child.
        for (k, _) in std::env::vars() {
            if strip_binary_env(&k) {
                cmd.env_remove(k);
            }
        }
        cmd.args(["--provider", "anthropic"])
            .env("FORGE_CLAUDE_BIN", &bin)
            .env("FORGE_SUPERVISOR", "0");
    }
    Ok(cmd.status().context("running forge run")?.success())
}

fn report(home: &Path, origin: &Path, ok: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let task = f
        .store
        .project_tasks(PROJECT)?
        .into_iter()
        .map(|t| t.id)
        .max();
    out!();
    let Some(id) = task else {
        bail!("the demo task was never queued");
    };
    let t = f.store.task(id)?;
    let state = t.map(|t| format!("{:?}", t.state)).unwrap_or_default();
    out!(
        "demo task {id} ended {}",
        if ok { "succeeded" } else { state.as_str() }
    );
    out!("  trace   forge trace {id}");
    out!(
        "  web     http://127.0.0.1:7788/?token={}   (start it with: forge web serve)",
        super::web::web_token(home)?
    );
    if ok {
        let head = git_in(origin, &["log", "-1", "--format=%h %s", "main"])?;
        out!("  landed  {head}   (in {})", origin.display());
    }
    if !ok {
        bail!("the demo task did not succeed; `forge trace {id}` says why");
    }
    Ok(())
}

pub(super) fn demo(fake: bool, reset: bool) -> Result<()> {
    preflight(fake)?;
    let home = crate::ctx::Paths::resolve()?.home;
    let dir = home.join("demo");
    if dir.exists() {
        if !reset {
            out!("the demo already exists at {}", dir.display());
            out!("run `forge demo --reset` to wipe it and run it again");
            return Ok(());
        }
        std::fs::remove_dir_all(&dir)?;
    }
    let (repo, origin) = scaffold(&dir)?;
    register(&repo)?;
    out!("demo repository {}", repo.display());
    let ok = run_task(&repo, &dir, fake)?;
    report(&home, &origin, ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_check_blocks_only_a_failure_fake_does_not_ignore() {
        let agent = "binary.claude";
        // A failure on the agent binary blocks a real run but not --fake.
        assert!(doctor_check_blocks(
            doctor::Status::Fail,
            agent,
            false,
            agent
        ));
        assert!(!doctor_check_blocks(
            doctor::Status::Fail,
            agent,
            true,
            agent
        ));
        // A failure on anything else still blocks --fake.
        assert!(doctor_check_blocks(
            doctor::Status::Fail,
            "binary.git",
            true,
            agent
        ));
        // A passing check never blocks, fake or not.
        assert!(!doctor_check_blocks(
            doctor::Status::Ok,
            agent,
            false,
            agent
        ));
    }

    #[test]
    fn sandbox_unavailable_only_when_bwrap_is_missing_and_nothing_else_covers_it() {
        assert!(sandbox_unavailable(false, false, false));
        assert!(!sandbox_unavailable(true, false, false));
        assert!(!sandbox_unavailable(false, true, false));
        assert!(!sandbox_unavailable(false, false, true));
    }

    #[test]
    fn strip_binary_env_matches_only_the_per_role_binary_overrides() {
        assert!(strip_binary_env("FORGE_CLAUDE_BIN_STEP"));
        assert!(strip_binary_env("FORGE_CODEX_BIN"));
        assert!(strip_binary_env("FORGE_CODEX_BIN_STEP"));
        assert!(strip_binary_env("FORGE_COPILOT_BIN"));
        assert!(!strip_binary_env("FORGE_CLAUDE_BIN"));
        assert!(!strip_binary_env("FORGE_SANDBOX"));
        assert!(!strip_binary_env("OTHER"));
    }
}
