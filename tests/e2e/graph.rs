//! `forge graph`: the module graph as data (docs/LATER.md, "The code
//! visualiser"), read straight off a repository's working tree with no
//! store, plus the built-in `repo-graph` operation that keeps a fresh
//! copy in the cache directory.

use crate::support::*;

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}

/// A fixture repository with three files and two imports: `src/lib.rs`
/// both `mod`s and `use`s `src/a.rs` (deduped to one edge), and
/// `src/a.rs` `mod`s its own submodule `src/a/b.rs`.
fn three_file_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@e"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::create_dir_all(repo.join("src/a")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "mod a;\nuse crate::a::helper;\n").unwrap();
    std::fs::write(repo.join("src/a.rs"), "pub fn helper() {}\nmod b;\n").unwrap();
    std::fs::write(repo.join("src/a/b.rs"), "pub fn deep() {}\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "x"]);
    dir
}

#[test]
fn forge_graph_groups_files_under_module_nodes_and_prints_the_two_import_edges() {
    let dir = three_file_repo();
    let repo = dir.path();

    let o = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .env_remove("FORGE2_HOME")
        .args(["graph", repo.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();

    let nodes = doc["nodes"].as_array().unwrap();
    let files: Vec<&str> = nodes
        .iter()
        .filter(|n| n["kind"] == "file")
        .map(|n| n["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec!["src/a.rs", "src/a/b.rs", "src/lib.rs"], "{doc}");

    let modules: Vec<&str> = nodes
        .iter()
        .filter(|n| n["kind"] == "module")
        .map(|n| n["path"].as_str().unwrap())
        .collect();
    assert_eq!(modules, vec!["src", "src/a"], "{doc}");

    let a_module = nodes
        .iter()
        .find(|n| n["path"] == "src/a" && n["kind"] == "module")
        .unwrap();
    assert_eq!(a_module["symbols"], 1, "{doc}");
    assert_eq!(a_module["lines"], 1, "{doc}");

    let edges: Vec<(&str, &str)> = doc["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["from"].as_str().unwrap(), e["to"].as_str().unwrap()))
        .collect();
    assert_eq!(
        edges,
        vec![("src/a.rs", "src/a/b.rs"), ("src/lib.rs", "src/a.rs")],
        "{doc}"
    );
}

#[test]
fn without_json_forge_graph_prints_a_one_line_count() {
    let dir = three_file_repo();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .env_remove("FORGE2_HOME")
        .args(["graph", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "3 file(s), 2 module(s), 2 edge(s)"
    );
}

/// The built-in `repo-graph` operation, in a workflow, writes
/// `forge graph`'s own document to `$FORGE_CACHE_DIR/graph.json` — the
/// same file every attempt overwrites, so a landing always leaves a
/// fresh graph behind (docs/ACTIONS.md, `repo-graph`).
#[test]
fn the_repo_graph_operation_writes_a_fresh_graph_to_the_cache_directory() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/graphed.toml"),
        "name = \"graphed\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"repo-graph\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "graphed",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let cache = e.home.join("cache/graph.json");
    let text = std::fs::read_to_string(&cache)
        .unwrap_or_else(|e| panic!("reading {}: {e}", cache.display()));
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        !doc["nodes"].as_array().unwrap().is_empty(),
        "{doc} ({})",
        cache.display()
    );
    assert!(doc["edges"].is_array(), "{doc}");
}
