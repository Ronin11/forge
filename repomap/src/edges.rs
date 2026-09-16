//! Import edges: the structure layer of the code visualiser. Reuses the
//! same extractor table and content-addressed cache as symbols (see
//! `main.rs`), adding one more thing a blob's content determines: the raw
//! import specifiers it contains, cached per blob and resolved against the
//! current file set on every run (so a file moving elsewhere never serves a
//! stale target from an unrelated blob's cache entry).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::{BlobCache, Index, cache_path, index, tracked};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RawImport {
    /// `use crate::a::b;` -> ["a", "b"]
    Use(Vec<String>),
    /// `mod x;`
    Mod(String),
    /// A TypeScript/JavaScript relative specifier: `./a` or `../a/b`.
    Relative(String),
    /// `import a.b.c` -> ["a", "b", "c"]
    PyImport(Vec<String>),
    /// `from a.b import c` -> (["a", "b"], "c")
    PyFrom(Vec<String>, String),
    /// A Go import path string, as written.
    Go(String),
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Node {
    pub path: String,
    pub lang: String,
    pub symbols: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl BlobCache {
    fn edges_path(&self, blob: &str) -> std::path::PathBuf {
        self.path_for("edges", blob)
    }

    pub fn get_edges(&self, blob: &str) -> Option<Vec<RawImport>> {
        let text = std::fs::read_to_string(self.edges_path(blob)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put_edges(&self, blob: &str, raws: &[RawImport]) {
        let path = self.edges_path(blob);
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(".{blob}.edges.{}.tmp", std::process::id()));
        if let Ok(text) = serde_json::to_string(raws)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn lang_of(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" => "javascript",
        "py" => "python",
        "sh" | "bash" => "shell",
        "go" => "go",
        _ => "",
    }
}

type EdgeExtractor = fn(&str) -> Vec<RawImport>;
const EDGE_EXTRACTORS: &[(&[&str], EdgeExtractor)] = &[
    (&["rs"], extract_rust_imports),
    (&["ts", "tsx", "js", "jsx", "mjs"], extract_ts_imports),
    (&["py"], extract_py_imports),
    (&["go"], extract_go_imports),
];

fn raw_imports(path: &str, text: &str) -> Vec<RawImport> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    EDGE_EXTRACTORS
        .iter()
        .find(|(exts, _)| exts.contains(&ext))
        .map(|(_, f)| f(text))
        .unwrap_or_default()
}

/// A `use`/`mod` keyword, past any `pub`/`pub(crate)`/`pub(super)` qualifier.
fn strip_vis_kw<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    for prefix in ["pub(crate) ", "pub(super) ", "pub "] {
        if let Some(r) = line.strip_prefix(prefix)
            && let Some(r2) = r.strip_prefix(kw)
        {
            return Some(r2);
        }
    }
    line.strip_prefix(kw)
}

fn extract_rust_imports(text: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = strip_vis_kw(line, "use ") {
            let rest = rest.trim_end_matches(';').trim();
            if let Some(path) = rest.strip_prefix("crate::") {
                let path = path.split('{').next().unwrap_or(path);
                let path = path.trim_end_matches("::*").trim();
                let segs: Vec<String> = path
                    .split("::")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if !segs.is_empty() {
                    out.push(RawImport::Use(segs));
                }
            }
        } else if let Some(rest) = strip_vis_kw(line, "mod ") {
            // Only a declaration `mod x;`, never an inline module's body.
            if let Some(name) = rest.trim().strip_suffix(';') {
                let name = name.trim();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    out.push(RawImport::Mod(name.to_string()));
                }
            }
        }
    }
    out
}

