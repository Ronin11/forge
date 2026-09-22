//! Black-box, per-language fixtures for `forge-repomap edges`: a small git
//! repo per test, the compiled binary run against it exactly as the web
//! client and `forge doctor` invoke it, the printed graph parsed back and
//! checked for the edges a reader would expect (and that an import of
//! something outside the repo never becomes one).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A fresh git repo with `files` committed, and the `edges` subcommand's
/// parsed graph for it.
fn graph_for(files: &[(&str, &str)]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    git(d, &["init", "-q"]);
    git(d, &["config", "user.email", "t@e"]);
    git(d, &["config", "user.name", "t"]);
    for (rel, contents) in files {
        write(d, rel, contents);
    }
    git(d, &["add", "."]);
    git(d, &["commit", "-qm", "x"]);

    let out = Command::new(env!("CARGO_BIN_EXE_forge-repomap"))
        .args(["edges"])
        .arg(d)
        .output()
        .expect("forge-repomap edges");
    assert!(
        out.status.success(),
        "forge-repomap edges failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("valid json")
}

fn edge<'a>(graph: &'a Value, from: &str, to: &str) -> Option<&'a Value> {
    graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["from"] == from && e["to"] == to)
}

#[test]
fn rust_use_and_mod_edges_and_an_external_crate_is_dropped() {
    let graph = graph_for(&[
        (
            "src/lib.rs",
            "mod a;\nuse crate::a::helper;\nuse std::fmt;\n",
        ),
        ("src/a.rs", "pub fn helper() {}\n"),
    ]);
    assert!(edge(&graph, "src/lib.rs", "src/a.rs").is_some(), "{graph}");
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1, "{graph}");
}

#[test]
fn typescript_relative_imports_resolve_and_a_bare_package_is_dropped() {
    let graph = graph_for(&[
        (
            "src/main.ts",
            "import { helper } from './util';\nimport React from 'react';\n",
        ),
        ("src/util/index.ts", "export const helper = 1;\n"),
    ]);
    assert!(
        edge(&graph, "src/main.ts", "src/util/index.ts").is_some(),
        "{graph}"
    );
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1, "{graph}");
}

#[test]
fn javascript_require_resolves_within_the_repo() {
    let graph = graph_for(&[
        (
            "src/main.js",
            "const util = require('./util.js');\nconst fs = require('fs');\n",
        ),
        ("src/util.js", "module.exports = {};\n"),
    ]);
    assert!(
        edge(&graph, "src/main.js", "src/util.js").is_some(),
        "{graph}"
    );
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1, "{graph}");
}

#[test]
fn python_import_and_from_resolve_and_a_stdlib_module_is_dropped() {
    let graph = graph_for(&[
        (
            "pkg/main.py",
            "import pkg.util\nfrom pkg import util\nimport os\n",
        ),
        ("pkg/util.py", "def helper():\n    pass\n"),
        ("pkg/__init__.py", ""),
    ]);
    assert!(
        edge(&graph, "pkg/main.py", "pkg/util.py").is_some(),
        "{graph}"
    );
    assert!(
        edge(&graph, "pkg/main.py", "pkg/__init__.py").is_none(),
        "{graph}"
    );
}

#[test]
fn go_imports_resolve_within_the_module_and_the_standard_library_is_dropped() {
    let graph = graph_for(&[
        ("go.mod", "module example.com/mod\n"),
        (
            "main.go",
            "package main\n\nimport (\n\t\"example.com/mod/pkg\"\n\t\"fmt\"\n)\n",
        ),
        ("pkg/pkg.go", "package pkg\n"),
    ]);
    assert!(edge(&graph, "main.go", "pkg/pkg.go").is_some(), "{graph}");
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1, "{graph}");
}
