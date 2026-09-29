//! `src/store/migrations.lock`: one line per shipped step, `<version>
//! <sha256 of its SQL>`. A live store has already run every locked step,
//! so an edited, reordered or removed one fails here instead of in
//! deploy-self's doctor against the live store (bc36fdb put the chat
//! tables before `workers.slots`, which live stores had applied as step
//! 82, and step 83 re-added `slots`). New steps are only ever appended,
//! and appending their hashes is the one change the lock takes.

use super::MIGRATIONS;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Set to append the hashes of steps the lock does not have yet.
const APPEND_ENV: &str = "FORGE_LOCK_MIGRATIONS";
const APPEND_COMMAND: &str =
    "FORGE_LOCK_MIGRATIONS=1 cargo test --bin forge migrations_lock_matches_every_shipped_step";

fn hash(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

fn lock_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/store/migrations.lock")
}

/// The lock's `(version, hash)` rows; `#` lines and blank lines are
/// commentary.
fn parse(lock: &str) -> Result<Vec<(usize, String)>, String> {
    lock.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (v, h) = l
                .split_once(' ')
                .ok_or_else(|| format!("migrations.lock: malformed line `{l}`"))?;
            let v = v
                .parse::<usize>()
                .map_err(|_| format!("migrations.lock: malformed version in `{l}`"))?;
            Ok((v, h.trim().to_string()))
        })
        .collect()
}

/// The lines to append for the steps after the locked ones, or why the
/// ladder no longer matches the lock: a locked step whose SQL changed
/// (edited, or another step moved into its place) or that is gone.
fn check(lock: &str, steps: &[&str]) -> Result<Vec<String>, String> {
    let rows = parse(lock)?;
    for (i, (v, _)) in rows.iter().enumerate() {
        if *v != i + 1 {
            return Err(format!(
                "migrations.lock: line {} names step {v}, expected {}; the lock is only appended to",
                i + 1,
                i + 1
            ));
        }
    }
    for (v, want) in &rows {
        let Some(sql) = steps.get(v - 1) else {
            return Err(format!(
                "step {v} is locked but MIGRATIONS has only {} steps: a shipped step was removed. \
                 Live stores have run it; restore it and append new steps after it",
                steps.len()
            ));
        };
        if hash(sql) != *want {
            return Err(format!(
                "step {v} (`{}`) is not the SQL the lock shipped: a shipped step was edited, \
                 or another step was moved into its place. Live stores have already run step {v}; \
                 restore the base's steps in their order and append new ones after them",
                super::first_line(sql)
            ));
        }
    }
    Ok(steps
        .iter()
        .enumerate()
        .skip(rows.len())
        .map(|(i, sql)| format!("{} {}", i + 1, hash(sql)))
        .collect())
}

#[test]
fn migrations_lock_matches_every_shipped_step() {
    let path = lock_path();
    let lock = std::fs::read_to_string(&path).unwrap_or_default();
    let append = match check(&lock, MIGRATIONS) {
        Ok(a) => a,
        Err(e) => panic!("{e}"),
    };
    if append.is_empty() {
        return;
    }
    if std::env::var_os(APPEND_ENV).is_some() {
        let mut text = lock;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for line in &append {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        eprintln!("appended {} step(s) to {}", append.len(), path.display());
    } else {
        eprintln!(
            "{} new migration step(s) not in src/store/migrations.lock; append their hashes with:\n  {APPEND_COMMAND}",
            append.len()
        );
    }
}

fn lock_of(steps: &[&str]) -> String {
    let mut s = String::from("# test lock\n");
    for (i, sql) in steps.iter().enumerate() {
        s.push_str(&format!("{} {}\n", i + 1, hash(sql)));
    }
    s
}

const A: &str = "\nCREATE TABLE a (id INTEGER);\n";
const B: &str = "\nALTER TABLE workers ADD COLUMN slots INTEGER NOT NULL DEFAULT 0;\n";
const C: &str = "\nCREATE TABLE chat_sessions (id INTEGER);\n";

#[test]
fn appending_steps_passes_and_names_the_new_hashes() {
    let lock = lock_of(&[A, B]);
    assert_eq!(check(&lock, &[A, B]), Ok(vec![]));
    assert_eq!(check(&lock, &[A, B, C]), Ok(vec![format!("3 {}", hash(C))]));
}

#[test]
fn reordering_shipped_steps_fails() {
    // bc36fdb: the branch's step went in before the base's shipped one.
    let lock = lock_of(&[A, B]);
    let e = check(&lock, &[A, C, B]).unwrap_err();
    assert!(e.contains("step 2"), "{e}");
    assert!(e.contains("CREATE TABLE chat_sessions"), "{e}");
}

#[test]
fn editing_a_shipped_step_fails() {
    let lock = lock_of(&[A, B]);
    let edited = "\nALTER TABLE workers ADD COLUMN slots INTEGER NOT NULL DEFAULT 1;\n";
    let e = check(&lock, &[A, edited]).unwrap_err();
    assert!(e.contains("step 2"), "{e}");
}

#[test]
fn removing_a_shipped_step_fails() {
    let lock = lock_of(&[A, B, C]);
    assert!(check(&lock, &[A, B]).unwrap_err().contains("step 3"));
    assert!(check(&lock, &[A, C]).is_err());
}

#[test]
fn a_lock_with_a_gap_or_duplicate_fails() {
    let lock = format!("1 {}\n3 {}\n", hash(A), hash(B));
    assert!(check(&lock, &[A, B, C]).is_err());
    let lock = format!("1 {}\n1 {}\n", hash(A), hash(A));
    assert!(check(&lock, &[A, B]).is_err());
}
