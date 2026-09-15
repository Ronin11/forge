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

/// A shared, content-addressed cache: one small file per blob, written
/// atomically, so every clone of a repository reuses what any other
/// parsed, and a branch costs only the blobs it changed. Landing warms it
/// for free: the last map on a branch already parsed the tree that is
/// about to become the base.
pub struct BlobCache {
    dir: PathBuf,
}

impl BlobCache {
    pub fn new(dir: &Path) -> BlobCache {
        BlobCache {
            dir: dir.to_path_buf(),
        }
    }

    fn path(&self, blob: &str) -> PathBuf {
        self.dir
            .join(&blob[..2.min(blob.len())])
            .join(format!("{blob}.json"))
    }

    pub fn get(&self, blob: &str) -> Option<Vec<Symbol>> {
        let text = std::fs::read_to_string(self.path(blob)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put(&self, blob: &str, syms: &[Symbol]) {
        let path = self.path(blob);
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(".{blob}.{}.tmp", std::process::id()));
        if let Ok(text) = serde_json::to_string(syms)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// A `Vec<Symbol>` that also dedups by (name, kind) in O(1) instead of a
/// linear scan per push, while keeping first-seen order.
#[derive(Default)]
struct Sink {
    out: Vec<Symbol>,
    seen: HashSet<(String, String)>,
}

impl Sink {
    fn push(&mut self, name: &str, kind: &str) {
        let name = name.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if name.is_empty() {
            return;
        }
        if self.seen.insert((name.to_string(), kind.to_string())) {
            self.out.push(Symbol {
                name: name.to_string(),
                kind: kind.to_string(),
            });
        }
    }
}

/// A line split on the punctuation that separates a declaration's keywords
/// from its name, shared by every extractor below.
fn words_of(line: &str) -> Vec<&str> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '(' | '<' | '{' | ':' | '=' | ';'))
        .filter(|w| !w.is_empty())
        .collect()
}

fn extract_rust(text: &str) -> Vec<Symbol> {
    let mut sink = Sink::default();
    for raw in text.lines() {
        let line = raw.trim_start();
        let words = words_of(line);
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
                sink.push(words[i + 1], kind);
            }
            if words[i] == "impl" {
                let target = words[i + 1..]
                    .iter()
                    .find(|w| !w.starts_with('\'') && **w != "for")
                    .copied()
                    .unwrap_or("");
                if let Some(t) = words[i + 1..].iter().position(|w| *w == "for") {
                    sink.push(words[i + 1 + t + 1], "impl");
                } else {
                    sink.push(target, "impl");
                }
            }
        }
        if line.starts_with("const ") && words.len() > 1 {
            sink.push(words[1], "const");
        }
    }
    sink.out
}

fn extract_typescript(text: &str) -> Vec<Symbol> {
    let mut sink = Sink::default();
    for raw in text.lines() {
        let line = raw.trim_start();
        let words = words_of(line);
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
                sink.push(words[i + 1], kind);
            }
        }
    }
    sink.out
}

fn extract_python(text: &str) -> Vec<Symbol> {
    let mut sink = Sink::default();
    for raw in text.lines() {
        let line = raw.trim_start();
        let words = words_of(line);
        if (line.starts_with("def ") || line.starts_with("async def ")) && words.len() > 1 {
            sink.push(words[if words[0] == "async" { 2 } else { 1 }], "def");
        } else if line.starts_with("class ") && words.len() > 1 {
            sink.push(words[1], "class");
        }
    }
    sink.out
}

fn extract_shell(text: &str) -> Vec<Symbol> {
    let mut sink = Sink::default();
    for raw in text.lines() {
        let line = raw.trim_start();
        if let Some(rest) = line.strip_prefix("function ") {
            sink.push(
                rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or(""),
                "function",
            );
        } else if let Some((name, rest)) = line.split_once("()")
            && !name.contains(char::is_whitespace)
            && rest.trim_start().starts_with('{')
        {
            sink.push(name, "function");
        }
    }
    sink.out
}

fn extract_go(text: &str) -> Vec<Symbol> {
    let mut sink = Sink::default();
    for raw in text.lines() {
        let line = raw.trim_start();
        let words = words_of(line);
        if line.starts_with("func ") && words.len() > 1 {
            let name = if words[1].starts_with('(') || words[1].is_empty() {
                words.get(3).copied().unwrap_or("")
            } else {
                words[1]
            };
            sink.push(name, "func");
        } else if line.starts_with("type ") && words.len() > 1 {
            sink.push(words[1], "type");
        }
    }
    sink.out
}

/// Extensions to their extractor: adding a language means adding one row
/// and one function, never touching the others.
type Extractor = fn(&str) -> Vec<Symbol>;
const EXTRACTORS: &[(&[&str], Extractor)] = &[
    (&["rs"], extract_rust),
    (&["ts", "tsx", "js", "jsx", "mjs"], extract_typescript),
    (&["py"], extract_python),
    (&["sh", "bash"], extract_shell),
    (&["go"], extract_go),
];

