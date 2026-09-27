//! A catalog file that shares a built-in action's name shadows the
//! built-in (docs/WORKFLOWS.md, "Authoring"). A copy that is only a stale
//! seed (written by an old `ensure`, never edited by the operator) must not
//! shadow later changes to the built-in, so it is told apart from an
//! operator edit by the catalog's git history: an edit is a commit on that
//! file that is not a seeding one (author `forge`, or a message starting
//! `catalog: built-in`).

use super::{BUILTIN_ACTIONS, BUILTIN_OPERATIONS};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
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
        let origin = if has_operator_commit(catalog, &rel) {
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
/// caches the result in the process, keyed on `state_key`, so a load that
/// finds nothing changed costs a hash comparison, not a diff per file.
///
/// A result holding a failed diff is never cached: the failure is meant to
/// be transient (a killed `git diff`), and caching it would keep reporting
/// the same failure until `HEAD` or an mtime changed, long after `git`
/// itself had recovered.
pub fn scan(catalog: &Path) -> Vec<Shadow> {
    static CACHE: Mutex<Option<(PathBuf, String, Vec<Shadow>)>> = Mutex::new(None);
    let key = state_key(catalog);
    let mut cache = CACHE.lock().unwrap();
    if let Some((dir, k, shadows)) = cache.as_ref()
        && dir == catalog
        && *k == key
    {
        return shadows.clone();
    }
    let out = scan_uncached(catalog);
    if out.iter().all(|s| s.diff.is_ok()) {
        *cache = Some((catalog.to_path_buf(), key, out.clone()));
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
/// edit is listed but is not a fault.
pub fn report(catalog: &Path) -> (bool, String, String) {
    let all = scan(catalog);
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

    // `scan`'s cache is a single process-wide slot (see `scan`'s doc
    // comment), so two tests calling `scan` on different catalogs at once
    // can evict each other's cached entry; this one test exercises both
    // the caching and the failed-diff behavior in sequence, on one thread,
    // rather than risk that race across separate `#[test]` functions.
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