/// `src/foo.rs`'s submodules live under `src/foo/`; `src/foo/mod.rs`
/// (and `lib.rs`/`main.rs`) name a directory that is already the module's
/// own, so their submodules are siblings instead.
fn rust_submodule_dir(path: &str) -> String {
    let p = Path::new(path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let parent = p
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    if matches!(stem, "mod" | "lib" | "main") {
        parent
    } else if parent.is_empty() {
        stem.to_string()
    } else {
        format!("{parent}/{stem}")
    }
}

fn resolve_rust(importer: &str, imp: &RawImport, nodes: &HashSet<String>) -> Option<String> {
    match imp {
        RawImport::Use(segs) => {
            let mut segs = segs.clone();
            loop {
                if segs.is_empty() {
                    return None;
                }
                let joined = segs.join("/");
                for cand in [format!("src/{joined}.rs"), format!("src/{joined}/mod.rs")] {
                    if nodes.contains(&cand) {
                        return Some(cand);
                    }
                }
                segs.pop();
            }
        }
        RawImport::Mod(name) => {
            let dir = rust_submodule_dir(importer);
            let candidates = if dir.is_empty() {
                [format!("{name}.rs"), format!("{name}/mod.rs")]
            } else {
                [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")]
            };
            candidates.into_iter().find(|c| nodes.contains(c))
        }
        _ => None,
    }
}

fn first_quoted(s: &str) -> Option<&str> {
    let s = s.trim_start();
    let (quote, rest) = if let Some(r) = s.strip_prefix('\'') {
        ('\'', r)
    } else if let Some(r) = s.strip_prefix('"') {
        ('"', r)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn specifiers_in_line(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for marker in ["from", "import", "require("] {
        if let Some(idx) = line.find(marker)
            && let Some(spec) = first_quoted(&line[idx + marker.len()..])
        {
            out.push(spec);
        }
    }
    out
}

fn extract_ts_imports(text: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        for spec in specifiers_in_line(line) {
            if spec.starts_with("./") || spec.starts_with("../") {
                out.push(RawImport::Relative(spec.to_string()));
            }
        }
    }
    out
}

fn normalize_path(p: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => {
                if let Some(s) = s.to_str() {
                    parts.push(s);
                }
            }
            _ => {}
        }
    }
    parts.join("/")
}

fn resolve_relative(importer: &str, spec: &str, nodes: &HashSet<String>) -> Option<String> {
    let base = Path::new(importer)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let joined = normalize_path(&base.join(spec));
    for ext in ["", ".ts", ".tsx", ".js", ".jsx", ".mjs"] {
        let cand = format!("{joined}{ext}");
        if nodes.contains(&cand) {
            return Some(cand);
        }
    }
    for idx in [
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "index.mjs",
    ] {
        let cand = if joined.is_empty() {
            idx.to_string()
        } else {
            format!("{joined}/{idx}")
        };
        if nodes.contains(&cand) {
            return Some(cand);
        }
    }
    None
}

fn extract_py_imports(text: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.split_whitespace().next().unwrap_or("");
                if module.is_empty() || module.starts_with('.') {
                    continue;
                }
                let segs: Vec<String> = module
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if !segs.is_empty() {
                    out.push(RawImport::PyImport(segs));
                }
            }
        } else if let Some(rest) = line.strip_prefix("from ")
            && let Some((module, names)) = rest.split_once(" import ")
        {
            let module = module.trim();
            if module.starts_with('.') {
                continue;
            }
            let segs: Vec<String> = module
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if segs.is_empty() {
                continue;
            }
            for name in names.split(',') {
                let name = name
                    .trim()
                    .trim_matches(|c| c == '(' || c == ')')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if !name.is_empty() && name != "*" {
                    out.push(RawImport::PyFrom(segs.clone(), name.to_string()));
                }
            }
        }
    }
    out
}

fn py_candidates(segs: &[String], nodes: &HashSet<String>) -> Option<String> {
    let joined = segs.join("/");
    [format!("{joined}.py"), format!("{joined}/__init__.py")]
        .into_iter()
        .find(|cand| nodes.contains(cand))
}

fn resolve_py(imp: &RawImport, nodes: &HashSet<String>) -> Option<String> {
    match imp {
        RawImport::PyImport(segs) => py_candidates(segs, nodes),
        RawImport::PyFrom(segs, name) => {
            let mut deep = segs.clone();
            deep.push(name.clone());
            py_candidates(&deep, nodes).or_else(|| py_candidates(segs, nodes))
        }
        _ => None,
    }
}

fn extract_go_imports(text: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("import ") {
            let rest = rest.trim();
            if rest.starts_with('(') {
                in_block = true;
                continue;
            }
            if let Some(spec) = quoted_import(rest) {
                out.push(RawImport::Go(spec));
            }
        } else if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            if let Some(spec) = quoted_import(line) {
                out.push(RawImport::Go(spec));
            }
        }
    }
    out
}

fn quoted_import(s: &str) -> Option<String> {
    let start = s.find('"')?;
    let rest = &s[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn go_module_prefix(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("go.mod")).ok()?;
    text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("module ")
            .map(|m| m.trim().to_string())
    })
}

