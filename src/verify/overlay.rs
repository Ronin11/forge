//! The verification namespace overlay: placing the hidden suites from the
//! trusted refs into a worktree before L1 runs, and removing them
//! afterwards so the next attempt starts blind.

use super::*;
use anyhow::Result;
use std::path::PathBuf;

/// Overlay refs for a human: a pinned forge-verify commit reads as
/// `forge-verify@<sha8>`, everything else as itself.
pub fn overlay_label(refs: &[String]) -> String {
    refs.iter()
        .map(|r| {
            if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
                format!("forge-verify@{}", &r[..8])
            } else {
                r.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn in_namespace(namespace: &[String], path: &str) -> bool {
    namespace.iter().any(|d| path.starts_with(d.as_str()))
}

/// Overlay the namespace files from each trusted ref into the tree.
/// Returns the files placed, for removal afterwards.
pub async fn overlay(
    repo: &Path,
    refs: &[String],
    namespace: &[String],
    dest: &Path,
) -> Result<Vec<PathBuf>> {
    let mut placed = Vec::new();
    if namespace.is_empty() {
        return Ok(placed);
    }
    let mut listed = Vec::new();
    for r in refs {
        listed.push((r, crate::git::ls_tree(repo, r, namespace).await?));
    }
    let names: Vec<&String> = listed.iter().flat_map(|(_, f)| f.iter()).collect();
    record_overlay(dest, namespace, &names)?;
    for (r, files) in &listed {
        crate::git::archive_into(repo, r, files, dest).await?;
        placed.extend(files.iter().map(|f| dest.join(f)));
    }
    Ok(placed)
}

/// Written before the first overlay file is: the namespace directories
/// (`D`) and every file about to be placed (`F`), one per line.
fn record_overlay(dest: &Path, namespace: &[String], files: &[&String]) -> Result<()> {
    let path = crate::git::overlay_manifest_path(dest);
    if !path.parent().is_some_and(Path::is_dir) {
        return Ok(());
    }
    let mut text = String::new();
    for d in namespace {
        text.push_str(&format!("D {}\n", d.trim_end_matches('/')));
    }
    for f in files {
        text.push_str(&format!("F {f}\n"));
    }
    if let Ok(old) = std::fs::read_to_string(&path) {
        text = format!("{old}{text}");
    }
    std::fs::write(&path, text)?;
    Ok(())
}

pub fn remove_overlay(placed: &[PathBuf], namespace: &[String], dest: &Path) {
    for f in placed {
        let _ = std::fs::remove_file(f);
    }
    for d in namespace {
        let dir = dest.join(d.trim_end_matches('/'));
        let _ = crate::git::remove_empty_dirs(&dir);
    }
    let _ = std::fs::remove_file(crate::git::overlay_manifest_path(dest));
}

/// Names the kernel's own verification overlay among dirty paths, so the
/// clean-tree failure does not read as the agent's mess.
pub(super) fn overlay_note(dirty: &[String], namespace: &[String]) -> String {
    let ours: Vec<&str> = dirty
        .iter()
        .filter(|p| in_namespace(namespace, p))
        .map(String::as_str)
        .collect();
    if ours.is_empty() {
        return String::new();
    }
    format!(
        " (Forge's own verification overlay, not the agent's: {}; a worker that died between overlay and cleanup leaves it, and Forge removes it when the next attempt starts)",
        ours.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_membership() {
        let ns = vec!["tests/acceptance/".to_string()];
        assert!(in_namespace(&ns, "tests/acceptance/a.sh"));
        assert!(!in_namespace(&ns, "tests/acceptance.sh"));
        assert!(!in_namespace(&ns, "src/a.ts"));
    }
}
