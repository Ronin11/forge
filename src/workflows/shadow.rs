//! A catalog file that shares a built-in action's name shadows the
//! built-in (docs/WORKFLOWS.md, "Authoring"). A copy that is only a stale
//! seed (written by an old `ensure`, never edited by the operator) must not
//! shadow later changes to the built-in, so it is told apart from an
//! operator edit by the catalog's git history: an edit is a commit on that
//! file that is not a seeding one (author `forge`, or a message starting
//! `catalog: built-in`).

use super::{BUILTIN_ACTIONS, BUILTIN_OPERATIONS};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

const SEED_MESSAGE: &str = "catalog: built-in";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    StaleSeed,
    OperatorEdit,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::StaleSeed => "stale seed",
            Origin::OperatorEdit => "operator edit",
        }
    }
}

/// One catalog action file sharing a built-in's name (identical or not).
#[derive(Clone, Debug)]
pub struct Shadow {
    /// File name under `actions/`, e.g. `deploy-self.toml`.
    pub file: String,
    pub origin: Origin,
    pub age_secs: u64,
    /// The copy has no real difference from the built-in (see `equivalent`).
    /// Such a copy is always a stale seed, whatever its history says.
    pub equivalent: bool,
    /// The unified diff, built-in to catalog copy, or the reason `git diff`
    /// could not produce one. `Err` must never be folded into an empty or
    /// zero-line diff: that would look exactly like "no differences".
    pub diff: Result<String, String>,
}

impl Shadow {
    /// Added and removed lines in the diff; `0` when the diff failed, so
    /// callers that report a failure must check `diff` itself and not rely
    /// on this alone.
    pub fn diff_lines(&self) -> usize {
        self.diff.as_deref().map_or(0, |diff| {
            diff.lines()
                .filter(|l| {
                    (l.starts_with('+') && !l.starts_with("+++"))
                        || (l.starts_with('-') && !l.starts_with("---"))
                })
                .count()
        })
    }
}

/// A comparable form of `text` that ignores whitespace and comments: each
/// line trimmed and its whitespace runs collapsed, blank lines and
/// comment-only lines dropped.
fn squeezed(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// Whether a catalog copy says nothing the built-in does not: its text is
/// the built-in's byte for byte, or differs only in whitespace or comments.
/// Such a copy is a stale seed, not an operator edit, however it got there.
/// The order matters: identical text is equivalent; when both texts parse as
/// TOML, their parsed values decide and nothing else (a `#` line inside a
/// multi-line string is content, not a comment); only when at least one fails
/// to parse do the lines compare once whitespace and comment-only lines are
/// set aside.
pub fn equivalent(builtin: &str, copy: &str) -> bool {
    if builtin == copy {
        return true;
    }
    if let (Ok(a), Ok(b)) = (
        toml::from_str::<toml::Value>(builtin),
        toml::from_str::<toml::Value>(copy),
    ) {
        return a == b;
    }
    squeezed(builtin) == squeezed(copy)
}

/// The git blob hash of `text`, as `git hash-object` gives it.
pub(super) fn text_blob_hash(text: &str) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        bail!("git hash-object --stdin failed");
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Whether the file's history holds a commit that is not a seeding one.
fn has_operator_commit(dir: &Path, rel: &str) -> bool {
    let Ok(o) = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "--format=%an%x09%s", "--", rel])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&o.stdout).lines().any(|l| {
        let (author, subject) = l.split_once('\t').unwrap_or((l, ""));
        !author.eq_ignore_ascii_case("forge") && !subject.starts_with(SEED_MESSAGE)
    })
}

/// The unified diff between a built-in and a catalog copy, run through a
/// private, uniquely-named temp file (`tempfile::NamedTempFile`, removed on
/// drop): several catalog loads run at once in one worker process, and a
/// name built only from the pid (the old approach) is shared by every call
/// in that process, so one call's `fs::write` truncation or final
/// `remove_file` could land while another call's `git diff` still had the
/// file memory-mapped — git then died of SIGBUS reading a hole where the
/// mapped file used to be.
///
/// `Err` names a git failure (killed by a signal, or an exit code that is
/// neither 0 nor 1): callers must log and report it, never fold it into an
/// empty diff, since an empty diff already means "no differences" and
/// would silently hide a real one.
fn diff_of(builtin: &str, copy: &Path) -> Result<String> {
    let mut tmp = tempfile::Builder::new()
        .prefix("forge-builtin-")
        .suffix(copy.file_name().and_then(|n| n.to_str()).unwrap_or("x"))
        .tempfile()
        .context("creating a temp file for the built-in diff")?;
    tmp.write_all(builtin.as_bytes())
        .context("writing the built-in to a temp file")?;
    let o = Command::new("git")
        .args(["diff", "--no-index", "--no-color", "--"])
        .arg(tmp.path())
        .arg(copy)
        .output()
        .context("running git diff --no-index")?;
    match o.status.code() {
        // `--no-index` exits 0 (identical) or 1 (differences found); both
        // are a successful diff, not a failure.
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Some(code) => bail!(
            "git diff --no-index exited {code}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        None => bail!(
            "git diff --no-index killed by signal {}",
            o.status.signal().unwrap_or(-1)
        ),
    }
}

