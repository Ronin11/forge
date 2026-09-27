//! A catalog file that shares a built-in action's name shadows the
//! built-in (docs/WORKFLOWS.md, "Authoring"). A copy that is only a stale
//! seed (written by an old `ensure`, never edited by the operator) must not
//! shadow later changes to the built-in, so it is told apart from an
//! operator edit by the catalog's git history: an edit is a commit on that
//! file that is not a seeding one (author `forge`, or a message starting
//! `catalog: built-in`).

use super::{BUILTIN_ACTIONS, BUILTIN_OPERATIONS};
use anyhow::{Result, bail};
use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

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
    /// The unified diff, built-in to catalog copy.
    pub diff: String,
}

impl Shadow {
    /// Added and removed lines in the diff.
    pub fn diff_lines(&self) -> usize {
        self.diff
            .lines()
            .filter(|l| {
                (l.starts_with('+') && !l.starts_with("+++"))
                    || (l.starts_with('-') && !l.starts_with("---"))
            })
            .count()
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

fn diff_of(builtin: &str, copy: &Path) -> String {
    let tmp = std::env::temp_dir().join(format!(
        "forge-builtin-{}-{}",
        std::process::id(),
        copy.file_name().and_then(|n| n.to_str()).unwrap_or("x")
    ));
    if std::fs::write(&tmp, builtin).is_err() {
        return String::new();
    }
    let o = Command::new("git")
        .args(["diff", "--no-index", "--no-color", "--"])
        .arg(&tmp)
        .arg(copy)
        .output();
    let _ = std::fs::remove_file(&tmp);
    o.map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Every catalog action file sharing a built-in's name, sorted by file name.
pub fn scan(catalog: &Path) -> Vec<Shadow> {
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
        out.push(Shadow {
            file: file.to_string(),
            origin,
            age_secs,
            diff: diff_of(builtin, &path),
        });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
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
            format!(
                "{} ({}, {} old, {} diff line(s))",
                s.file,
                s.origin.as_str(),
                age_text(s.age_secs),
                s.diff_lines()
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
