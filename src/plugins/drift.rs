//! Installed plugin copies drift from the tree they were installed from the
//! way catalog workflows once drifted from the built-ins: nothing said so.
//! `install` records a hash of the plugin's tracked files (its `plugin.toml`
//! and the script its `run` names) beside the installed copy; `check`
//! compares the installed copy, the recorded hash, and the source's current
//! hash; `refresh` copies the source over the installed copy when that
//! loses nothing the operator wrote. See docs/PLUGINS.md "Drift".

use super::{Manifest, parse_manifest, run_build};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Written into the installed directory by `install` and `refresh`; never
/// copied out of a source directory and never counted in the hash.
pub const RECORD_FILE: &str = ".forge-install.json";

/// The operator's own per-plugin configuration lives in the installed
/// directory under this name; a refresh never writes it.
const CONFIG_FILE: &str = "config";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallRecord {
    /// The directory the plugin was installed from, absolute.
    pub source: String,
    /// `tracked_hash` of the installed files when they were last copied.
    pub hash: String,
}

/// How an installed plugin stands against its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drift {
    /// The installed files are the source's current files.
    Current,
    /// Untouched since install; the source has moved on.
    Behind,
    /// The installed files differ from both the recorded install hash and
    /// the source's: the operator changed them.
    OperatorEdit,
    /// Nothing recorded what was installed (installed before hashes were
    /// recorded), so an edit cannot be told from a stale copy.
    Unrecorded,
    /// The recorded source directory is gone or no longer a plugin.
    SourceMissing,
}

impl Drift {
    pub fn as_str(self) -> &'static str {
        match self {
            Drift::Current => "current",
            Drift::Behind => "behind",
            Drift::OperatorEdit => "operator-edit",
            Drift::Unrecorded => "unrecorded",
            Drift::SourceMissing => "source-missing",
        }
    }

    /// Drift worth a doctor warning: the installed copy is not what the
    /// source says it should be, and the operator can act on it.
    pub fn is_drifted(self) -> bool {
        matches!(self, Drift::Behind | Drift::OperatorEdit)
    }
}

/// The pure comparison: hashes of the installed files, the recorded install
/// hash, and the source's current hash (`None` when there is no source).
pub fn compare(installed: &str, recorded: Option<&str>, source: Option<&str>) -> Drift {
    let Some(source) = source else {
        return match recorded {
            Some(_) => Drift::SourceMissing,
            None => Drift::Unrecorded,
        };
    };
    if installed == source {
        return Drift::Current;
    }
    match recorded {
        Some(r) if r == installed => Drift::Behind,
        Some(_) => Drift::OperatorEdit,
        None => Drift::Unrecorded,
    }
}

/// A plugin's drift as `forge plugin list` and doctor show it.
#[derive(Clone, Debug)]
pub struct Standing {
    pub drift: Drift,
    /// Added plus removed lines between the installed and source files;
    /// `None` when there is no source to compare.
    pub diff_lines: Option<usize>,
    pub source: Option<String>,
}

impl Standing {
    pub fn describe(&self) -> String {
        match (self.drift, self.diff_lines) {
            (Drift::Behind, Some(n)) => format!("behind the repo copy ({n} diff line(s))"),
            (Drift::OperatorEdit, Some(n)) => {
                format!("operator edit ({n} diff line(s) from the repo copy)")
            }
            (Drift::Current, _) => "matches the repo copy".to_string(),
            (Drift::Unrecorded, _) => "no install record".to_string(),
            (Drift::SourceMissing, _) => "source copy missing".to_string(),
            (d, _) => d.as_str().to_string(),
        }
    }
}

/// The files that make up "the plugin" for drift: its manifest and the
/// script its `run` names, when that is a file inside the directory.
fn tracked_files(dir: &Path) -> Vec<String> {
    let mut files = vec!["plugin.toml".to_string()];
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let manifest = std::fs::read_to_string(dir.join("plugin.toml"))
        .ok()
        .and_then(|t| parse_manifest(&dir.join("plugin.toml"), &t, &name).ok());
    if let Some(m) = manifest {
        let script = m.run[0].trim_start_matches("./").to_string();
        if !script.starts_with('/') && !script.contains("..") && dir.join(&script).is_file() {
            files.push(script);
        }
    }
    files
}