/// Symbols in one file, by its extension. Regex-free, line-oriented: the
/// declarations a reader would scan for, never bodies.
pub fn extract(path: &str, text: &str) -> Vec<Symbol> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    EXTRACTORS
        .iter()
        .find(|(exts, _)| exts.contains(&ext))
        .map(|(_, f)| f(text))
        .unwrap_or_default()
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

/// Every tracked source file's symbols: from the shared blob cache when
/// given one, else the clone's own cache file, parsed only where the blob
/// is new. Returns the files and how many blobs had to be parsed.
/// A tree's files with their symbols, in tracked order.
pub type Files = Vec<(String, Vec<Symbol>)>;

pub fn index(dir: &Path, shared: Option<&BlobCache>) -> Result<(Files, usize)> {
    // The shared cache, when given, is the only cache: one source of truth
    // that every clone both reads and seeds.
    let cache_file = cache_path(dir);
    let mut cache: Index = if shared.is_some() {
        Index::default()
    } else {
        std::fs::read_to_string(&cache_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    };
    let mut files = Vec::new();
    let mut dirty = false;
    let mut parsed = 0;
    for (path, blob) in tracked(dir)? {
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !EXTRACTORS.iter().any(|(exts, _)| exts.contains(&ext)) {
            continue;
        }
        let syms = if let Some(s) = shared.and_then(|c| c.get(&blob)) {
            s
        } else if let Some(s) = cache.blobs.get(&blob).filter(|_| shared.is_none()) {
            s.clone()
        } else {
            let text = std::fs::read_to_string(dir.join(&path)).unwrap_or_default();
            let s = extract(&path, &text);
            parsed += 1;
            if let Some(c) = shared {
                c.put(&blob, &s);
            } else {
                cache.blobs.insert(blob.clone(), s.clone());
                dirty = true;
            }
            s
        };
        files.push((path, syms));
    }
    if dirty && let Ok(text) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&cache_file, text);
    }
    Ok((files, parsed))
}

