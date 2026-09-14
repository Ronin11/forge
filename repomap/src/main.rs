//! forge-repomap: where things are, for an agent about to start looking.
//!
//! `index` parses every tracked file into its symbols (functions, types,
//! exports), cached in the clone's .git by blob hash so a rebuild only
//! re-parses what changed. `rank` scores files against a task's words,
//! plus a prior of files earlier work read most, and prints the best under
//! a budget as `path: symbol, symbol, ...`. Deterministic end to end.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Index {
    /// blob sha -> symbols, the cache; paths map onto blobs per tree.
    pub blobs: HashMap<String, Vec<Symbol>>,
}

/// Symbols in one file, by its extension. Regex-free, line-oriented: the
/// declarations a reader would scan for, never bodies.
pub fn extract(path: &str, text: &str) -> Vec<Symbol> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut out = Vec::new();
    let push = |out: &mut Vec<Symbol>, name: &str, kind: &str| {
        let name = name.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if !name.is_empty()
            && !out
                .iter()
                .any(|s: &Symbol| s.name == name && s.kind == kind)
        {
            out.push(Symbol {
                name: name.to_string(),
                kind: kind.to_string(),
            });
        }
    };
    for raw in text.lines() {
        let line = raw.trim_start();
        let words: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || matches!(c, '(' | '<' | '{' | ':' | '=' | ';'))
            .filter(|w| !w.is_empty())
            .collect();
        match ext {
            "rs" => {
                let mut i = 0;
                // Visibility and qualifiers: `pub(crate)` splits on '(' into "pub", "crate)".
                while i < words.len()
                    && (words[i].starts_with("pub")
                        || words[i].ends_with(')')
                        || matches!(words[i], "async" | "unsafe" | "const" | "extern" | "\"C\""))
                {
                    i += 1;
                }
                if i + 1 < words.len() {
                    let kind = match words[i] {
                        "fn" => "fn",
                        "struct" => "struct",
                        "enum" => "enum",
                        "trait" => "trait",
                        "type" => "type",
                        "mod" => "mod",
                        "static" => "static",
                        _ => "",
                    };
                    if !kind.is_empty() && !line.starts_with("//") {
                        push(&mut out, words[i + 1], kind);
                    }
                    if words[i] == "impl" {
                        let target = words[i + 1..]
                            .iter()
                            .find(|w| !w.starts_with('\'') && **w != "for")
                            .copied()
                            .unwrap_or("");
                        if let Some(t) = words[i + 1..].iter().position(|w| *w == "for") {
                            push(&mut out, words[i + 1 + t + 1], "impl");
                        } else {
                            push(&mut out, target, "impl");
                        }
                    }
                }
                if line.starts_with("const ") && words.len() > 1 {
                    push(&mut out, words[1], "const");
                }
            }
            "ts" | "tsx" | "js" | "jsx" | "mjs" => {
                let mut i = 0;
                while i < words.len()
                    && matches!(
                        words[i],
                        "export" | "default" | "async" | "declare" | "abstract"
                    )
                {
                    i += 1;
                }
                if i + 1 < words.len() {
                    let kind = match words[i] {
                        "function" => "function",
                        "class" => "class",
                        "interface" => "interface",
                        "type" => "type",
                        "enum" => "enum",
                        "const" | "let" | "var" if i > 0 && words[0] == "export" => "const",
                        _ => "",
                    };
                    if !kind.is_empty() && !line.starts_with("//") {
                        push(&mut out, words[i + 1], kind);
                    }
                }
            }
            "py" => {
                if (line.starts_with("def ") || line.starts_with("async def ")) && words.len() > 1 {
                    push(
                        &mut out,
                        words[if words[0] == "async" { 2 } else { 1 }],
                        "def",
                    );
                } else if line.starts_with("class ") && words.len() > 1 {
                    push(&mut out, words[1], "class");
                }
            }
            "sh" | "bash" => {
                if let Some(rest) = line.strip_prefix("function ") {
                    push(
                        &mut out,
                        rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .next()
                            .unwrap_or(""),
                        "function",
                    );
                } else if let Some((name, rest)) = line.split_once("()")
                    && !name.contains(char::is_whitespace)
                    && rest.trim_start().starts_with('{')
                {
                    push(&mut out, name, "function");
                }
            }
            "go" => {
                if line.starts_with("func ") && words.len() > 1 {
                    let name = if words[1].starts_with('(') || words[1].is_empty() {
                        words.get(3).copied().unwrap_or("")
                    } else {
                        words[1]
                    };
                    push(&mut out, name, "func");
                } else if line.starts_with("type ") && words.len() > 1 {
                    push(&mut out, words[1], "type");
                }
            }
            _ => {}
        }
    }
    out
}

