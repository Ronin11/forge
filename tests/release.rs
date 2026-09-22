//! scripts/release.sh packs the workspace's own binaries into every release
//! archive (see docs/OPS.md, "Releases"). This derives that set from every
//! member's Cargo.toml — a `[[bin]]` table, or the package name when a
//! crate builds a default `src/main.rs` binary — and fails if the script's
//! own `BINS=` list drifts from it, so a new crate that ships a binary
//! cannot be forgotten the way the portal went a week unchecked in the
//! client boundary (see tests/boundary.rs).

use std::collections::BTreeSet;
use std::path::Path;

fn bin_names(manifest_dir: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    let mut names = BTreeSet::new();
    if let Some(bins) = manifest.get("bin").and_then(|b| b.as_array()) {
        for b in bins {
            names.insert(b["name"].as_str().unwrap().to_string());
        }
    } else if manifest_dir.join("src/main.rs").exists() {
        names.insert(manifest["package"]["name"].as_str().unwrap().to_string());
    }
    names
}

/// Every binary the workspace builds: the root package plus each member,
/// skipping a crate like `client` that ships only a library.
fn workspace_binaries(root: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    let members = manifest["workspace"]["members"]
        .as_array()
        .expect("[workspace] members is an array");
    let mut out = bin_names(root);
    for m in members {
        out.extend(bin_names(&root.join(m.as_str().unwrap())));
    }
    out
}

/// The `BINS="..."` line in scripts/release.sh, parsed as text so this test
/// exercises exactly what the script packs, not a copy of it.
fn packed_binaries(root: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(root.join("scripts/release.sh")).unwrap();
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("BINS="))
        .expect("scripts/release.sh declares BINS=\"...\"");
    let quoted = line
        .split('"')
        .nth(1)
        .expect("BINS=\"...\" is a quoted, space-separated list");
    quoted.split_whitespace().map(|s| s.to_string()).collect()
}

#[test]
fn the_release_archive_packs_every_workspace_binary_and_no_others() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        packed_binaries(root),
        workspace_binaries(root),
        "scripts/release.sh's BINS list has drifted from the workspace's own binaries"
    );
}
