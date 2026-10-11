//! `forge plugin test <name>`: a plugin's own proof that it works end to
//! end, the manifest's `test` (docs/PLUGINS.md, "Self-test"). It runs in
//! the plugin's directory with the environment the running plugin has, so
//! it can look at what that process left in `FORGE_PLUGIN_STATE`; `forge
//! doctor` runs it for every enabled plugin that declares one.
use super::{Plugin, note_in_log, plugin_env};
use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a self-test may run before it is killed and counted a failure.
pub const TIMEOUT: Duration = Duration::from_secs(300);

/// What one self-test said.
pub struct Outcome {
    pub ok: bool,
    /// Its combined stdout and stderr, or why it could not run.
    pub output: String,
}

impl Outcome {
    /// The last non-empty line of the output: the verdict a self-test
    /// prints last, for one-line displays.
    pub fn last_line(&self) -> &str {
        self.output
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
    }
}

/// Runs `plugin`'s self-test; `Ok(None)` when it declares none. The output
/// is kept at `logs/plugins/<name>.test.log` and a one-line result is
/// noted in the plugin's own log.
pub fn run(home: &Path, plugin: &Plugin, timeout: Duration) -> Result<Option<Outcome>> {
    let Some(argv) = &plugin.manifest.test else {
        return Ok(None);
    };
    let state_dir = home.join("plugins-state").join(&plugin.name);
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    let logs = home.join("logs").join("plugins");
    std::fs::create_dir_all(&logs)?;
    let out_path = logs.join(format!("{}.test.log", plugin.name));
    let out = std::fs::File::create(&out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut child = match Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&plugin.dir)
        .env_clear()
        .envs(plugin_env(plugin, home, &state_dir)?)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out))
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Ok(Some(Outcome {
                ok: false,
                output: format!("could not start {argv:?}: {e}"),
            }));
        }
    };
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if start.elapsed() >= timeout {
            // Its whole group: a shell test's children too.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut output = std::fs::read_to_string(&out_path).unwrap_or_default();
    let ok = status.is_some_and(|s| s.success());
    match status {
        None => output.push_str(&format!(
            "\n[forge] self-test killed after {}s",
            timeout.as_secs()
        )),
        Some(s) if !s.success() && output.trim().is_empty() => {
            output = format!("[forge] self-test {s} with no output")
        }
        _ => {}
    }
    let outcome = Outcome { ok, output };
    note_in_log(
        &logs.join(format!("{}.log", plugin.name)),
        &format!(
            "self-test {}: {}",
            if ok { "passed" } else { "failed" },
            outcome.last_line()
        ),
    );
    Ok(Some(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn plugin(dir: &Path, script: &str) -> Plugin {
        let path = dir.join("t.sh");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let text = "name = \"p\"\nrun = [\"./t.sh\"]\ntest = [\"./t.sh\", \"selftest\"]\ncapabilities = [\"events\"]\n";
        Plugin {
            name: "p".into(),
            manifest: super::super::parse_manifest(&dir.join("plugin.toml"), text, "p").unwrap(),
            dir: dir.to_path_buf(),
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn a_self_test_passes_on_exit_zero_and_sees_the_plugins_state() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(
            dir.path(),
            "#!/bin/sh\n[ \"$1\" = selftest ] || exit 2\necho checking\necho \"ok in $FORGE_PLUGIN_STATE\"\n",
        );
        let o = run(home.path(), &p, TIMEOUT).unwrap().unwrap();
        assert!(o.ok, "{}", o.output);
        assert!(o.last_line().ends_with("plugins-state/p"), "{}", o.output);
        let log = std::fs::read_to_string(home.path().join("logs/plugins/p.log")).unwrap();
        assert!(log.contains("self-test passed: ok in"), "{log}");
    }

    #[test]
    fn a_self_test_that_hangs_is_killed_and_fails() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path(), "#!/bin/sh\necho waiting\nsleep 30\n");
        let o = run(home.path(), &p, Duration::from_millis(300))
            .unwrap()
            .unwrap();
        assert!(!o.ok);
        assert!(o.output.contains("waiting"), "{}", o.output);
        assert!(o.output.contains("killed after"), "{}", o.output);
    }
}