/// Tracked files with their blob hashes: what the tree holds, from git.
fn tracked(dir: &Path) -> Result<Vec<(String, String)>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "-s"])
        .output()
        .context("git ls-files")?;
    if !out.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let _mode = it.next()?;
            let blob = it.next()?.to_string();
            let _stage = it.next()?;
            let path = l.split('\t').nth(1)?.to_string();
            Some((path, blob))
        })
        .collect())
}

fn cache_path(dir: &Path) -> PathBuf {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-dir"])
        .output();
    let git_dir = out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| ".git".into());
    let git_dir = if Path::new(&git_dir).is_absolute() {
        PathBuf::from(git_dir)
    } else {
        dir.join(git_dir)
    };
    git_dir.join("forge-repomap.json")
}

/// Every tracked source file's symbols, from the cache where the blob is
/// known and parsed otherwise; the cache is rewritten with what was added.
pub fn index(dir: &Path) -> Result<Vec<(String, Vec<Symbol>)>> {
    let cache_file = cache_path(dir);
    let mut cache: Index = std::fs::read_to_string(&cache_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut files = Vec::new();
    let mut dirty = false;
    for (path, blob) in tracked(dir)? {
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !matches!(
            ext,
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "sh" | "bash" | "go"
        ) {
            continue;
        }
        let syms = match cache.blobs.get(&blob) {
            Some(s) => s.clone(),
            None => {
                let text = std::fs::read_to_string(dir.join(&path)).unwrap_or_default();
                let s = extract(&path, &text);
                cache.blobs.insert(blob.clone(), s.clone());
                dirty = true;
                s
            }
        };
        files.push((path, syms));
    }
    if dirty && let Ok(text) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&cache_file, text);
    }
    Ok(files)
}

/// Words of a task worth matching: lower-cased, split on non-identifier
/// characters, three letters or more, no stopwords.
pub fn task_words(task: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "from", "into", "add", "make", "task", "file",
        "files", "should", "must", "when", "then", "each", "its", "not", "are", "use", "new",
        "one", "all", "any", "also", "only", "test", "tests", "src",
    ];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for w in task.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let w = w.to_ascii_lowercase();
        if w.len() >= 3 && !STOP.contains(&w.as_str()) && seen.insert(w.clone()) {
            out.push(w);
        }
    }
    out
}

/// A file's score against the task: path words count three, symbol names
/// two, a hot-file prior five, and long files lose a little so a match in
/// a small file outranks the same match in a large one.
pub fn score(path: &str, syms: &[Symbol], words: &[String], hot: &[String]) -> f64 {
    let path_l = path.to_ascii_lowercase();
    let mut s = 0.0;
    for w in words {
        if path_l.contains(w.as_str()) {
            s += 3.0;
        }
        for sym in syms {
            let n = sym.name.to_ascii_lowercase();
            if n == *w {
                s += 2.0;
            } else if n.contains(w.as_str()) && w.len() >= 4 {
                s += 1.0;
            }
        }
    }
    if hot.iter().any(|h| h == path) {
        s += 5.0;
    }
    s - (syms.len() as f64 * 0.01)
}