/// SHA-256 over each tracked file's name and bytes. A missing tracked file
/// hashes as absent, so it differs from an empty one.
pub fn tracked_hash(dir: &Path) -> String {
    let mut h = Sha256::new();
    for f in tracked_files(dir) {
        h.update(f.as_bytes());
        match std::fs::read(dir.join(&f)) {
            Ok(bytes) => {
                h.update(b"\0present\0");
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update(b"\0absent\0"),
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Added plus removed lines turning `old` into `new`: the length of both
/// texts less twice their longest common line subsequence.
pub fn diff_line_count(old: &str, new: &str) -> usize {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        prev = cur;
    }
    a.len() + b.len() - 2 * prev[b.len()]
}

/// Diff line count over every tracked file of either directory.
fn dir_diff_lines(installed: &Path, source: &Path) -> usize {
    let mut files = tracked_files(installed);
    for f in tracked_files(source) {
        if !files.contains(&f) {
            files.push(f);
        }
    }
    files
        .iter()
        .map(|f| {
            let read = |d: &Path| std::fs::read_to_string(d.join(f)).unwrap_or_default();
            diff_line_count(&read(installed), &read(source))
        })
        .sum()
}

pub fn read_record(dir: &Path) -> Option<InstallRecord> {
    let text = std::fs::read_to_string(dir.join(RECORD_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_record(dir: &Path, source: &Path) -> Result<()> {
    let record = InstallRecord {
        source: source.display().to_string(),
        hash: tracked_hash(dir),
    };
    let path = dir.join(RECORD_FILE);
    std::fs::write(&path, serde_json::to_string_pretty(&record)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Records where `dir` (a fresh copy) came from and what it hashed to.
pub(super) fn record_install(dir: &Path, source: &Path) -> Result<()> {
    let source = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    write_record(dir, &source)
}

fn source_is_plugin(source: &Path) -> bool {
    source.join("plugin.toml").is_file()
}

/// An installed plugin against `source` (the recorded one when `None`).
pub fn check(dir: &Path, source_override: Option<&Path>) -> Standing {
    let record = read_record(dir);
    let source: Option<PathBuf> = source_override
        .map(Path::to_path_buf)
        .or_else(|| record.as_ref().map(|r| PathBuf::from(&r.source)))
        .filter(|s| source_is_plugin(s));
    let installed = tracked_hash(dir);
    let source_hash = source.as_deref().map(tracked_hash);
    let drift = compare(
        &installed,
        record.as_ref().map(|r| r.hash.as_str()),
        source_hash.as_deref(),
    );
    Standing {
        drift,
        diff_lines: source.as_deref().map(|s| dir_diff_lines(dir, s)),
        source: source.map(|s| s.display().to_string()),
    }
}

/// Doctor's view of every plugin installed under `<home>/plugins`: a
/// `; installed vs repo copy: name state, ...` suffix (with the diff size where it drifted), and the hint
/// naming the first drifted plugin (empty when none drifted).
pub fn summarize(cat: &super::Catalog, home: &Path) -> (String, String) {
    let installed = home.join("plugins");
    let mut parts = Vec::new();
    let mut hint = String::new();
    for p in cat.plugins.values().filter(|p| p.root == installed) {
        let s = check(&p.dir, None);
        match s.diff_lines {
            Some(n) if s.drift.is_drifted() => {
                parts.push(format!(
                    "{} {} ({n} diff line(s))",
                    p.name,
                    s.drift.as_str()
                ));
                if hint.is_empty() {
                    hint = format!(
                        "forge plugin refresh {} (or --all): installed plugin files differ from the repo copy they were installed from",
                        p.name
                    );
                }
            }
            _ => parts.push(format!("{} {}", p.name, s.drift.as_str())),
        }
    }
    let summary = match parts.is_empty() {
        true => String::new(),
        false => format!("; installed vs repo copy: {}", parts.join(", ")),
    };
    (summary, hint)
}

/// What `refresh` did, or why it did not.
#[derive(Debug)]
pub enum Refresh {
    /// Already the source's files; nothing copied.
    Current,
    /// Copied. `backups` are the old files kept beside their replacements.
    Refreshed {
        backups: Vec<PathBuf>,
        diff_lines: usize,
        manifest: Manifest,
    },
    /// Refused without `--force`: the operator's edit (or an installed
    /// copy nothing recorded) would be overwritten.
    Refused { drift: Drift, diff_lines: usize },
}

fn backup_path(file: &Path) -> PathBuf {
    let stamp = crate::unix_now();
    let base = file.file_name().unwrap().to_string_lossy().into_owned();
    let mut n = 0;
    loop {
        let suffix = if n == 0 {
            format!("{base}.bak-{stamp}")
        } else {
            format!("{base}.bak-{stamp}-{n}")
        };
        let p = file.with_file_name(suffix);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// Every file under `src` a refresh copies: all but the operator's
/// `config` (top level), any install record, and earlier backups.
fn refreshable(src: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(src.join(rel))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = rel.join(&name);
        if entry.file_type()?.is_dir() {
            refreshable(src, &rel, out)?;
        } else if !(name == RECORD_FILE
            || name.contains(".bak-")
            || (rel.as_os_str() == CONFIG_FILE))
        {
            out.push(rel);
        }
    }
    Ok(())
}

/// Copies `source`'s current files over the installed `<home>/plugins/<name>`,
/// leaving `config` and `<home>/plugins-state` alone and keeping each
/// replaced file beside it as `<file>.bak-<unix time>`. Re-runs `build`.
/// An operator edit is refused unless `force`. The caller restarts the
/// plugin (it needs the store to know whether it is enabled).
pub fn refresh(home: &Path, name: &str, from: Option<&Path>, force: bool) -> Result<Refresh> {
    let dir = home.join("plugins").join(name);
    if !dir.is_dir() {
        bail!("no installed plugin named {name:?} at {}", dir.display());
    }
    let record = read_record(&dir);
    let source = match (from, &record) {
        (Some(f), _) => f.to_path_buf(),
        (None, Some(r)) => PathBuf::from(&r.source),
        (None, None) => bail!(
            "{name}: no install record says where it came from; pass --from <path to the plugin's directory>"
        ),
    };
    let manifest_path = source.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("{name}: reading {}", manifest_path.display()))?;
    let manifest = parse_manifest(&manifest_path, &text, name)
        .with_context(|| format!("{name}: the source is not a valid plugin"))?;
    let source = std::fs::canonicalize(&source).unwrap_or(source);

    let sync = check(&dir, Some(&source));
    let diff_lines = sync.diff_lines.unwrap_or(0);
    match sync.drift {
        Drift::Current => {
            write_record(&dir, &source)?;
            return Ok(Refresh::Current);
        }
        Drift::OperatorEdit | Drift::Unrecorded if !force => {
            return Ok(Refresh::Refused {
                drift: sync.drift,
                diff_lines,
            });
        }
        _ => {}
    }

    let mut files = Vec::new();
    refreshable(&source, Path::new(""), &mut files)?;
    let mut backups = Vec::new();
    for rel in &files {
        let (from, to) = (source.join(rel), dir.join(rel));
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if to.is_file() && std::fs::read(&from)? != std::fs::read(&to)? {
            let bak = backup_path(&to);
            std::fs::copy(&to, &bak)
                .with_context(|| format!("backing up {} to {}", to.display(), bak.display()))?;
            backups.push(bak);
        }
        // Replace rather than write through, so a running script is not
        // truncated under its interpreter.
        let tmp = to.with_file_name(format!(".refresh-{}", crate::unix_now()));
        std::fs::copy(&from, &tmp)
            .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
        std::fs::set_permissions(&tmp, std::fs::metadata(&from)?.permissions())?;
        std::fs::rename(&tmp, &to)?;
    }
    write_record(&dir, &source)?;
    run_build(&manifest, &dir)?;
    Ok(Refresh::Refreshed {
        backups,
        diff_lines,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_source_is_current_however_it_got_there() {
        assert_eq!(compare("a", Some("a"), Some("a")), Drift::Current);
        assert_eq!(compare("b", Some("a"), Some("b")), Drift::Current);
        assert_eq!(compare("a", None, Some("a")), Drift::Current);
    }

    #[test]
    fn untouched_install_with_a_newer_source_is_behind() {
        assert_eq!(compare("a", Some("a"), Some("b")), Drift::Behind);
    }

    #[test]
    fn differing_from_both_recorded_and_source_is_an_operator_edit() {
        assert_eq!(compare("c", Some("a"), Some("b")), Drift::OperatorEdit);
        assert_eq!(compare("c", Some("a"), Some("a")), Drift::OperatorEdit);
    }

    #[test]
    fn without_a_record_or_source_the_answer_says_which_is_missing() {
        assert_eq!(compare("a", None, Some("b")), Drift::Unrecorded);
        assert_eq!(compare("a", None, None), Drift::Unrecorded);
        assert_eq!(compare("a", Some("a"), None), Drift::SourceMissing);
    }

    #[test]
    fn only_behind_and_operator_edit_are_drifted() {
        assert!(Drift::Behind.is_drifted() && Drift::OperatorEdit.is_drifted());
        assert!(!Drift::Current.is_drifted() && !Drift::Unrecorded.is_drifted());
    }

    #[test]
    fn diff_lines_count_added_and_removed_lines() {
        assert_eq!(diff_line_count("a\nb\nc\n", "a\nb\nc\n"), 0);
        assert_eq!(diff_line_count("a\nb\nc\n", "a\nx\nc\n"), 2);
        assert_eq!(diff_line_count("a\n", "a\nb\nc\n"), 2);
        assert_eq!(diff_line_count("", "a\nb\n"), 2);
        assert_eq!(diff_line_count("a\nb\n", ""), 2);
    }

    fn plugin(root: &Path, script: &str) -> PathBuf {
        let dir = root.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "name = \"demo\"\nrun = [\"./demo.sh\"]\ncapabilities = [\"events\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("demo.sh"), script).unwrap();
        dir
    }

    #[test]
    fn the_hash_covers_the_manifest_and_the_script_but_not_other_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = plugin(t.path(), "echo 1\n");
        let base = tracked_hash(&dir);
        std::fs::write(dir.join("config"), "X=1\n").unwrap();
        std::fs::write(dir.join(RECORD_FILE), "{}").unwrap();
        assert_eq!(tracked_hash(&dir), base);
        std::fs::write(dir.join("demo.sh"), "echo 2\n").unwrap();
        assert_ne!(tracked_hash(&dir), base);
        std::fs::write(dir.join("demo.sh"), "echo 1\n").unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "name = \"demo\"\nrun = [\"./demo.sh\"]\ncapabilities = [\"events\", \"intake\"]\n",
        )
        .unwrap();
        assert_ne!(tracked_hash(&dir), base);
    }

    #[test]
    fn check_follows_an_install_through_edit_and_source_change() {
        let t = tempfile::tempdir().unwrap();
        let src = plugin(&t.path().join("repo"), "echo 1\n");
        let dest = t.path().join("home/plugins/demo");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        for f in ["plugin.toml", "demo.sh"] {
            std::fs::copy(src.join(f), dest.join(f)).unwrap();
        }
        record_install(&dest, &src).unwrap();
        assert_eq!(check(&dest, None).drift, Drift::Current);

        std::fs::write(src.join("demo.sh"), "echo 1\necho 2\n").unwrap();
        let s = check(&dest, None);
        assert_eq!((s.drift, s.diff_lines), (Drift::Behind, Some(1)));

        std::fs::write(dest.join("demo.sh"), "echo edited\n").unwrap();
        assert_eq!(check(&dest, None).drift, Drift::OperatorEdit);

        std::fs::remove_dir_all(&src).unwrap();
        assert_eq!(check(&dest, None).drift, Drift::SourceMissing);
    }
}
