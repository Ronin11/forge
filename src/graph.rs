//! The module graph as data (docs/LATER.md, "The code visualiser"): files
//! and symbols as nodes, import edges between them, files grouped under
//! their directories as module nodes. Built from `forge-repomap edges`
//! (repomap/src/edges.rs), which already extracts every file's symbol
//! count and the imports that resolve inside the tree; this only adds
//! line counts (read straight off disk, since the argument is a working
//! tree, not a bare tree) and the directory grouping. `build` is
//! deterministic end to end: no model, no store. `overlay` is the one
//! part that does read the store, laying what the record knows for each
//! file node on top (docs/LATER.md, "The overlay, from the record").

use crate::store::Store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One task's touch on a file node, from `Store::file_changes`.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct TaskTouch {
    pub id: i64,
    pub at: i64,
    pub cost_usd: f64,
}

/// One review demotion whose evidence names a file node, from
/// `Store::task_demotions`.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Demotion {
    pub id: i64,
    pub at: i64,
    pub reason: String,
}

/// What the record knows about one file node (docs/LATER.md, "The
/// overlay, from the record"): empty for a file no task ever touched.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Overlay {
    pub tasks: Vec<TaskTouch>,
    pub demotions: Vec<Demotion>,
    pub repair_cost_usd: f64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Node {
    pub path: String,
    pub kind: String,
    pub symbols: usize,
    pub lines: usize,
    #[serde(default)]
    pub overlay: Overlay,
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
            overlay: Overlay::default(),
        });
    }
    for (path, (symbols, lines)) in modules {
        nodes.push(Node {
            path,
            kind: "module".to_string(),
            symbols,
            lines,
            overlay: Overlay::default(),
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

/// Fills in every file node's overlay from `store` (docs/LATER.md, "The
/// overlay, from the record"): the tasks that changed it, review
/// demotions whose evidence names it, and its share of the quality
/// statistics' repair cost. `repo` must be the same string a queued
/// task's own `repo` field carries (a canonicalized path), since that is
/// the only thing tying an attempt back to this repository. Module
/// nodes are left untouched; a file node no task ever touched keeps its
/// default, empty overlay.
pub fn overlay(store: &Store, repo: &str, graph: &mut Graph) -> Result<()> {
    let changes = store.file_changes(repo)?;
    let demotions = store.task_demotions(repo)?;
    let repair_costs = store.task_repair_costs(repo)?;

    // Which files each task touched, from the same `changes` rows: the
    // split `repair_cost_usd` divides a task's cached repair cost by,
    // since the quality statistics attribute it per task, not per path.
    let mut files_by_task: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for c in &changes {
        files_by_task
            .entry(c.task_id)
            .or_default()
            .push(c.path.clone());
    }
    let mut repair_share: BTreeMap<String, f64> = BTreeMap::new();
    for (task_id, cost) in &repair_costs {
        if let Some(paths) = files_by_task.get(task_id) {
            let share = cost / paths.len() as f64;
            for p in paths {
                *repair_share.entry(p.clone()).or_default() += share;
            }
        }
    }

    for node in &mut graph.nodes {
        if node.kind != "file" {
            continue;
        }
        node.overlay.tasks = changes
            .iter()
            .filter(|c| c.path == node.path)
            .map(|c| TaskTouch {
                id: c.task_id,
                at: c.at,
                cost_usd: c.cost_usd,
            })
            .collect();
        node.overlay.demotions = demotions
            .iter()
            .filter(|d| d.evidence.iter().any(|e| e.contains(&node.path)))
            .map(|d| Demotion {
                id: d.task_id,
                at: d.at,
                reason: d.reason.clone(),
            })
            .collect();
        node.overlay.repair_cost_usd = repair_share.get(&node.path).copied().unwrap_or(0.0);
    }
    Ok(())
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

    /// `overlay` end to end: a task that touched two files splits its
    /// cached repair cost evenly between them, a review demotion whose
    /// evidence names one of those files attaches to that file only, and
    /// a file no task ever touched keeps `Overlay::default()`.
    #[test]
    fn overlay_splits_repair_cost_and_matches_demotions_by_evidence() {
        use crate::store::{Attempt, AttemptState, FinishAttempt, Store, Task, TaskState};

        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let repo = "/repo";

        let finish = |id: i64, reason: &str, envelope_json: &str| {
            s.finish_attempt(&FinishAttempt {
                id,
                state: if reason.starts_with("review demoted") {
                    AttemptState::NeedsInput
                } else {
                    AttemptState::Succeeded
                },
                reason: reason.into(),
                finished_at: Some(10),
                agent_exit: Some(0),
                timed_out: false,
                num_turns: 1,
                tool_calls: 1,
                cost_usd: Some(0.0),
                agent_ms: 0,
                commits: 0,
                files_changed: 0,
                dirty: false,
                verdict_json: "[]".into(),
                result_text: String::new(),
                envelope_json: envelope_json.into(),
                rl_five_hour: None,
                rl_seven_day: None,
                rl_five_hour_resets: None,
                rl_seven_day_resets: None,
                end_sha: String::new(),
                outputs_json: String::new(),
                session_id: String::new(),
                first_edit: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
                cli_cost_usd: None,
            })
            .unwrap();
        };

        // Task X lands, touching both a.rs and b.rs; its repair cost
        // (from the quality statistics) splits evenly between them.
        let mut x = Task {
            repo: repo.into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: 1,
            started_at: Some(1),
            finished_at: Some(1),
            workflow: "direct".into(),
            landed_sha: "xsha".into(),
            ..Default::default()
        };
        x.id = s.insert_task(&x).unwrap();
        s.update_task(&x).unwrap();
        let x1 = s
            .insert_attempt(&Attempt {
                task_id: x.id,
                attempt_no: 1,
                step: "code".into(),
                started_at: 1,
                ..Default::default()
            })
            .unwrap();
        finish(
            x1,
            "",
            r#"{"schema_version":1,"summary":"s","needs_input":null,"changes":[{"path":"src/a.rs","kind":"modified","summary":""},{"path":"src/b.rs","kind":"modified","summary":""}],"checks_run":[],"claims":[]}"#,
        );
        s.set_repair_cost_cache(x.id, 4.0, 9999).unwrap();

        // Task Y is demoted by a review whose evidence names a.rs.
        let mut y = Task {
            repo: repo.into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Blocked,
            created_at: 1,
            started_at: Some(1),
            finished_at: Some(1),
            workflow: "direct".into(),
            ..Default::default()
        };
        y.id = s.insert_task(&y).unwrap();
        let y1 = s
            .insert_attempt(&Attempt {
                task_id: y.id,
                attempt_no: 1,
                step: "review".into(),
                started_at: 1,
                ..Default::default()
            })
            .unwrap();
        finish(
            y1,
            "review demoted: off by one",
            r#"{"schema_version":1,"summary":"s","needs_input":{"question":"off by one","tried":"","kind":"review"},"changes":[],"checks_run":[],"claims":[{"claim":"c","evidence":"cat -n src/a.rs shows the bug"}]}"#,
        );

        let mut graph = Graph {
            nodes: vec![
                Node {
                    path: "src/a.rs".into(),
                    kind: "file".into(),
                    symbols: 1,
                    lines: 1,
                    overlay: Overlay::default(),
                },
                Node {
                    path: "src/b.rs".into(),
                    kind: "file".into(),
                    symbols: 1,
                    lines: 1,
                    overlay: Overlay::default(),
                },
                Node {
                    path: "src/c.rs".into(),
                    kind: "file".into(),
                    symbols: 1,
                    lines: 1,
                    overlay: Overlay::default(),
                },
            ],
            edges: vec![],
        };

        overlay(&s, repo, &mut graph).unwrap();

        let a = graph.nodes.iter().find(|n| n.path == "src/a.rs").unwrap();
        assert_eq!(a.overlay.tasks.len(), 1, "{a:?}");
        assert_eq!(a.overlay.tasks[0].id, x.id);
        assert_eq!(a.overlay.demotions.len(), 1, "{a:?}");
        assert_eq!(a.overlay.demotions[0].id, y.id);
        assert_eq!(a.overlay.demotions[0].reason, "review demoted: off by one");
        assert_eq!(
            a.overlay.repair_cost_usd, 2.0,
            "half of X's 4.0, split with b.rs"
        );

        let b = graph.nodes.iter().find(|n| n.path == "src/b.rs").unwrap();
        assert_eq!(b.overlay.tasks.len(), 1, "{b:?}");
        assert!(b.overlay.demotions.is_empty(), "{b:?}");
        assert_eq!(b.overlay.repair_cost_usd, 2.0);

        let c = graph.nodes.iter().find(|n| n.path == "src/c.rs").unwrap();
        assert_eq!(c.overlay, Overlay::default(), "no task ever touched c.rs");
    }
}
