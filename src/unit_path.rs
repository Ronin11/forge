//! The PATH the systemd units `forge init` writes carry: composing it from
//! the installing shell, reading it back from the unit file, and resolving
//! a binary under it. A foundation helper, so `init`, `doctor` and the
//! worker share one reading of the unit.

use anyhow::{Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const WORKER_UNIT: &str = "forge-worker.service";

/// `$XDG_CONFIG_HOME/systemd/user`, else `$HOME/.config/systemd/user` —
/// the OS user's own config directory, never `FORGE_HOME` (which may sit
/// elsewhere entirely): systemd only ever looks for user units there.
pub fn systemd_user_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(p).join("systemd/user"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/systemd/user"))
}

/// `bin_dir` followed by the shell's own PATH entries, deduplicated in
/// order. Empty and relative entries are dropped: a unit has no working
/// directory to resolve them against. Only a shell with no usable PATH at
/// all falls back to the system directories.
pub fn compose(bin_dir: &Path, shell_path: Option<OsString>) -> String {
    let mut dirs: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    let shell: Vec<PathBuf> = shell_path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let fallback = ["/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from);
    let tail = if shell.iter().any(|d| d.is_absolute()) {
        shell
    } else {
        fallback.to_vec()
    };
    for d in tail {
        if d.is_absolute() && !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// The `Environment=PATH=` a unit file declares, if it exists and has one.
pub fn declared_path(unit: &Path) -> Option<String> {
    let text = std::fs::read_to_string(unit).ok()?;
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("Environment=")?;
        let v = v.trim().trim_matches('"');
        v.strip_prefix("PATH=").map(str::to_string)
    })
}

/// The worker unit's file and its declared PATH, when the file has one.
pub fn worker_unit_path() -> Option<(PathBuf, String)> {
    let file = systemd_user_dir()?.join(WORKER_UNIT);
    let path = declared_path(&file)?;
    Some((file, path))
}

/// `name` under the colon-separated `path`.
pub fn find_in(path: &str, name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name)).filter(|p| p.is_file());
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|c| c.is_file())
}

/// Resolve `name` under this process's PATH; when it is missing, say whose
/// PATH that is and how to fix the unit that gave the worker it.
pub fn require_on_path(name: &str) -> Result<()> {
    if crate::sandbox::resolve_binary(name).is_ok() {
        return Ok(());
    }
    let have = std::env::var("PATH").unwrap_or_default();
    bail!(
        "{name} not found in PATH\nthe worker's PATH is {have}; {WORKER_UNIT} declares it with Environment=PATH=. Re-run `forge init --relink` from a shell where {name} resolves, then restart the unit"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_puts_bin_dir_first_and_dedups_the_shell_path() {
        let p = compose(
            Path::new("/h/bin"),
            Some("/x/agents:/h/bin:/usr/bin:/x/agents:rel::/bin".into()),
        );
        assert_eq!(p, "/h/bin:/x/agents:/usr/bin:/bin");
    }

    #[test]
    fn compose_falls_back_without_a_shell_path() {
        assert_eq!(
            compose(Path::new("/h/bin"), None),
            "/h/bin:/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn declared_path_reads_the_environment_line() {
        let d = std::env::temp_dir().join(format!("unit-path-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("u.service");
        std::fs::write(
            &f,
            "[Service]\nEnvironment=FORGE_HOME=/h\nEnvironment=PATH=/a:/b\n",
        )
        .unwrap();
        assert_eq!(declared_path(&f).as_deref(), Some("/a:/b"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
