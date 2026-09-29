//! Keep the kernel's modules layered; pre-existing violations have a shrinking exception list.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

// Bottom up: a module may import only modules in its own layer or below, and
// never a module that imports it back (directly or through a chain).
const LAYERS: &[(&str, &[&str])] = &[
    (
        "foundation",
        &[
            "git", "egress", "sandbox", "executor", "pricing", "envelope", "checks", "config",
        ],
    ),
    ("store", &["store"]),
    ("agent", &["agent"]),
    (
        "kernel",
        &[
            "attempt",
            "verify",
            "operation",
            "landing",
            "engine",
            "queue",
            "worker",
            "job",
            "supervisor",
            "environment",
            "handoff",
            "journal",
            "mechanic",
        ],
    ),
    (
        "analysis",
        &["audit", "experiment", "profile", "render", "stats"],
    ),
    ("view", &["view"]),
    ("cli", &["cli", "init"]),
];

// (from, to, reason). Remove entries as edges are fixed; never add new ones.
// A stale entry, one that is no longer a violation, fails the test.
const ALLOWLIST: &[(&str, &str, &str)] = &[
    (
        "agent",
        "supervisor",
        "existing edge awaiting a layering fix",
    ),
    ("attempt", "audit", "existing edge awaiting a layering fix"),
    ("attempt", "engine", "existing edge awaiting a layering fix"),
    (
        "attempt",
        "handoff",
        "existing edge awaiting a layering fix",
    ),
    (
        "attempt",
        "journal",
        "existing edge awaiting a layering fix",
    ),
    (
        "attempt",
        "landing",
        "existing edge awaiting a layering fix",
    ),
    ("attempt", "verify", "existing edge awaiting a layering fix"),
    ("checks", "agent", "existing edge awaiting a layering fix"),
    ("cli", "init", "existing edge awaiting a layering fix"),
    ("config", "agent", "existing edge awaiting a layering fix"),
    (
        "config",
        "environment",
        "existing edge awaiting a layering fix",
    ),
    (
        "config",
        "executor",
        "existing edge awaiting a layering fix",
    ),
    ("egress", "agent", "existing edge awaiting a layering fix"),
    ("engine", "attempt", "existing edge awaiting a layering fix"),
    ("engine", "audit", "existing edge awaiting a layering fix"),
    ("engine", "handoff", "existing edge awaiting a layering fix"),
    ("engine", "landing", "existing edge awaiting a layering fix"),
    (
        "engine",
        "operation",
        "existing edge awaiting a layering fix",
    ),
    ("engine", "queue", "existing edge awaiting a layering fix"),
    ("engine", "verify", "existing edge awaiting a layering fix"),
    ("engine", "view", "existing edge awaiting a layering fix"),
    ("engine", "worker", "existing edge awaiting a layering fix"),
    ("executor", "agent", "existing edge awaiting a layering fix"),
    (
        "executor",
        "config",
        "existing edge awaiting a layering fix",
    ),
    (
        "executor",
        "sandbox",
        "existing edge awaiting a layering fix",
    ),
    ("git", "job", "existing edge awaiting a layering fix"),
    (
        "handoff",
        "journal",
        "existing edge awaiting a layering fix",
    ),
    ("init", "cli", "existing edge awaiting a layering fix"),
    ("job", "operation", "existing edge awaiting a layering fix"),
    ("journal", "engine", "existing edge awaiting a layering fix"),
    ("landing", "audit", "existing edge awaiting a layering fix"),
    ("landing", "engine", "existing edge awaiting a layering fix"),
    ("landing", "queue", "existing edge awaiting a layering fix"),
    ("landing", "verify", "existing edge awaiting a layering fix"),
    ("landing", "view", "existing edge awaiting a layering fix"),
    (
        "operation",
        "engine",
        "existing edge awaiting a layering fix",
    ),
    (
        "operation",
        "landing",
        "existing edge awaiting a layering fix",
    ),
    (
        "operation",
        "verify",
        "existing edge awaiting a layering fix",
    ),
    ("pricing", "agent", "existing edge awaiting a layering fix"),
    (
        "queue",
        "experiment",
        "existing edge awaiting a layering fix",
    ),
    ("queue", "job", "existing edge awaiting a layering fix"),
    ("queue", "render", "existing edge awaiting a layering fix"),
    ("queue", "view", "existing edge awaiting a layering fix"),
    ("sandbox", "agent", "existing edge awaiting a layering fix"),
    ("sandbox", "config", "existing edge awaiting a layering fix"),
    ("store", "audit", "existing edge awaiting a layering fix"),
    ("store", "profile", "existing edge awaiting a layering fix"),
    ("store", "render", "existing edge awaiting a layering fix"),
    ("store", "view", "existing edge awaiting a layering fix"),
    (
        "supervisor",
        "attempt",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "audit",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "engine",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "journal",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "landing",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "queue",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "verify",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "view",
        "existing edge awaiting a layering fix",
    ),
    (
        "supervisor",
        "worker",
        "existing edge awaiting a layering fix",
    ),
    (
        "verify",
        "operation",
        "existing edge awaiting a layering fix",
    ),
    ("worker", "engine", "existing edge awaiting a layering fix"),
    ("worker", "job", "existing edge awaiting a layering fix"),
    (
        "worker",
        "supervisor",
        "existing edge awaiting a layering fix",
    ),
    ("worker", "view", "existing edge awaiting a layering fix"),
];

