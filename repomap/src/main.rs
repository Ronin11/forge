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

mod edges;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    /// The line the declaration starts on (1-based) and its real end: by
    /// brace depth from the declaration's first `{` in Rust, TypeScript,
    /// JavaScript and Go; the last more-indented line in Python; the
    /// matching `}` in shell. A declaration with no resolvable body ends
    /// on its own start line. A cheap span a reader can hand to Read
    /// offset/limit. Zero on an entry cached before spans existed, which
    /// `Index` treats as a miss so the blob re-parses.
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub end: usize,
    /// The declaration line, trimmed and capped at about 160 characters:
    /// enough to tell a reader what the symbol is without opening the file.
    #[serde(default)]
    pub sig: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Index {
    /// blob sha -> symbols, the cache; paths map onto blobs per tree.
    pub blobs: HashMap<String, Vec<Symbol>>,
    /// blob sha -> raw import specifiers, cached the same way; resolving
    /// them to real paths happens fresh every run against the file set.
    #[serde(default)]
    pub edges: HashMap<String, Vec<edges::RawImport>>,
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

    /// A blob's cache file for one kind of cached data (`"sym2"`, `"edges"`),
    /// sharded two hex characters deep so no directory holds every blob.
    fn path_for(&self, kind: &str, blob: &str) -> PathBuf {
        self.dir
            .join(&blob[..2.min(blob.len())])
            .join(format!("{blob}.{kind}.json"))
    }

    /// The pre-span, pre-`sig` cache format's path: kept only so a test can
    /// prove an entry written there is never read back as `sym2`.
    #[cfg(test)]
    fn path(&self, blob: &str) -> PathBuf {
        self.dir
            .join(&blob[..2.min(blob.len())])
            .join(format!("{blob}.json"))
    }

    /// Symbols cached under kind `sym2`: bumped from the unkeyed format
    /// spans and `sig` replaced, so an old entry simply misses instead of
    /// being misread as one with a real end and a signature.
    pub fn get(&self, blob: &str) -> Option<Vec<Symbol>> {
        let text = std::fs::read_to_string(self.path_for("sym2", blob)).ok()?;
        serde_json::from_str::<Vec<Symbol>>(&text)
            .ok()
            .filter(|syms| syms.iter().all(|x| x.start > 0))
    }

    pub fn put(&self, blob: &str, syms: &[Symbol]) {
        let path = self.path_for("sym2", blob);
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(".{blob}.sym2.{}.tmp", std::process::id()));
        if let Ok(text) = serde_json::to_string(syms)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// A `Vec<Symbol>` that also dedups by (name, kind, line) in O(1) instead of a
/// linear scan per push, while keeping first-seen order.
#[derive(Default)]
struct Sink {
    out: Vec<Symbol>,
    seen: HashSet<(String, String, usize)>,
    /// The 1-based line the extractor is on; every push records it.
    line: usize,
    /// The current line, trimmed and capped; every push records it as the
    /// symbol's `sig`.
    sig: String,
}

impl Sink {
    /// Marks the extractor's position: `line_no` for spans, `line` (already
    /// trimmed of leading space) capped at 160 characters for `sig`.
    fn at(&mut self, line_no: usize, line: &str) {
        self.line = line_no;
        self.sig = line.trim_end().chars().take(160).collect();
    }

    fn push(&mut self, name: &str, kind: &str) {
        let name = name.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if name.is_empty() {
            return;
        }
        if self
            .seen
            .insert((name.to_string(), kind.to_string(), self.line))
        {
            self.out.push(Symbol {
                name: name.to_string(),
                kind: kind.to_string(),
                start: self.line,
                end: self.line,
                sig: self.sig.clone(),
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
    for (li, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        sink.at(li + 1, line);
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
    for (li, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        sink.at(li + 1, line);
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
    for (li, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        sink.at(li + 1, line);
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
    for (li, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        sink.at(li + 1, line);
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
    for (li, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        sink.at(li + 1, line);
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
fn extract_names(path: &str, text: &str) -> Vec<Symbol> {
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

/// How a language's declarations close, for `real_end` below: Rust's `'a`
/// is a lifetime rather than a char literal and it has no backtick or
/// `#`-comment syntax; Curly (TypeScript, JavaScript, Go) treats every
/// quote as a plain multi-character string, including backtick template
/// literals; Shell has `#` line comments and no `/* */` block comments.
#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Rust,
    Curly,
    Shell,
}

/// The char index each 1-based line starts at, so a symbol's `start` line
/// can be turned into a scan position without re-walking the file.
fn line_starts_of(chars: &[char]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// A declaration's real end, scanning forward from its first character:
/// brace depth from its first unescaped `{`, skipping string and char
/// literals and comments (a Rust `'a` is a lifetime, not a char literal),
/// until the depth returns to zero; or, if a bare `;` comes first, the
/// line it is on. `None` when neither closes before the file ends (an
/// unmatched or malformed declaration), left for the caller to fall back
/// to the start line.
fn real_end(chars: &[char], line_starts: &[usize], start_line: usize, lang: Lang) -> Option<usize> {
    let mut i = *line_starts.get(start_line - 1)?;
    let mut line = start_line;
    let mut depth = 0i32;
    let mut seen_open = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            '#' if lang == Lang::Shell => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if lang != Lang::Shell && chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if lang != Lang::Shell && chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(chars.len());
            }
            '`' if lang == Lang::Curly => {
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            'r' | 'b'
                if lang == Lang::Rust
                    && !(i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) =>
            {
                // `r"…"`, `r#"…"#`, `br#"…"#`: a raw string, no escapes.
                let mut j = i;
                if chars[j] == 'b' {
                    j += 1;
                }
                if chars.get(j) == Some(&'r') {
                    j += 1;
                    let mut hashes = 0;
                    while chars.get(j) == Some(&'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if chars.get(j) == Some(&'"') {
                        j += 1;
                        while j < chars.len() {
                            if chars[j] == '\n' {
                                line += 1;
                            } else if chars[j] == '"'
                                && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#'))
                            {
                                j += hashes;
                                break;
                            }
                            j += 1;
                        }
                        i = (j + 1).min(chars.len());
                        continue;
                    }
                }
                i += 1; // an identifier, or a byte string the `"` arm handles
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '\'' if lang == Lang::Rust => {
                if chars.get(i + 1) == Some(&'\\') {
                    let bound = (i + 12).min(chars.len());
                    let mut j = i + 2;
                    while j < bound && chars[j] != '\'' && chars[j] != '\n' {
                        j += 1;
                    }
                    if j < bound && chars.get(j) == Some(&'\'') {
                        i = j + 1;
                    } else {
                        i += 1; // a lifetime, not a char literal
                    }
                } else if chars.get(i + 1).is_some() && chars.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1; // a lifetime, not a char literal
                }
            }
            '\'' => {
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if lang == Lang::Curly && chars[i] == '\\' {
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '{' => {
                depth += 1;
                seen_open = true;
                i += 1;
            }
            '}' => {
                depth -= 1;
                i += 1;
                if seen_open && depth <= 0 {
                    return Some(line);
                }
            }
            ';' if depth == 0 && !seen_open => return Some(line),
            _ => i += 1,
        }
    }
    None
}

/// Leading spaces and tabs, for Python's indentation-based spans.
fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// A Python `def` or `class`'s real end: the last line indented deeper
/// than its own, blank lines skipped over rather than ending the span.
fn python_end(lines: &[&str], start_line: usize) -> usize {
    let idx = start_line.saturating_sub(1);
    let Some(&decl) = lines.get(idx) else {
        return start_line;
    };
    let base_indent = indent_of(decl);
    let mut end_idx = idx;
    for (i, l) in lines.iter().enumerate().skip(idx + 1) {
        if l.trim().is_empty() {
            continue;
        }
        if indent_of(l) > base_indent {
            end_idx = i;
        } else {
            break;
        }
    }
    end_idx + 1
}

/// The declarations of a file with real spans: each symbol's `end` comes
/// from its own declaration, never guessed from the next one's position.
pub fn extract(path: &str, text: &str) -> Vec<Symbol> {
    // The compact repository map keeps one entry per name and kind.
    let mut seen = HashSet::new();
    extract_all(path, text)
        .into_iter()
        .filter(|s| seen.insert((s.name.clone(), s.kind.clone())))
        .collect()
}

fn extract_all(path: &str, text: &str) -> Vec<Symbol> {
    let mut syms = extract_names(path, text);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let chars: Vec<char> = text.chars().collect();
    let line_starts = line_starts_of(&chars);
    let lines: Vec<&str> = text.lines().collect();
    for sym in &mut syms {
        sym.end = match ext {
            "rs" => real_end(&chars, &line_starts, sym.start, Lang::Rust).unwrap_or(sym.start),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "go" => {
                real_end(&chars, &line_starts, sym.start, Lang::Curly).unwrap_or(sym.start)
            }
            "sh" | "bash" => {
                real_end(&chars, &line_starts, sym.start, Lang::Shell).unwrap_or(sym.start)
            }
            "py" => python_end(&lines, sym.start),
            _ => sym.start,
        };
    }
    syms
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
    if words.is_empty() {
        // No task to rank against (the shared map every task on a base
        // gets, docs/CONTEXT.md): a file that declares more is worth more,
        // capped so one giant file cannot crowd the map.
        s + (syms.len().min(40) as f64 * 0.05)
    } else {
        s - (syms.len() as f64 * 0.01)
    }
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
    spans: bool,
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
        if words.is_empty() && syms.is_empty() {
            // A file with nothing declared says nothing in a map ranked by
            // nothing; leave the budget for files that do.
            continue;
        }
        let names: Vec<String> = syms
            .iter()
            .map(|x| {
                let name = if x.kind == "impl" {
                    format!("impl {}", x.name)
                } else {
                    x.name.clone()
                };
                if spans && x.start > 0 {
                    format!("{name}@{}", x.start)
                } else {
                    name
                }
            })
            .collect();
        // One line per file, at most 220 characters: when the symbols do
        // not fit, the trailing ones are dropped whole (with an ellipsis),
        // never cut mid-name and never stripped of their `@start`.
        let mut line = format!("{path}:");
        let mut dropped = false;
        for (i, name) in names.iter().enumerate() {
            let sep = if i == 0 { " " } else { ", " };
            if line.len() + sep.len() + name.len() > 218 {
                dropped = true;
                break;
            }
            line.push_str(sep);
            line.push_str(name);
        }
        if dropped {
            line.push_str(", …");
        }
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

const USAGE: &str = "usage: forge-repomap (index|rank) [--dir D] [--task T] [--budget CHARS] [--hot a,b] [--cache DIR] [--changed-since SHA] [--no-spans]\n       forge-repomap edges <root> [--cache DIR]\n       forge-repomap outline <path> [--dir D]\n       forge-repomap def <name> [--in <path>] [--dir D]";

#[derive(Debug)]
struct Args {
    dir: PathBuf,
    task: String,
    budget: usize,
    /// Render `name@start` (the default); `--no-spans` renders names
    /// only, the control arm of the `map` experiment factor.
    spans: bool,
    hot: Vec<String>,
    cache: Option<PathBuf>,
    since: Option<String>,
    cmd: String,
    query: Option<String>,
    in_path: Option<String>,
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
    let mut spans = true;
    let mut hot: Vec<String> = Vec::new();
    let mut cache: Option<PathBuf> = None;
    let mut since: Option<String> = None;
    let mut cmd = String::new();
    let mut query = None;
    let mut in_path = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "index" | "rank" | "edges" | "outline" | "def" if cmd.is_empty() => {
                cmd = args[i].clone();
            }
            "--in" => {
                in_path = Some(flag_value(args, i, "--in")?.to_string());
                i += 1;
            }
            other
                if matches!(cmd.as_str(), "outline" | "def")
                    && !other.starts_with("--")
                    && query.is_none() =>
            {
                query = Some(other.to_string());
            }
            "--dir" => {
                dir = PathBuf::from(flag_value(args, i, "--dir")?);
                i += 1;
            }
            "--no-spans" => {
                spans = false;
                i += 1;
                continue;
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
            other if cmd == "edges" && !other.starts_with("--") => {
                dir = PathBuf::from(other);
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
        query,
        in_path,
        spans,
    })
}

/// Read current working files together with their spans; the index cache is
/// keyed by staged blobs and may not describe an edited working file.
fn navigate(dir: &Path, cmd: &str, query: &str, in_path: Option<&str>) -> Result<String> {
    use std::fmt::Write;

    let filter = if cmd == "outline" {
        Some(query)
    } else {
        in_path
    };
    let filter = filter.map(|p| p.strip_prefix("./").unwrap_or(p));
    let tracked = tracked(dir)?;
    if let Some(path) = filter
        && !tracked.iter().any(|(p, _)| p == path)
    {
        anyhow::bail!("unknown tracked file: {path}");
    }
    let mut files = Vec::new();
    for (path, _) in tracked {
        if filter.is_some_and(|p| p != path) {
            continue;
        }
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !EXTRACTORS.iter().any(|(exts, _)| exts.contains(&ext)) {
            if filter.is_some() {
                anyhow::bail!("no extractor for tracked file: {path}");
            }
            continue;
        }
        let text =
            std::fs::read_to_string(dir.join(&path)).with_context(|| format!("read {path}"))?;
        let symbols = extract_all(&path, &text);
        files.push((path, text, symbols));
    }
    let mut out = String::new();
    if cmd == "outline" {
        for (_, _, symbols) in &files {
            for s in symbols {
                writeln!(out, "{}-{} {} {}", s.start, s.end, s.kind, s.sig)?;
            }
        }
        return Ok(out);
    }
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for (path, text, symbols) in &files {
        for s in symbols {
            let qualified = query.rsplit_once("::").is_some_and(|(owner, method)| {
                s.name == method
                    && s.kind == "fn"
                    && symbols.iter().any(|parent| {
                        parent.kind == "impl"
                            && parent.name == owner
                            && parent.start < s.start
                            && parent.end >= s.end
                    })
            });
            if s.name == query || qualified {
                exact.push((path, text, s));
            } else if s.name.contains(query) {
                partial.push((path, text, s));
            }
        }
    }
    let matches = if exact.is_empty() { partial } else { exact };
    match matches.as_slice() {
        [] => writeln!(
            out,
            "No match for {query}; try forge-repomap outline <path>."
        )?,
        [(path, text, s)] => {
            writeln!(out, "{path}:{}-{}", s.start, s.end)?;
            for (i, line) in text
                .lines()
                .enumerate()
                .skip(s.start - 1)
                .take((s.end - s.start + 1).min(250))
            {
                writeln!(out, "{:6}\t{line}", i + 1)?;
            }
            if s.end - s.start + 1 > 250 {
                writeln!(out, "...truncated, Read {path} offset/limit for the rest")?;
            }
        }
        _ => {
            for (path, _, s) in matches {
                writeln!(out, "{path}:{}-{} {} {}", s.start, s.end, s.kind, s.sig)?;
            }
        }
    }
    Ok(out)
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
        query,
        in_path,
        spans,
    } = parse_args(&args)?;
    let shared = cache
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
        .map(BlobCache::new);
    match cmd.as_str() {
        "outline" | "def" => {
            let query = query.context("outline/def needs a path/name")?;
            print!("{}", navigate(&dir, &cmd, &query, in_path.as_deref())?);
        }
        "index" => {
            let (files, parsed) = index(&dir, shared.as_ref())?;
            let map: BTreeMap<&String, &Vec<Symbol>> = files.iter().map(|(p, s)| (p, s)).collect();
            println!("{}", serde_json::to_string_pretty(&map)?);
            eprintln!("{} file(s), {parsed} parsed, the rest cached", files.len());
        }
        "rank" => {
            let (files, parsed) = index(&dir, shared.as_ref())?;
            let changed = since
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|b| changed_since(&dir, b))
                .unwrap_or_default();
            let words = task_words(&task);
            print!("{}", render(&files, &words, &hot, &changed, budget, spans));
            eprintln!(
                "{} file(s), {parsed} parsed, {} changed since base",
                files.len(),
                changed.len()
            );
        }
        "edges" => {
            let (graph, parsed) = edges::build(&dir, shared.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&graph)?);
            eprintln!(
                "{} node(s), {} edge(s), {parsed} parsed",
                graph.nodes.len(),
                graph.edges.len()
            );
        }
        _ => anyhow::bail!("{USAGE}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    /// Every declaration carries the line it starts on and its real end —
    /// the matching brace, not the next declaration's position — in Rust,
    /// TypeScript and shell alike; the map renders `name@start`.
    #[test]
    fn spans_cover_the_file_from_the_first_declaration_in_three_languages() {
        let rs = "use x;\n\npub fn a() {\n  1\n}\n\nstruct B {\n  x: i32,\n}\n";
        let syms = extract("m.rs", rs);
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        let b = syms.iter().find(|s| s.name == "B").unwrap();
        assert_eq!((a.start, a.end), (3, 5));
        assert_eq!((b.start, b.end), (7, 9));
        let ts = "import x from 'y';\nexport function f() {}\nexport class C {}\n";
        let syms = extract("m.ts", ts);
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        let c = syms.iter().find(|s| s.name == "C").unwrap();
        assert_eq!((f.start, f.end), (2, 2));
        assert_eq!((c.start, c.end), (3, 3));
        let sh = "#!/bin/bash\nfoo() {\n  :\n}\nbar() {\n  :\n}\n";
        let syms = extract("m.sh", sh);
        let foo = syms.iter().find(|s| s.name == "foo").unwrap();
        let bar = syms.iter().find(|s| s.name == "bar").unwrap();
        assert_eq!((foo.start, foo.end), (2, 4));
        assert_eq!((bar.start, bar.end), (5, 7));
        let out = render(
            &[("m.rs".to_string(), extract("m.rs", rs))],
            &["a".to_string()],
            &[],
            &[],
            1000,
            true,
        );
        assert!(out.contains("a@3,") || out.contains("a@3\n"), "{out}");
        let plain = render(
            &[("m.rs".to_string(), extract("m.rs", rs))],
            &["a".to_string()],
            &[],
            &[],
            1000,
            false,
        );
        assert!(
            plain.contains("m.rs: a, B") && !plain.contains('@'),
            "{plain}"
        );
    }

    /// A long file drops its trailing symbols whole, spans intact, rather
    /// than cutting a name in the middle.
    #[test]
    fn a_long_line_drops_trailing_symbols_before_touching_spans() {
        let syms: Vec<Symbol> = (0..60)
            .map(|i| Symbol {
                name: format!("symbol_number_{i}"),
                kind: "fn".into(),
                start: i * 3 + 1,
                end: i * 3 + 3,
                sig: String::new(),
            })
            .collect();
        let out = render(&[("big.rs".to_string(), syms)], &[], &[], &[], 1000, true);
        let line = out.lines().next().unwrap();
        assert!(line.len() <= 222, "{}", line.len());
        assert!(line.ends_with(", …"), "{line}");
        for piece in line
            .trim_start_matches("big.rs: ")
            .trim_end_matches(", …")
            .split(", ")
        {
            assert!(
                piece.contains('@') && piece.split('@').nth(1).unwrap().parse::<usize>().is_ok(),
                "{piece}"
            );
        }
    }

    /// With no task words (the shared map), files that declare more lead and
    /// files that declare nothing are left out, instead of the reverse.
    #[test]
    fn without_task_words_files_with_symbols_lead_and_empty_files_are_omitted() {
        let sym = |n: &str| Symbol {
            name: n.into(),
            kind: "fn".into(),
            start: 0,
            end: 0,
            sig: String::new(),
        };
        let files = vec![
            ("scripts/empty.sh".to_string(), vec![]),
            ("src/small.rs".to_string(), vec![sym("one")]),
            ("src/big.rs".to_string(), vec![sym("a"), sym("b"), sym("c")]),
        ];
        let out = render(&files, &[], &[], &[], 10_000, true);
        let big = out.find("src/big.rs").unwrap();
        let small = out.find("src/small.rs").unwrap();
        assert!(big < small, "{out}");
        assert!(!out.contains("scripts/empty.sh"), "{out}");
    }
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

    /// A nested `fn`'s span is its own braces, not swallowed by the
    /// enclosing one: brace depth tracks both independently.
    #[test]
    fn a_nested_fn_span_is_its_own_braces_not_the_enclosing_ones() {
        let rs = "fn outer() {\n    fn inner() {\n        1\n    }\n    2\n}\n";
        let syms = extract("m.rs", rs);
        let outer = syms.iter().find(|s| s.name == "outer").unwrap();
        let inner = syms.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!((outer.start, outer.end), (1, 6));
        assert_eq!((inner.start, inner.end), (2, 4));
    }

    /// An `impl` block's span covers every method inside it, closing on
    /// the brace that matches the block's own, not the first method's.
    #[test]
    fn an_impl_blocks_span_covers_its_methods() {
        let rs = "impl Star {\n    fn a(&self) {}\n    fn b(&self) {\n        1\n    }\n}\n";
        let syms = extract("m.rs", rs);
        let imp = syms.iter().find(|s| s.name == "Star").unwrap();
        let b = syms.iter().find(|s| s.name == "b").unwrap();
        assert_eq!((imp.start, imp.end), (1, 6));
        assert_eq!((b.start, b.end), (3, 5));
    }

    /// A `}` inside a string literal never closes the brace it sits in.
    #[test]
    fn a_brace_inside_a_string_literal_does_not_close_the_span() {
        let rs = "fn f() {\n    let s = \"}\";\n}\n";
        let syms = extract("m.rs", rs);
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert_eq!((f.start, f.end), (1, 3));
    }

    /// A `}` or `"` inside a raw string never closes the span, and a raw
    /// string's trailing backslash is not an escape.
    #[test]
    fn a_brace_inside_a_raw_string_does_not_close_the_span() {
        let rs = "pub fn example() {\n    let _s = r#\"a quote \" and a brace }\"#;\n}\n";
        let syms = extract("m.rs", rs);
        assert_eq!(syms[0].end, 3);
        let rs = "fn g() {\n    let _s = r\"\\\";\n    let _t = 1;\n}\n";
        let syms = extract("m.rs", rs);
        let g = syms.iter().find(|s| s.name == "g").unwrap();
        assert_eq!((g.start, g.end), (1, 4));
    }

    /// `'a` is a lifetime, not a char literal, so it never sends the scan
    /// looking for a closing quote that isn't there.
    #[test]
    fn a_lifetime_is_not_mistaken_for_a_char_literal() {
        let rs = "fn f<'a>(x: &'a str) -> &'a str {\n    x\n}\n";
        let syms = extract("m.rs", rs);
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert_eq!((f.start, f.end), (1, 3));
    }

    /// A declaration with no body ends on its own line, as soon as the
    /// `;` is reached with no `{` seen first.
    #[test]
    fn a_one_line_declaration_ends_on_its_own_line() {
        let rs = "type Alias = Star;\npub trait Speak {\n    fn talk(&self);\n}\n";
        let syms = extract("m.rs", rs);
        let alias = syms.iter().find(|s| s.name == "Alias").unwrap();
        let talk = syms.iter().find(|s| s.name == "talk").unwrap();
        assert_eq!((alias.start, alias.end), (1, 1));
        assert_eq!((talk.start, talk.end), (3, 3));
    }

    /// A Python class's span runs to the last line of its last method,
    /// blank lines inside the body skipped rather than ending the span.
    #[test]
    fn a_python_classs_span_covers_its_methods() {
        let py = "class Foo:\n    def a(self):\n        return 1\n\n    def b(self):\n        return 2\n";
        let syms = extract("f.py", py);
        let foo = syms.iter().find(|s| s.name == "Foo").unwrap();
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        let b = syms.iter().find(|s| s.name == "b").unwrap();
        assert_eq!((foo.start, foo.end), (1, 6));
        assert_eq!((a.start, a.end), (2, 3));
        assert_eq!((b.start, b.end), (5, 6));
    }

    /// A shell function's span runs to its matching `}`, past semicolons
    /// and an `if`/`then`/`fi` inside its body.
    #[test]
    fn a_shell_functions_span_ends_at_the_matching_brace() {
        let sh = "foo() {\n    echo 1\n    if true; then\n        echo 2\n    fi\n}\n";
        let syms = extract("f.sh", sh);
        let foo = syms.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!((foo.start, foo.end), (1, 6));
    }

    /// `sig` is the declaration line, trimmed, capped at 160 characters.
    #[test]
    fn sig_is_the_trimmed_declaration_line_capped_at_160_chars() {
        let rs = "    pub fn tick(s: &S) {}\n";
        let syms = extract("m.rs", rs);
        assert_eq!(syms[0].sig, "pub fn tick(s: &S) {}");
        let long_name = "a".repeat(200);
        let rs = format!("pub fn {long_name}() {{}}\n");
        let syms = extract("m.rs", &rs);
        assert_eq!(syms[0].sig.chars().count(), 160);
        assert!(rs.trim().starts_with(&syms[0].sig));
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
                        start: 0,
                        end: 0,
                        sig: String::new(),
                    },
                    Symbol {
                        name: "rates".into(),
                        kind: "fn".into(),
                        start: 0,
                        end: 0,
                        sig: String::new(),
                    },
                ],
            ),
            (
                "src/sim/save.rs".to_string(),
                vec![Symbol {
                    name: "serialize".into(),
                    kind: "fn".into(),
                    start: 0,
                    end: 0,
                    sig: String::new(),
                }],
            ),
            (
                "src/ui/main.rs".to_string(),
                vec![Symbol {
                    name: "render".into(),
                    kind: "fn".into(),
                    start: 0,
                    end: 0,
                    sig: String::new(),
                }],
            ),
        ];
        let words =
            task_words("Stars decay each tick into remnants; keep the tick size independent");
        assert!(words.contains(&"tick".to_string()) && !words.contains(&"the".to_string()));
        let map = render(&files, &words, &[], &[], 10_000, true);
        assert!(map.starts_with("src/sim/tick.rs: tick, rates\n"), "{map}");
        assert!(!map.contains("main.rs"), "unscored files stay out: {map}");
        let hot = vec!["src/ui/main.rs".to_string()];
        let map = render(&files, &words, &hot, &[], 10_000, true);
        assert!(
            map.contains("main.rs"),
            "the prior brings a hot file in: {map}"
        );
        let tiny = render(&files, &words, &[], &[], 30, true);
        assert_eq!(tiny.lines().count(), 1, "{tiny}");
        let nothing = render(
            &files,
            &task_words("unrelated words entirely"),
            &hot,
            &[],
            10_000,
            true,
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
            true,
        );
        assert!(map.starts_with("b.py:"), "{map}");
    }

    /// An entry written at the pre-`sig` cache path (the format `sym2`
    /// replaces) is never read back as one: `get` misses, forcing a
    /// re-parse instead of misreading spans and signatures that were
    /// never computed.
    #[test]
    fn an_old_sym_cache_entry_is_ignored_in_favour_of_sym2() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path());
        let blob = "deadbeefcafe";
        let old_path = cache.path(blob);
        std::fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        let stale = vec![Symbol {
            name: "stale".into(),
            kind: "fn".into(),
            start: 1,
            end: 1,
            sig: String::new(),
        }];
        std::fs::write(&old_path, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(
            cache.get(blob).is_none(),
            "an old-format entry must not be read as sym2"
        );
        let fresh = vec![Symbol {
            name: "fresh".into(),
            kind: "fn".into(),
            start: 1,
            end: 2,
            sig: "fn fresh() {".into(),
        }];
        cache.put(blob, &fresh);
        assert_eq!(cache.get(blob), Some(fresh));
    }
}
