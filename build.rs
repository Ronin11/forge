//! Captures the short commit hash at build time so `forge version` can
//! report exactly which checkout produced the binary. A build from a
//! tree with no `.git` (the archive `forge deploy` builds a landed
//! commit in, see docs/DEPLOY.md) names its commit in `FORGE_BUILD_SHA`.
//! `FORGE_GIT_SHA_FULL` is the full hash, the id `deploy-self` names its
//! release by, so `init --relink` names the same commit the same way.

fn main() {
    println!("cargo:rerun-if-env-changed=FORGE_BUILD_SHA");
    let named = std::env::var("FORGE_BUILD_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let rev_parse = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let sha = named
        .clone()
        .unwrap_or_else(|| rev_parse(&["rev-parse", "--short", "HEAD"]));
    let full = named.unwrap_or_else(|| rev_parse(&["rev-parse", "HEAD"]));
    println!("cargo:rustc-env=FORGE_GIT_SHA={sha}");
    println!("cargo:rustc-env=FORGE_GIT_SHA_FULL={full}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    builtin_history();
}

/// Writes `builtin_history.rs` to `OUT_DIR` from the checked-in
/// `src/builtins/history.tsv` (`scripts/builtin-history.sh` regenerates it):
/// for every built-in action file, the blob hashes it has had and the time
/// (unix seconds) it last changed. `forge doctor` uses it to tell a catalog
/// copy of an old built-in that the built-in has since outgrown. The table
/// is a file and not a `git log` at build time because `forge deploy` builds
/// from a `git archive` tree that has no history.
fn builtin_history() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let table = std::fs::read_to_string("src/builtins/history.tsv").unwrap_or_default();
    let mut files: std::collections::BTreeMap<String, (i64, Vec<String>)> = Default::default();
    for line in table.lines() {
        let mut cols = line.split('\t');
        let (Some(name), Some(when), Some(hash)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let e = files.entry(name.to_string()).or_default();
        e.0 = e.0.max(when.parse().unwrap_or(0));
        e.1.push(hash.to_string());
    }
    let mut src = String::from("&[\n");
    for (name, (changed, hashes)) in &files {
        src.push_str(&format!("    ({name:?}, {changed}, &{hashes:?}),\n"));
    }
    src.push_str("]\n");
    std::fs::write(std::path::Path::new(&out).join("builtin_history.rs"), src)
        .expect("write builtin_history.rs");
    println!("cargo:rerun-if-changed=src/builtins/history.tsv");
}