fn resolve_go(spec: &str, module: Option<&str>, nodes: &HashSet<String>) -> Option<String> {
    let module = module?;
    let rel = if spec == module {
        String::new()
    } else {
        spec.strip_prefix(&format!("{module}/"))?.to_string()
    };
    let mut candidates: Vec<&String> = nodes
        .iter()
        .filter(|p| {
            let pp = Path::new(p.as_str());
            pp.extension().and_then(|e| e.to_str()) == Some("go")
                && !p.ends_with("_test.go")
                && pp
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .unwrap_or_default()
                    == rel
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next().cloned()
}

fn resolve(
    importer: &str,
    imp: &RawImport,
    nodes: &HashSet<String>,
    go_module: Option<&str>,
) -> Option<String> {
    match imp {
        RawImport::Use(_) | RawImport::Mod(_) => resolve_rust(importer, imp, nodes),
        RawImport::Relative(spec) => resolve_relative(importer, spec, nodes),
        RawImport::PyImport(_) | RawImport::PyFrom(_, _) => resolve_py(imp, nodes),
        RawImport::Go(spec) => resolve_go(spec, go_module, nodes),
    }
}

/// The graph: every source file the extractor table handles as a node,
/// and every import that resolves to another file in the tree as an edge.
/// Raw import specifiers are cached per blob hash exactly like symbols;
/// only their resolution against the current file set is redone each run,
/// so a file that moves never leaves a stale edge behind.
pub fn build(dir: &Path, shared: Option<&BlobCache>) -> Result<(Graph, usize)> {
    let (files, _) = index(dir, shared)?;
    let node_set: HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let mut nodes: Vec<Node> = files
        .iter()
        .map(|(p, syms)| {
            let ext = Path::new(p)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            Node {
                path: p.clone(),
                lang: lang_of(ext).to_string(),
                symbols: syms.len(),
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.path.cmp(&b.path));

    let go_module = go_module_prefix(dir);
    let cache_file = cache_path(dir);
    let mut cache: Index = if shared.is_some() {
        Index::default()
    } else {
        std::fs::read_to_string(&cache_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    };
    let mut dirty = false;
    let mut parsed = 0usize;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut edge_list: Vec<Edge> = Vec::new();
    for (path, blob) in tracked(dir)? {
        if !node_set.contains(&path) {
            continue;
        }
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !EDGE_EXTRACTORS.iter().any(|(exts, _)| exts.contains(&ext)) {
            continue;
        }
        let raws: Vec<RawImport> = if let Some(r) = shared.and_then(|c| c.get_edges(&blob)) {
            r
        } else if let Some(r) = cache.edges.get(&blob).filter(|_| shared.is_none()) {
            r.clone()
        } else {
            let text = std::fs::read_to_string(dir.join(&path)).unwrap_or_default();
            let r = raw_imports(&path, &text);
            parsed += 1;
            if let Some(c) = shared {
                c.put_edges(&blob, &r);
            } else {
                cache.edges.insert(blob.clone(), r.clone());
                dirty = true;
            }
            r
        };
        for raw in &raws {
            if let Some(target) = resolve(&path, raw, &node_set, go_module.as_deref())
                && seen.insert((path.clone(), target.clone()))
            {
                edge_list.push(Edge {
                    from: path.clone(),
                    to: target,
                    kind: "import".to_string(),
                });
            }
        }
    }
    if dirty && let Ok(text) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&cache_file, text);
    }
    Ok((
        Graph {
            nodes,
            edges: edge_list,
        },
        parsed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rust_use_and_mod_resolve_and_an_unresolvable_import_is_dropped() {
        let nodes = set(&["src/a/b.rs", "src/a/mod.rs", "src/a.rs", "src/lib.rs"]);
        let imp = extract_rust_imports("use crate::a::b;\nuse crate::nope::at::all;\n");
        assert_eq!(imp.len(), 2);
        assert_eq!(
            resolve_rust("src/lib.rs", &imp[0], &nodes),
            Some("src/a/b.rs".to_string())
        );
        assert_eq!(resolve_rust("src/lib.rs", &imp[1], &nodes), None);

        // `use crate::a::Thing;` falls back from the item to its module file.
        let item_use = extract_rust_imports("use crate::a::Thing;\n");
        assert_eq!(
            resolve_rust("src/lib.rs", &item_use[0], &nodes),
            Some("src/a.rs".to_string())
        );

        // `mod x;` in a plain file resolves relative to a same-named directory.
        let modimp =
            extract_rust_imports("mod b;\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n");
        assert_eq!(modimp, vec![RawImport::Mod("b".to_string())]);
        assert_eq!(
            resolve_rust("src/a.rs", &modimp[0], &nodes),
            Some("src/a/b.rs".to_string())
        );
    }

    #[test]
    fn typescript_relative_imports_resolve_with_extension_and_index_and_drop_the_rest() {
        let nodes = set(&["src/a.ts", "src/util/index.ts", "src/main.ts"]);
        let text = "import { a } from './a';\nimport { u } from './util';\nimport { x } from './missing';\nconst y = require('../a');\n";
        let raws = extract_ts_imports(text);
        assert_eq!(raws.len(), 4);
        assert_eq!(
            resolve_relative("src/main.ts", "./a", &nodes),
            Some("src/a.ts".to_string())
        );
        assert_eq!(
            resolve_relative("src/main.ts", "./util", &nodes),
            Some("src/util/index.ts".to_string())
        );
        assert_eq!(resolve_relative("src/main.ts", "./missing", &nodes), None);
        assert_eq!(
            resolve_relative("src/sub/main.ts", "../a", &nodes),
            Some("src/a.ts".to_string())
        );
    }

    #[test]
    fn python_import_and_from_resolve_with_fallback_and_drop_the_rest() {
        let nodes = set(&["a/b.py", "a/__init__.py"]);
        let raws =
            extract_py_imports("import a.b\nfrom a import c\nfrom a.b import nope\nimport z.q\n");
        assert_eq!(raws.len(), 4);
        assert_eq!(
            resolve_py(&raws[0], &nodes),
            Some("a/b.py".to_string()),
            "import a.b"
        );
        assert_eq!(
            resolve_py(&raws[1], &nodes),
            Some("a/__init__.py".to_string()),
            "from a import c falls back to the package file"
        );
        assert_eq!(
            resolve_py(&raws[2], &nodes),
            Some("a/b.py".to_string()),
            "from a.b import nope: a/b/nope.py doesn't exist, so it falls back to a/b.py"
        );
        assert_eq!(
            resolve_py(&raws[3], &nodes),
            None,
            "import z.q: nothing under z exists"
        );
    }

    #[test]
    fn go_imports_resolve_within_the_module_path_and_drop_external_packages() {
        let nodes = set(&["main.go", "pkg/x/x.go", "pkg/x/y.go"]);
        let raws = extract_go_imports(
            "import (\n\t\"example.com/mod/pkg/x\"\n\t\"fmt\"\n)\nimport \"example.com/mod\"\n",
        );
        assert_eq!(raws.len(), 3);
        let module = Some("example.com/mod");
        assert_eq!(
            resolve(
                "main.go",
                &RawImport::Go("example.com/mod/pkg/x".to_string()),
                &nodes,
                module
            ),
            Some("pkg/x/x.go".to_string()),
            "picks the first file alphabetically in the package directory"
        );
        assert_eq!(
            resolve("main.go", &RawImport::Go("fmt".to_string()), &nodes, module),
            None,
            "stdlib packages aren't in the module"
        );
        assert_eq!(
            resolve(
                "main.go",
                &RawImport::Go("example.com/mod".to_string()),
                &nodes,
                module
            ),
            Some("main.go".to_string()),
            "the module root package resolves to a root-level file"
        );
    }

    #[test]
    fn the_output_is_stable_across_two_runs() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(d)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@e"]);
        git(&["config", "user.name", "t"]);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/lib.rs"), "mod a;\n").unwrap();
        std::fs::write(d.join("src/a.rs"), "pub fn f() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "x"]);

        let (first, _) = build(d, None).unwrap();
        let (second, _) = build(d, None).unwrap();
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        assert!(
            first
                .edges
                .iter()
                .any(|e| e.from == "src/lib.rs" && e.to == "src/a.rs"),
            "{first:?}"
        );

        // A shared cache also reuses raw imports across runs without
        // reparsing the unchanged blob.
        let cache_dir = tempfile::tempdir().unwrap();
        let shared = BlobCache::new(cache_dir.path());
        let (_, parsed_first) = build(d, Some(&shared)).unwrap();
        let (_, parsed_second) = build(d, Some(&shared)).unwrap();
        assert!(parsed_first > 0);
        assert_eq!(parsed_second, 0, "the second run reused the cached edges");
    }
}