/// Files the branch has changed since `base`: what earlier attempts on
/// this work touched, which the next one most likely needs again.
pub fn changed_since(dir: &Path, base: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--name-only", base, "HEAD"])
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
pub fn score(
    path: &str,
    syms: &[Symbol],
    words: &[String],
    hot: &[String],
    changed: &[String],
) -> f64 {
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
    if changed.iter().any(|c| c == path) {
        s += 6.0;
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
    changed: &[String],
    budget: usize,
) -> String {
    let mut scored: Vec<(f64, &String, &Vec<Symbol>)> = files
        .iter()
        .map(|(p, s)| (score(p, s, words, hot, changed), p, s))
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

const USAGE: &str = "usage: forge-repomap (index|rank) [--dir D] [--task T] [--budget CHARS] [--hot a,b] [--cache DIR] [--changed-since SHA]";

#[derive(Debug)]
struct Args {
    dir: PathBuf,
    task: String,
    budget: usize,
    hot: Vec<String>,
    cache: Option<PathBuf>,
    since: Option<String>,
    cmd: String,
}

/// The value following a flag, or an error with the usage line when the
/// flag is the last argument instead of a panic on an out-of-bounds index.
fn flag_value<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str> {
    args.get(i + 1)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value; {USAGE}"))
}

fn parse_args(args: &[String]) -> Result<Args> {
    let mut dir = PathBuf::from(".");
    let mut task = String::new();
    let mut budget = 6000usize;
    let mut hot: Vec<String> = Vec::new();
    let mut cache: Option<PathBuf> = None;
    let mut since: Option<String> = None;
    let mut cmd = String::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "index" | "rank" => cmd = if args[i] == "index" { "index" } else { "rank" }.into(),
            "--dir" => {
                dir = PathBuf::from(flag_value(args, i, "--dir")?);
                i += 1;
            }
            "--task" => {
                task = flag_value(args, i, "--task")?.to_string();
                i += 1;
            }
            "--budget" => {
                budget = flag_value(args, i, "--budget")?
                    .parse()
                    .context("--budget")?;
                i += 1;
            }
            "--hot" => {
                hot = flag_value(args, i, "--hot")?
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                i += 1;
            }
            "--cache" => {
                cache = Some(PathBuf::from(flag_value(args, i, "--cache")?));
                i += 1;
            }
            "--changed-since" => {
                since = Some(flag_value(args, i, "--changed-since")?.to_string());
                i += 1;
            }
            other => anyhow::bail!("unknown argument {other}; {USAGE}"),
        }
        i += 1;
    }
    Ok(Args {
        dir,
        task,
        budget,
        hot,
        cache,
        since,
        cmd,
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Args {
        dir,
        task,
        budget,
        hot,
        cache,
        since,
        cmd,
    } = parse_args(&args)?;
    let shared = cache
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
        .map(BlobCache::new);
    let (files, parsed) = index(&dir, shared.as_ref())?;
    let changed = since
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|b| changed_since(&dir, b))
        .unwrap_or_default();
    match cmd.as_str() {
        "index" => {
            let map: BTreeMap<&String, &Vec<Symbol>> = files.iter().map(|(p, s)| (p, s)).collect();
            println!("{}", serde_json::to_string_pretty(&map)?);
            eprintln!("{} file(s), {parsed} parsed, the rest cached", files.len());
        }
        "rank" => {
            let words = task_words(&task);
            print!("{}", render(&files, &words, &hot, &changed, budget));
            eprintln!(
                "{} file(s), {parsed} parsed, {} changed since base",
                files.len(),
                changed.len()
            );
        }
        _ => anyhow::bail!("{USAGE}"),
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
    fn go_functions_and_types_are_symbols() {
        let go = "package main\n\nfunc Alpha() {\n}\n\ntype Widget struct {\n}\n";
        let names: Vec<String> = extract("a.go", go)
            .iter()
            .map(|s| format!("{} {}", s.kind, s.name))
            .collect();
        assert_eq!(names, vec!["func Alpha", "type Widget"]);
    }

    #[test]
    fn rust_traits_type_aliases_and_modules_are_symbols() {
        let rs = "pub trait Speak {\n    fn talk(&self);\n}\ntype Alias = Star;\nmod inner {\n}\n";
        let names: Vec<String> = extract("src/sim/tick.rs", rs)
            .iter()
            .map(|s| format!("{} {}", s.kind, s.name))
            .collect();
        assert_eq!(
            names,
            vec!["trait Speak", "fn talk", "type Alias", "mod inner"]
        );
    }

    #[test]
    fn a_trailing_flag_with_no_value_is_an_error_not_a_panic() {
        let args: Vec<String> = ["forge-repomap", "rank", "--task"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = parse_args(&args).unwrap_err().to_string();
        assert!(err.contains("--task"), "{err}");
        assert!(err.contains("usage:"), "{err}");
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
        let map = render(&files, &words, &[], &[], 10_000);
        assert!(map.starts_with("src/sim/tick.rs: tick, rates\n"), "{map}");
        assert!(!map.contains("main.rs"), "unscored files stay out: {map}");
        let hot = vec!["src/ui/main.rs".to_string()];
        let map = render(&files, &words, &hot, &[], 10_000);
        assert!(
            map.contains("main.rs"),
            "the prior brings a hot file in: {map}"
        );
        let tiny = render(&files, &words, &[], &[], 30);
        assert_eq!(tiny.lines().count(), 1, "{tiny}");
        let nothing = render(
            &files,
            &task_words("unrelated words entirely"),
            &hot,
            &[],
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
        let (first, parsed) = index(d, None).unwrap();
        assert_eq!((first.len(), parsed), (2, 2));
        let cache = std::fs::read_to_string(d.join(".git/forge-repomap.json")).unwrap();
        assert!(cache.contains("alpha") && cache.contains("beta"));
        std::fs::write(d.join("a.rs"), "pub fn alpha() {}\npub fn gamma() {}\n").unwrap();
        git(&["add", "."]);
        let (again, parsed) = index(d, None).unwrap();
        let a = again.iter().find(|(p, _)| p == "a.rs").unwrap();
        assert_eq!(a.1.len(), 2, "the changed file was re-parsed");
        assert_eq!(parsed, 1, "only the changed blob");
        git(&["commit", "-qm", "y"]);
        // A shared cache: a second clone parses nothing the first one did.
        let shared_dir = tempfile::tempdir().unwrap();
        let shared = BlobCache::new(shared_dir.path());
        let (_, parsed) = index(d, Some(&shared)).unwrap();
        assert_eq!(parsed, 2, "first use of the shared cache parses everything");
        let clone = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args([
                    "clone",
                    "-q",
                    d.to_str().unwrap(),
                    clone.path().to_str().unwrap()
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        let (files, parsed) = index(clone.path(), Some(&shared)).unwrap();
        assert_eq!((files.len(), parsed), (2, 0), "the clone reused every blob");
        // The branch's own changes rank first.
        std::fs::write(
            clone.path().join("b.py"),
            "def beta(): pass\ndef delta(): pass\n",
        )
        .unwrap();
        let git2 = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(clone.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git2(&["config", "user.email", "t@e"]);
        git2(&["config", "user.name", "t"]);
        git2(&["commit", "-qam", "more"]);
        let changed = changed_since(clone.path(), "HEAD~1");
        assert_eq!(changed, vec!["b.py"]);
        let (files, _) = index(clone.path(), Some(&shared)).unwrap();
        let map = render(
            &files,
            &task_words("nothing in particular"),
            &[],
            &changed,
            10_000,
        );
        assert!(map.starts_with("b.py:"), "{map}");
    }
}
