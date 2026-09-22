//! The module graph as data (docs/LATER.md, "The code visualiser"): files
//! and symbols as nodes, import edges between them, files grouped under
//! their directories as module nodes. Built from `forge-repomap edges`
//! (repomap/src/edges.rs), which already extracts every file's symbol
//! count and the imports that resolve inside the tree; this only adds
//! line counts (read straight off disk, since the argument is a working
//! tree, not a bare tree) and the directory grouping. Deterministic end
//! to end: no model, no store.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Node {
    pub path: String,
    pub kind: String,
    pub symbols: usize,
    pub lines: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Deserialize)]
struct RawNode {
    path: String,
    symbols: usize,
}

#[derive(Deserialize)]
struct RawEdge {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct RawGraph {
    nodes: Vec<RawNode>,
    edges: Vec<RawEdge>,
}

/// Where `forge-repomap` lives: beside this binary, exactly as `ctx.rs`
/// finds it for a sandboxed attempt and `operation.rs` hands it to a
/// built-in operation as `FORGE_BIN_DIR`.
pub fn repomap_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the forge binary")?;
    let dir = exe
        .parent()
        .context("the forge binary has no parent directory")?;
    Ok(dir.join("forge-repomap"))
}

/// Every file's immediate parent directory becomes a module node, its
/// `symbols`/`lines` the sum over the files grouped under it. A
/// root-level file (no parent) joins no module. Import edges pass
/// through unchanged.
fn group(raw: RawGraph, repo: &Path) -> Graph {
    let mut nodes = Vec::with_capacity(raw.nodes.len());
    let mut modules: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for n in raw.nodes {
        let lines = std::fs::read_to_string(repo.join(&n.path))
            .map(|t| t.lines().count())
            .unwrap_or(0);
        let module = Path::new(&n.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty());
        if let Some(module) = module {
            let entry = modules.entry(module).or_default();
            entry.0 += n.symbols;
            entry.1 += lines;
        }
        nodes.push(Node {
            path: n.path,
            kind: "file".to_string(),
            symbols: n.symbols,
            lines,
        });
    }
    for (path, (symbols, lines)) in modules {
        nodes.push(Node {
            path,
            kind: "module".to_string(),
            symbols,
            lines,
        });
    }
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    let edges = raw
        .edges
        .into_iter()
        .map(|e| Edge {
            from: e.from,
            to: e.to,
        })
        .collect();
    Graph { nodes, edges }
}

/// The module graph for a repository at its working tree: shells out to
/// `forge-repomap edges`, then groups the result the way `group` does.
pub fn build(repo: &Path, repomap_bin: &Path) -> Result<Graph> {
    let out = std::process::Command::new(repomap_bin)
        .arg("edges")
        .arg(repo)
        .output()
        .with_context(|| format!("running {} edges {}", repomap_bin.display(), repo.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "forge-repomap edges failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let raw: RawGraph =
        serde_json::from_slice(&out.stdout).context("parsing forge-repomap edges output")?;
    Ok(group(raw, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_group_under_their_directory_as_a_module_and_edges_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(d.join("src/a.rs"), "one\ntwo\n").unwrap();
        std::fs::write(d.join("src/b.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.join("lib/c.rs"), "one\n").unwrap();

        let raw = RawGraph {
            nodes: vec![
                RawNode {
                    path: "src/a.rs".to_string(),
                    symbols: 2,
                },
                RawNode {
                    path: "src/b.rs".to_string(),
                    symbols: 1,
                },
                RawNode {
                    path: "lib/c.rs".to_string(),
                    symbols: 0,
                },
            ],
            edges: vec![
                RawEdge {
                    from: "src/a.rs".to_string(),
                    to: "src/b.rs".to_string(),
                },
                RawEdge {
                    from: "src/b.rs".to_string(),
                    to: "lib/c.rs".to_string(),
                },
            ],
        };

        let graph = group(raw, d);

        assert_eq!(graph.edges.len(), 2, "{graph:?}");
        assert_eq!(
            graph.edges[0],
            Edge {
                from: "src/a.rs".to_string(),
                to: "src/b.rs".to_string()
            }
        );
        assert_eq!(
            graph.edges[1],
            Edge {
                from: "src/b.rs".to_string(),
                to: "lib/c.rs".to_string()
            }
        );

        let by_path: BTreeMap<&str, &Node> =
            graph.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
        assert_eq!(graph.nodes.len(), 5, "{graph:?}");

        let a = by_path["src/a.rs"];
        assert_eq!((a.kind.as_str(), a.symbols, a.lines), ("file", 2, 2));
        let b = by_path["src/b.rs"];
        assert_eq!((b.kind.as_str(), b.symbols, b.lines), ("file", 1, 3));
        let c = by_path["lib/c.rs"];
        assert_eq!((c.kind.as_str(), c.symbols, c.lines), ("file", 0, 1));

        let src = by_path["src"];
        assert_eq!(
            (src.kind.as_str(), src.symbols, src.lines),
            ("module", 3, 5)
        );
        let lib = by_path["lib"];
        assert_eq!(
            (lib.kind.as_str(), lib.symbols, lib.lines),
            ("module", 0, 1)
        );
    }

    #[test]
    fn a_root_level_file_joins_no_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();
        let raw = RawGraph {
            nodes: vec![RawNode {
                path: "a.rs".to_string(),
                symbols: 1,
            }],
            edges: vec![],
        };
        let graph = group(raw, dir.path());
        assert_eq!(graph.nodes.len(), 1, "{graph:?}");
        assert_eq!(graph.nodes[0].kind, "file");
    }
}