/// The newest modification time among the catalog's action files (and the
/// directory itself, so an added or removed file counts too), used to tell
/// whether a cached `scan` is still good.
fn newest_mtime(actions: &Path) -> SystemTime {
    let mut newest = SystemTime::UNIX_EPOCH;
    for meta in std::fs::metadata(actions).ok().into_iter().chain(
        std::fs::read_dir(actions)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok()),
    ) {
        if let Ok(t) = meta.modified() {
            newest = newest.max(t);
        }
    }
    newest
}

/// A key that changes whenever the catalog's shadowing state might have:
/// the repository's `HEAD` (an operator edit is told apart by its commit
/// history) and the newest file mtime under `actions/` (a change not yet
/// committed). Cheap next to `scan_uncached`'s one `git diff` per built-in.
fn state_key(catalog: &Path) -> String {
    let head = Command::new("git")
        .arg("-C")
        .arg(catalog)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mtime = newest_mtime(&catalog.join("actions"))
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{head}:{mtime}")
}

fn scan_uncached(catalog: &Path) -> Vec<Shadow> {
    let actions = catalog.join("actions");
    let mut out = Vec::new();
    for (file, builtin) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let path = actions.join(file);
        if !path.is_file() {
            continue;
        };
        let rel = format!("actions/{file}");
        let equivalent = std::fs::read_to_string(&path).is_ok_and(|t| equivalent(builtin, &t));
        let origin = if !equivalent && has_operator_commit(catalog, &rel) {
            Origin::OperatorEdit
        } else {
            Origin::StaleSeed
        };
        let age_secs = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(0, |d| d.as_secs());
        let diff = diff_of(builtin, &path).map_err(|e| {
            eprintln!("forge: diffing actions/{file} against its built-in failed: {e:#}");
            format!("{e:#}")
        });
        out.push(Shadow {
            file: file.to_string(),
            origin,
            age_secs,
            equivalent,
            diff,
        });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}

/// Every catalog action file sharing a built-in's name, sorted by file
/// name. A task start (`load_catalog`) and `forge doctor` each call this
/// once per catalog load, so — with four workers starting tasks at once —
/// the same catalog state was being diffed several times a second; this
/// caches the result in the process, keyed on the catalog path and then on
/// `state_key`, so a load that finds nothing changed costs a hash
/// comparison, not a diff per file, and different catalogs never evict each
/// other's cached entry.
///
/// A result holding a failed diff is never cached: the failure is meant to
/// be transient (a killed `git diff`), and caching it would keep reporting
/// the same failure until `HEAD` or an mtime changed, long after `git`
/// itself had recovered.
type ScanCache = HashMap<PathBuf, (String, Vec<Shadow>)>;

pub fn scan(catalog: &Path) -> Vec<Shadow> {
    static CACHE: Mutex<Option<ScanCache>> = Mutex::new(None);
    let key = state_key(catalog);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((k, shadows)) = cache.get(catalog)
        && *k == key
    {
        return shadows.clone();
    }
    let out = scan_uncached(catalog);
    if out.iter().all(|s| s.diff.is_ok()) {
        cache.insert(catalog.to_path_buf(), (key, out.clone()));
    }
    out
}

/// Stale seeds among the catalog's files; the loader skips these. Each is
/// noted once per process on stderr.
pub(super) fn stale_seeds(catalog: &Path) -> HashSet<String> {
    static NOTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut out = HashSet::new();
    for s in scan(catalog) {
        if s.origin != Origin::StaleSeed {
            continue;
        }
        let key = format!("{}:{}", catalog.display(), s.file);
        let mut noted = NOTED.lock().unwrap();
        if noted.get_or_insert_with(HashSet::new).insert(key) {
            eprintln!(
                "forge: ignoring stale seed actions/{} in the catalog (it shadows the built-in and was never edited); `forge workflows refresh` removes it",
                s.file
            );
        }
        out.insert(s.file);
    }
    out
}