fn layer_of(module: &str) -> Option<usize> {
    LAYERS.iter().position(|(_, mods)| mods.contains(&module))
}

fn tracked_sources(root: &Path) -> Vec<PathBuf> {
    let out = Command::new("git")
        .args(["ls-files", "src"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|p| p.ends_with(".rs"))
        .map(PathBuf::from)
        .collect()
}

/// Top-level module of a `src/` path, or None for main.rs.
fn module_of(path: &Path) -> Option<String> {
    let first = path.strip_prefix("src").ok()?.components().next()?;
    let name = first.as_os_str().to_str()?;
    let name = name.strip_suffix(".rs").unwrap_or(name);
    (name != "main").then(|| name.to_string())
}

fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "tests.rs" || name.ends_with("_tests.rs")
}

fn strip_comments(src: &str) -> String {
    let mut out = String::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            for d in chars.by_ref() {
                if d == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn ident_at(s: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
        i += 1;
    }
    (s[start..i].iter().collect(), i)
}

/// Top-level modules named by `crate::x` and by `crate::{x, y::z, ...}` groups,
/// which may span lines and nest.
fn crate_refs(src: &str) -> BTreeSet<String> {
    let text: Vec<char> = strip_comments(src).chars().collect();
    let pat: Vec<char> = "crate::".chars().collect();
    let mut refs = BTreeSet::new();
    let mut i = 0;
    while i + pat.len() <= text.len() {
        let boundary =
            i == 0 || !(text[i - 1].is_alphanumeric() || text[i - 1] == '_' || text[i - 1] == ':');
        if boundary && text[i..i + pat.len()] == pat[..] {
            i += pat.len();
            while i < text.len() && text[i].is_whitespace() {
                i += 1;
            }
            if i < text.len() && text[i] == '{' {
                i = group_refs(&text, i, &mut refs);
            } else {
                let (name, next) = ident_at(&text, i);
                if !name.is_empty() {
                    refs.insert(name);
                }
                i = next;
            }
        } else {
            i += 1;
        }
    }
    refs
}

/// Reads the group opening at `open`; records the first path segment of each
/// depth-1 item and returns the index after the closing brace.
fn group_refs(text: &[char], open: usize, refs: &mut BTreeSet<String>) -> usize {
    let mut depth = 0;
    let mut item_start = true;
    let mut i = open;
    while i < text.len() {
        let c = text[i];
        match c {
            '{' => {
                depth += 1;
                if depth == 1 {
                    item_start = true;
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            ',' if depth == 1 => item_start = true,
            c if c.is_whitespace() => {}
            c if depth == 1 && item_start && (c.is_alphabetic() || c == '_') => {
                let (name, next) = ident_at(text, i);
                if name != "self" {
                    refs.insert(name);
                }
                item_start = false;
                i = next;
                continue;
            }
            _ => item_start = false,
        }
        i += 1;
    }
    i
}

/// (from, to) -> the first file the edge was found in.
fn edges(root: &Path) -> BTreeMap<(String, String), String> {
    let mut found = BTreeMap::new();
    for path in tracked_sources(root) {
        let Some(from) = module_of(&path) else {
            continue;
        };
        if is_test_file(&path) {
            continue;
        }
        let src = std::fs::read_to_string(root.join(&path)).unwrap();
        for to in crate_refs(&src) {
            if to != from && layer_of(&to).is_some() {
                found
                    .entry((from.clone(), to))
                    .or_insert_with(|| path.display().to_string());
            }
        }
    }
    found
}

fn reaches(graph: &BTreeMap<&str, Vec<&str>>, from: &str, target: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(n) = stack.pop() {
        if n == target {
            return true;
        }
        if seen.insert(n) {
            stack.extend(graph.get(n).into_iter().flatten().copied());
        }
    }
    false
}

/// Violating edges with their description.
fn violations(found: &BTreeMap<(String, String), String>) -> BTreeMap<(String, String), String> {
    let mut same: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (from, to) in found.keys() {
        if layer_of(from) == layer_of(to) {
            same.entry(from).or_default().push(to);
        }
    }
    let mut bad = BTreeMap::new();
    for ((from, to), file) in found {
        let (Some(lf), Some(lt)) = (layer_of(from), layer_of(to)) else {
            continue;
        };
        let why = if lt > lf {
            "points up a layer"
        } else if lt == lf && reaches(&same, to, from) {
            "closes a cycle within a layer"
        } else {
            continue;
        };
        bad.insert(
            (from.clone(), to.clone()),
            format!(
                "{from} ({}) -> {to} ({}): {why}, found in {file}",
                LAYERS[lf].0, LAYERS[lt].0
            ),
        );
    }
    bad
}

#[test]
fn layers_only_import_downward() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bad = violations(&edges(root));
    let allowed: BTreeSet<(&str, &str)> = ALLOWLIST.iter().map(|(f, t, _)| (*f, *t)).collect();
    let new: Vec<String> = bad
        .iter()
        .filter(|((f, t), _)| !allowed.contains(&(f.as_str(), t.as_str())))
        .map(|(k, msg)| format!("{msg}\n    (\"{}\", \"{}\", \"reason\"),", k.0, k.1))
        .collect();
    assert!(new.is_empty(), "layering violations:\n{}", new.join("\n"));
    let stale: Vec<String> = allowed
        .iter()
        .filter(|(f, t)| !bad.contains_key(&(f.to_string(), t.to_string())))
        .map(|(f, t)| format!("{f} -> {t}"))
        .collect();
    assert!(
        stale.is_empty(),
        "stale ALLOWLIST entries, delete them: {stale:?}"
    );
}

#[test]
fn allowlist_entries_are_unique_and_explained() {
    let mut seen = BTreeSet::new();
    for (from, to, reason) in ALLOWLIST {
        assert!(seen.insert((from, to)), "duplicate entry {from} -> {to}");
        assert!(!reason.is_empty(), "{from} -> {to} needs a reason");
        assert!(layer_of(from).is_some() && layer_of(to).is_some());
    }
}

#[test]
fn multi_line_and_nested_groups_are_read() {
    let src = "use crate::{\n    config,\n    egress::{Policy, Rule},\n    sandbox::Sandbox,\n};\nuse crate::git::Git; // crate::view\n";
    let refs: Vec<String> = crate_refs(src).into_iter().collect();
    assert_eq!(refs, ["config", "egress", "git", "sandbox"]);
}
