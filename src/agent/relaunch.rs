//! The relaunch budget every sandboxed spawn shares: agent phases, checks
//! and operations. Beside `command_in`, which builds each of those spawns.

use std::time::Duration;

/// Relaunches allowed after the first launch.
const MAX_RELAUNCHES: u32 = 3;

/// A launch that exits sooner than this with the bind-mount message never
/// got as far as running anything.
const QUICK_EXIT: Duration = Duration::from_secs(2);

/// The launch race this covers: two sandboxes seed `$HOME/.claude.json`
/// from the same host file at once, or the operator's own claude rewrites
/// it while a launch binds it, and bwrap's own bind-mount setup (not the
/// claude CLI's rename) loses. bwrap always reports it exactly this way, so
/// matching the message is precise enough without a regex crate.
pub(crate) fn is_transient_bwrap_failure(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("bwrap: Can") else {
            return false;
        };
        let mut chars = rest.chars();
        chars.next().is_some() && chars.as_str().starts_with("t bind mount")
    })
}

/// The relaunch budget for one spawn. After each launch that ends, ask
/// `again`: `true` means the launch failed on the transient bwrap race and
/// should be redone. That is a launch failure, not an attempt, so it gets a
/// few silent relaunches rather than burning one of the attempt's own
/// retries. Any other quick exit (a real crash, a fast fake in tests) is
/// returned as is.
#[derive(Default)]
pub(crate) struct Relaunch {
    count: u32,
}

impl Relaunch {
    /// Whether the launch that just ended with `stderr` after `wall` (and
    /// did or did not hit its timeout) should be relaunched; counts it
    /// when so.
    pub(crate) fn again(&mut self, stderr: &str, timed_out: bool, wall: Duration) -> bool {
        let quick = !timed_out && wall < QUICK_EXIT;
        if quick && self.count < MAX_RELAUNCHES && is_transient_bwrap_failure(stderr) {
            self.count += 1;
            return true;
        }
        false
    }

    /// The note a relaunch prints.
    pub(crate) fn note(&self) -> String {
        format!(
            "transient bwrap bind-mount failure on launch, relaunching (attempt {}/{MAX_RELAUNCHES})",
            self.count
        )
    }

    /// The log line recording a relaunch and the stderr that caused it.
    pub(crate) fn log_line(&self, stderr: &str) -> String {
        format!(
            "{{\"type\":\"forge_relaunch\",\"attempt\":{},\"reason\":{}}}",
            self.count,
            serde_json::to_string(stderr).unwrap_or_default()
        )
    }
}

/// An `Execution` whose bwrap is a script that fails `failures` times with
/// the bind-mount message and then runs the command after `--`, and the
/// file counting its launches.
#[cfg(test)]
pub(crate) fn fake_bwrap(
    dir: &std::path::Path,
    failures: u32,
) -> (crate::executor::Execution, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let counter = dir.join("launches");
    let script = dir.join("bwrap");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             n=$(cat {c} 2>/dev/null || echo 0); n=$((n + 1)); echo $n > {c}\n\
             if [ \"$n\" -le {failures} ]; then\n\
             echo \"bwrap: Can't bind mount /h/.claude.json on /h/.claude.json: \
             Unable to mount source on destination: No such file or directory\" >&2\n\
             exit 1\n\
             fi\n\
             while [ \"$1\" != -- ]; do shift; done; shift\n\
             exec \"$@\"\n",
            c = counter.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let sandbox = crate::sandbox::Sandbox::with_bwrap(script, home);
    (crate::executor::Execution::bwrap_only(sandbox), counter)
}

/// How many times the fake bwrap was launched.
#[cfg(test)]
pub(crate) fn launches(counter: &std::path::Path) -> u32 {
    std::fs::read_to_string(counter)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_transient_bwrap_failure_matches_the_bind_mount_race() {
        let msg = "bwrap: Can't bind mount /home/ronin/.claude.json on \
                    /home/ronin/.claude.json: Unable to mount source on \
                    destination: No such file or directory";
        assert!(is_transient_bwrap_failure(msg));
        // Leading indentation on the line is still a match.
        assert!(is_transient_bwrap_failure(&format!("  {msg}")));
    }

    #[test]
    fn is_transient_bwrap_failure_ignores_other_stderr() {
        assert!(!is_transient_bwrap_failure(""));
        assert!(!is_transient_bwrap_failure("agent crashed: out of memory"));
        assert!(!is_transient_bwrap_failure(
            "bwrap: Can't create file /run/forge/seed/claude.json: Permission denied"
        ));
        assert!(!is_transient_bwrap_failure(
            "bwrap: execvp claude: No such file or directory"
        ));
    }

    #[test]
    fn the_budget_is_three_relaunches_and_only_for_quick_exits() {
        let msg = "bwrap: Can't bind mount /h/.claude.json on /h/.claude.json: gone";
        let quick = Duration::from_millis(50);
        let mut r = Relaunch::default();
        assert!(r.again(msg, false, quick));
        assert!(r.again(msg, false, quick));
        assert!(r.again(msg, false, quick));
        assert!(!r.again(msg, false, quick), "a fourth relaunch is refused");
        let mut r = Relaunch::default();
        assert!(
            !r.again(msg, true, quick),
            "a timeout is not a launch failure"
        );
        assert!(
            !r.again(msg, false, Duration::from_secs(5)),
            "a slow exit ran"
        );
        assert!(!r.again("other", false, quick));
    }
}
