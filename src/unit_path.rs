//! The PATH the systemd units `forge init` writes carry: composing it from
//! the installing shell, reading it back from the unit file, and resolving
//! a binary under it. A foundation helper, so `init`, `doctor` and the
//! worker share one reading of the unit.

use anyhow::{Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const WORKER_UNIT: &str = "forge-worker.service";
pub const WEB_UNIT: &str = "forge-web.service";

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

/// `s` as one double-quoted systemd word: `\` and `"` escaped and `%`
/// doubled (a specifier), so whitespace stays inside the word. A newline
/// would end the line and start another directive, so it is refused.
/// `expand` also doubles `$`, which `ExecStart=` arguments need
/// (`$VAR` expansion) and `Environment=` values must not have: systemd
/// does no expansion there, so `$$` would reach the process as two dollars.
fn quote(s: &str, expand: bool) -> Result<String> {
    if s.contains(['\n', '\r']) {
        bail!("cannot write {s:?} into a systemd unit: it contains a newline");
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' if expand => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// `Environment="key=value"`, with no `$` doubling.
pub fn environment_line(key: &str, value: &str) -> Result<String> {
    Ok(format!(
        "Environment={}",
        quote(&format!("{key}={value}"), false)?
    ))
}

/// `ExecStart=` with each argument quoted; `$` is doubled.
pub fn exec_start_line(args: &[&str]) -> Result<String> {
    let words = args
        .iter()
        .map(|a| quote(a, true))
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("ExecStart={}", words.join(" ")))
}

/// The value of an `Environment=` line: unquoted and unescaped when it is
/// double-quoted, as written by `environment_line`, else taken as is.
/// Only `Environment=` is read, where `$` has no meaning and is kept.
fn unquote(v: &str) -> String {
    let v = v.trim();
    let Some(inner) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return v.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(&n)) if n == '\\' || n == '"' => {
                out.push(n);
                chars.next();
            }
            ('%', Some('%')) => {
                out.push('%');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// `fresh` followed by the entries of `existing` (a unit's declared PATH)
/// that `fresh` lacks, in their order, so a re-run from a narrower shell
/// keeps the directories the unit already had. Empty and relative entries
/// of `existing` are dropped, as `compose` drops them.
pub fn merge(fresh: &str, existing: Option<&str>) -> String {
    let mut dirs: Vec<&str> = fresh.split(':').collect();
    for d in existing.unwrap_or_default().split(':') {
        if Path::new(d).is_absolute() && !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
}

/// The `Environment=PATH=` a unit file declares, if it exists and has one.
pub fn declared_path(unit: &Path) -> Option<String> {
    let text = std::fs::read_to_string(unit).ok()?;
    text.lines().find_map(|l| {
        let v = unquote(l.trim().strip_prefix("Environment=")?);
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
    fn merge_keeps_the_existing_entries_the_new_path_lacks_after_the_new_ones() {
        assert_eq!(
            merge("/h/bin:/usr/bin", Some("/h/bin:/x/agents:rel::/usr/bin:/y")),
            "/h/bin:/usr/bin:/x/agents:/y"
        );
        assert_eq!(merge("/h/bin:/usr/bin", None), "/h/bin:/usr/bin");
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

    #[test]
    fn quote_escapes_and_refuses_a_newline() {
        assert_eq!(
            quote(r#"a b\c"d%e$f"#, false).unwrap(),
            r#""a b\\c\"d%%e$f""#
        );
        assert_eq!(
            quote(r#"a b\c"d%e$f"#, true).unwrap(),
            r#""a b\\c\"d%%e$$f""#
        );
        assert!(quote("a\nb", false).is_err());
        assert!(quote("a\rb", true).is_err());
    }

    #[test]
    fn exec_start_quotes_each_argument_and_doubles_dollars() {
        let l = exec_start_line(&["/h ome/forge", "work", "$x"]).unwrap();
        assert_eq!(l, r#"ExecStart="/h ome/forge" "work" "$$x""#);
    }

    #[test]
    fn declared_path_reads_back_a_home_and_path_with_spaces_and_specials() {
        let home = "/tmp/my home/h$x%y";
        let path = "/tmp/my home/bin:/tmp/dir$y:/mnt/c/Program Files/a\"b";
        let h = environment_line("FORGE_HOME", home).unwrap();
        let p = environment_line("PATH", path).unwrap();
        assert_eq!(h, r#"Environment="FORGE_HOME=/tmp/my home/h$x%%y""#);
        assert!(p.contains("/tmp/dir$y:") && !p.contains("$$"), "{p}");
        let d = std::env::temp_dir().join(format!("unit-path-q-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("u.service");
        std::fs::write(&f, format!("[Service]\n{h}\n{p}\n")).unwrap();
        assert_eq!(declared_path(&f).as_deref(), Some(path));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