/// Age as the doctor and refresh print it: `3d`, `5h`, `12m`.
pub fn age_text(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

/// Delete a catalog copy so the built-in applies again, committing the
/// removal when the file was tracked.
pub async fn remove(catalog: &Path, file: &str) -> Result<()> {
    let rel = format!("actions/{file}");
    let tracked = Command::new("git")
        .arg("-C")
        .arg(catalog)
        .args(["ls-files", "--error-unmatch", "--", &rel])
        .output()?
        .status
        .success();
    std::fs::remove_file(catalog.join(&rel))?;
    if tracked {
        crate::git::commit_path(
            catalog,
            &rel,
            &format!("{SEED_MESSAGE} {file}: catalog copy removed by forge workflows refresh"),
        )
        .await?;
    }
    Ok(())
}

/// The doctor's shadowing row: whether to warn, the detail, and the hint.
/// A stale seed warns and points at `forge workflows refresh`; an operator
/// edit is listed but is not a fault. Only copies with real diff lines are
/// listed: one that does not differ from the built-in (or only in
/// whitespace or comments) is a seed `refresh` removes without a flag, not
/// something to read; a failed diff is still listed, as it hides nothing.
pub fn report(catalog: &Path) -> (bool, String, String) {
    let all: Vec<Shadow> = scan(catalog)
        .into_iter()
        .filter(|s| !s.equivalent || s.diff.is_err())
        .collect();
    if all.is_empty() {
        return (
            false,
            "no catalog file shadows a built-in".into(),
            String::new(),
        );
    }
    let detail = all
        .iter()
        .map(|s| {
            let state = match &s.diff {
                Ok(_) => format!("{} diff line(s)", s.diff_lines()),
                Err(e) => format!("diff failed: {e}"),
            };
            format!(
                "{} ({}, {} old, {})",
                s.file,
                s.origin.as_str(),
                age_text(s.age_secs),
                state
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let stale = all.iter().any(|s| s.origin == Origin::StaleSeed);
    let hint = if stale { "forge workflows refresh" } else { "" };
    (stale, detail, hint.into())
}

/// The `shadowing` row of `forge doctor`.
pub fn doctor_check(home: &Path) -> crate::doctor::Check {
    use crate::doctor::{Check, Status};
    let (stale, detail, hint) = report(&home.join("workflows"));
    Check {
        name: "shadowing".into(),
        status: if stale { Status::Warn } else { Status::Ok },
        detail,
        hint,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const BUILTIN: &str =
        "name = \"fmt\"\n# the formatter\ndescription = \"format\"\nrun = [\"cargo fmt\"]\n";

    #[test]
    fn a_byte_identical_copy_is_equivalent() {
        assert!(equivalent(BUILTIN, BUILTIN));
    }

    #[test]
    fn a_copy_differing_only_in_whitespace_or_comments_is_equivalent() {
        let spaced =
            "name   = \"fmt\"\n\n\n  description = \"format\"   \nrun = [ \"cargo fmt\" ]\n";
        assert!(equivalent(BUILTIN, spaced));
        let no_comment = "name = \"fmt\"\ndescription = \"format\"\nrun = [\"cargo fmt\"]\n";
        assert!(equivalent(BUILTIN, no_comment));
        let commented = "# operator note\nname = \"fmt\"\n# the formatter\ndescription = \"format\" # trailing\nrun = [\"cargo fmt\"]\n";
        assert!(equivalent(BUILTIN, commented));
    }

    #[test]
    fn a_copy_with_a_changed_value_or_key_is_not_equivalent() {
        let changed = BUILTIN.replace("format", "EDITED format");
        assert!(!equivalent(BUILTIN, &changed));
        let extra = format!("{BUILTIN}timeout_secs = 60\n");
        assert!(!equivalent(BUILTIN, &extra));
        let joined = BUILTIN.replace("cargo fmt", "cargofmt");
        assert!(!equivalent(BUILTIN, &joined));
    }

    #[test]
    fn a_hash_line_inside_a_multiline_string_is_content_not_a_comment() {
        let builtin = "name = \"x\"\nscript = \"\"\"\n    User root\n\"\"\"\n";
        let edited =
            "name = \"x\"\nscript = \"\"\"\n    User root\n#StrictHostKeyChecking no\n\"\"\"\n";
        assert!(!equivalent(builtin, edited));
        let changed = edited.replace("no\n", "yes\n");
        assert!(!equivalent(edited, &changed));
        let outside =
            "# a real comment\nname = \"x\"\nscript = \"\"\"\n    User root\n\"\"\"\n# another\n";
        assert!(equivalent(builtin, outside));
    }

    #[test]
    fn an_unparseable_copy_is_compared_line_by_line() {
        let broken = "name = \"fmt\n# c\n";
        assert!(equivalent(broken, "name = \"fmt\n\n"));
        assert!(!equivalent(broken, "name = \"fmt2\n"));
    }

    #[test]
    fn scan_treats_an_operator_committed_equivalent_copy_as_a_stale_seed() {
        let home = tempfile::tempdir().unwrap();
        let catalog = crate::workflows::catalog_dir(home.path()).unwrap();
        let (_, builtin) = BUILTIN_OPERATIONS
            .iter()
            .find(|(f, _)| *f == "fmt.toml")
            .unwrap();
        std::fs::write(
            catalog.join("actions/fmt.toml"),
            format!("# mine\n{builtin}"),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&catalog)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{o:?}");
        };
        git(&["add", "actions/fmt.toml"]);
        git(&[
            "-c",
            "user.name=operator",
            "-c",
            "user.email=o@x",
            "commit",
            "-qm",
            "note",
        ]);
        let found = scan(&catalog);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].equivalent);
        assert_eq!(found[0].origin, Origin::StaleSeed);
        let (stale, detail, _) = report(&catalog);
        assert!(!stale, "{detail}");
        assert!(!detail.contains("fmt.toml"), "{detail}");
    }

    /// Eight threads diffing the same built-in against the same catalog
    /// copy at once used to race on a temp path shared by `std::process::
    /// id()` alone: one call's `fs::write` or `remove_file` could land
    /// under another call's still-running `git diff`, and `unwrap_or_
    /// default` turned that failure into a silent, wrongly-empty diff.
    /// `diff_of`'s temp file is now unique per call, so every thread must
    /// see the true, non-empty diff.
    #[test]
    fn diff_of_from_eight_threads_at_once_is_never_empty_or_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("fmt.toml");
        std::fs::write(&copy, "name = \"fmt\"\ndescription = \"catalog copy\"\n").unwrap();
        let builtin = Arc::new(String::from("name = \"fmt\"\ndescription = \"built-in\"\n"));
        let copy = Arc::new(copy);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let builtin = Arc::clone(&builtin);
                let copy = Arc::clone(&copy);
                std::thread::spawn(move || diff_of(&builtin, &copy))
            })
            .collect();
        for h in handles {
            let diff = h.join().unwrap().unwrap_or_else(|e| panic!("{e:#}"));
            assert!(diff.contains("-description = \"built-in\""), "{diff}");
            assert!(diff.contains("+description = \"catalog copy\""), "{diff}");
        }
    }

    // `scan`'s cache is keyed per catalog path, so unrelated catalogs used
    // by other tests running in parallel can't evict this one's entry; this
    // test still exercises both the caching and the failed-diff behavior in
    // sequence, on one thread, since it needs to observe cache hits and
    // misses in a specific order.
    #[test]
    fn scan_caches_on_success_but_never_on_a_failed_diff() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let catalog = crate::workflows::catalog_dir(home.path()).unwrap();
        assert!(scan(&catalog).is_empty());
        let copy = catalog.join("actions/fmt.toml");
        std::fs::write(&copy, "name = \"fmt\"\ndescription = \"catalog copy\"\n").unwrap();
        let first = scan(&catalog);
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(first[0].diff_lines() > 0, "{first:?}");
        let second = scan(&catalog);
        assert_eq!(second.len(), first.len());
        assert_eq!(second[0].diff, first[0].diff);

        // An unreadable copy makes real `git diff --no-index` exit 128.
        // That failure must be reported, not folded into `0 diff line(s)`
        // (indistinguishable from "no differences", the bug this guards
        // against), and must not be cached: once the copy is readable
        // again, the very next `scan` must see the real diff rather than
        // the stuck failure. A fresh catalog, so the failure is the very
        // first `scan` result for this `state_key` and not shadowed by the
        // success just cached above (`chmod` alone leaves the mtime the
        // `state_key` hashes on unchanged).
        let home2 = tempfile::tempdir().unwrap();
        let catalog2 = crate::workflows::catalog_dir(home2.path()).unwrap();
        let copy2 = catalog2.join("actions/fmt.toml");
        std::fs::write(&copy2, "name = \"fmt\"\ndescription = \"catalog copy\"\n").unwrap();
        std::fs::set_permissions(&copy2, std::fs::Permissions::from_mode(0o000)).unwrap();
        let failed = scan(&catalog2);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].diff.is_err(), "{:?}", failed[0].diff);
        assert_eq!(failed[0].diff_lines(), 0);
        let (stale, detail, _hint) = report(&catalog2);
        assert!(stale);
        assert!(detail.contains("diff failed"), "{detail}");
        assert!(!detail.contains("0 diff line(s)"), "{detail}");

        std::fs::set_permissions(&copy2, std::fs::Permissions::from_mode(0o644)).unwrap();
        let recovered = scan(&catalog2);
        assert!(recovered[0].diff.is_ok(), "{:?}", recovered[0].diff);
        assert!(recovered[0].diff_lines() > 0, "{recovered:?}");
    }
}