/// The map: files by score, each on one line, until the budget in
/// characters is spent. Files that score nothing are left out unless
/// nothing scored at all, in which case the hottest files lead.
pub fn render(
    files: &[(String, Vec<Symbol>)],
    words: &[String],
    hot: &[String],
    budget: usize,
) -> String {
    let mut scored: Vec<(f64, &String, &Vec<Symbol>)> = files
        .iter()
        .map(|(p, s)| (score(p, s, words, hot), p, s))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });
    let any = scored.iter().any(|(s, _, _)| *s > 0.0);
    let mut out = String::new();
    for (s, path, syms) in scored {
        if any && s <= 0.0 {
            break;
        }
        let names: Vec<String> = syms
            .iter()
            .map(|x| {
                if x.kind == "impl" {
                    format!("impl {}", x.name)
                } else {
                    x.name.clone()
                }
            })
            .collect();
        let mut line = format!("{path}: {}", names.join(", "));
        if line.len() > 220 {
            line.truncate(217);
            line.push('…');
        }
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut dir = PathBuf::from(".");
    let mut task = String::new();
    let mut budget = 6000usize;
    let mut hot: Vec<String> = Vec::new();
    let mut cmd = "";
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "index" | "rank" => cmd = if args[i] == "index" { "index" } else { "rank" },
            "--dir" => {
                dir = PathBuf::from(&args[i + 1]);
                i += 1;
            }
            "--task" => {
                task = args[i + 1].clone();
                i += 1;
            }
            "--budget" => {
                budget = args[i + 1].parse().context("--budget")?;
                i += 1;
            }
            "--hot" => {
                hot = args[i + 1]
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                i += 1;
            }
            other => anyhow::bail!(
                "unknown argument {other}; usage: forge-repomap (index|rank) [--dir D] [--task T] [--budget CHARS] [--hot a,b]"
            ),
        }
        i += 1;
    }
    let files = index(&dir)?;
    match cmd {
        "index" => {
            let map: BTreeMap<&String, &Vec<Symbol>> = files.iter().map(|(p, s)| (p, s)).collect();
            println!("{}", serde_json::to_string_pretty(&map)?);
        }
        "rank" => {
            let words = task_words(&task);
            print!("{}", render(&files, &words, &hot, budget));
        }
        _ => anyhow::bail!(
            "usage: forge-repomap (index|rank) [--dir D] [--task T] [--budget CHARS] [--hot a,b]"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_typescript_python_and_shell_declarations_are_symbols() {
        let rs = "pub fn tick(s: &S) {}\nstruct Star {\n}\npub(crate) async fn land() {}\nimpl Star {\nimpl Display for Star {\nenum Kind { A }\n// fn not_this() {}\n";
        let names: Vec<String> = extract("src/sim/tick.rs", rs)
            .iter()
            .map(|s| format!("{} {}", s.kind, s.name))
            .collect();
        assert_eq!(
            names,
            vec![
                "fn tick",
                "struct Star",
                "fn land",
                "impl Star",
                "enum Kind"
            ]
        );
        let ts = "export function tick(s: S) {}\nexport const RESOURCES = [];\nclass Bot {}\nexport interface GameState {}\nfunction helper() {}\n";
        let names: Vec<String> = extract("src/sim/tick.ts", ts)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(
            names,
            vec!["tick", "RESOURCES", "Bot", "GameState", "helper"]
        );
        let py = "def greet(x):\n    pass\nclass Thing:\n    pass\nasync def go():\n    pass\n";
        assert_eq!(extract("a.py", py).len(), 3);
        let sh = "function one {\n}\ntwo() {\n}\n";
        assert_eq!(extract("x.sh", sh).len(), 2);
    }

    #[test]
    fn the_task_words_rank_the_files_and_the_budget_cuts() {
        let files = vec![
            (
                "src/sim/tick.rs".to_string(),
                vec![
                    Symbol {
                        name: "tick".into(),
                        kind: "fn".into(),
                    },
                    Symbol {
                        name: "rates".into(),
                        kind: "fn".into(),
                    },
                ],
            ),
            (
                "src/sim/save.rs".to_string(),
                vec![Symbol {
                    name: "serialize".into(),
                    kind: "fn".into(),
                }],
            ),
            (
                "src/ui/main.rs".to_string(),
                vec![Symbol {
                    name: "render".into(),
                    kind: "fn".into(),
                }],
            ),
        ];
        let words =
            task_words("Stars decay each tick into remnants; keep the tick size independent");
        assert!(words.contains(&"tick".to_string()) && !words.contains(&"the".to_string()));
        let map = render(&files, &words, &[], 10_000);
        assert!(map.starts_with("src/sim/tick.rs: tick, rates\n"), "{map}");
        assert!(!map.contains("main.rs"), "unscored files stay out: {map}");
        let hot = vec!["src/ui/main.rs".to_string()];
        let map = render(&files, &words, &hot, 10_000);
        assert!(
            map.contains("main.rs"),
            "the prior brings a hot file in: {map}"
        );
        let tiny = render(&files, &words, &[], 30);
        assert_eq!(tiny.lines().count(), 1, "{tiny}");
        let nothing = render(
            &files,
            &task_words("unrelated words entirely"),
            &hot,
            10_000,
        );
        assert!(
            nothing.starts_with("src/ui/main.rs"),
            "with no match, the prior leads: {nothing}"
        );
    }

    #[test]
    fn the_index_caches_by_blob_and_reparses_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
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
        std::fs::write(d.join("a.rs"), "pub fn alpha() {}\n").unwrap();
        std::fs::write(d.join("b.py"), "def beta(): pass\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "x"]);
        let first = index(d).unwrap();
        assert_eq!(first.len(), 2);
        let cache = std::fs::read_to_string(d.join(".git/forge-repomap.json")).unwrap();
        assert!(cache.contains("alpha") && cache.contains("beta"));
        std::fs::write(d.join("a.rs"), "pub fn alpha() {}\npub fn gamma() {}\n").unwrap();
        git(&["add", "."]);
        let again = index(d).unwrap();
        let a = again.iter().find(|(p, _)| p == "a.rs").unwrap();
        assert_eq!(a.1.len(), 2, "the changed file was re-parsed");
    }
}
