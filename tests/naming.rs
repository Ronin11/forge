//! The product is "Forge", not "Forge 2": task 561 renamed the environment
//! variables and the default data directory, and this task renamed the
//! remaining words. Nothing outside a dated document (docs/REVIEW.md,
//! docs/REVIEW-2.md, docs/LATER.md, docs/research/) may say "forge2" or
//! "FORGE2" again except the one-release fallback machinery from task 561
//! (the env-var resolver in `config::env`/`old_env_vars_set`, the directory
//! fallback in `ctx::Paths::resolve`/`legacy_home_migration` and
//! forge-web's own `home`, the plugin-child env export, the sandboxed
//! bin-var passthrough, and the `forge doctor` checks and docs table row
//! that report on all of that) — named explicitly below, one line at a
//! time, so a new mention cannot hide inside an already-exempt file.

use std::path::{Path, PathBuf};

/// Exact (path, trimmed line) pairs that are allowed to say "forge2" or
/// "FORGE2": the fallback resolvers task 561 built for one release, and the
/// doctor/docs lines that report on them. Anything else is the name
/// creeping back.
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/config.rs",
        "/// `FORGE_<NAME>`, falling back to `FORGE2_<NAME>` for one release: the",
    ),
    (
        "src/config.rs",
        "std::env::var(format!(\"FORGE_{name}\")).or_else(|_| std::env::var(format!(\"FORGE2_{name}\")))",
    ),
    (
        "src/config.rs",
        "/// Every `FORGE2_*` variable currently set, sorted by name: what `forge",
    ),
    (
        "src/config.rs",
        ".filter(|(k, _)| k.starts_with(\"FORGE2_\"))",
    ),
    (
        "src/config.rs",
        "/// when the new one is unset; `old_env_vars_set` names every `FORGE2_*`",
    ),
    (
        "src/config.rs",
        "std::env::remove_var(\"FORGE2_ENV_RESOLVER_TEST\");",
    ),
    (
        "src/config.rs",
        "unsafe { std::env::set_var(\"FORGE2_ENV_RESOLVER_TEST\", \"old\") };",
    ),
    (
        "src/config.rs",
        "assert!(old_env_vars_set().contains(&\"FORGE2_ENV_RESOLVER_TEST\".to_string()));",
    ),
    (
        "src/ctx.rs",
        "/// `FORGE_HOME` (`FORGE2_HOME` for one release, see `config::env`), else",
    ),
    (
        "src/ctx.rs",
        "/// `~/.local/share/forge2` when the new default does not exist yet but",
    ),
    ("src/ctx.rs", "let old = base.join(\"forge2\");"),
    (
        "src/ctx.rs",
        "/// names a home explicitly (`FORGE_HOME`, `FORGE2_HOME`, `XDG_DATA_HOME`",
    ),
    (
        "src/ctx.rs",
        "/// (`~/.local/share/forge2`) because the new one does not exist yet:",
    ),
    (
        "src/doctor.rs",
        "/// Every `FORGE2_*` variable still set: the rename to `FORGE_*` is one",
    ),
    (
        "src/doctor.rs",
        ".map(|k| format!(\"{k} -> FORGE_{}\", &k[\"FORGE2_\".len()..]))",
    ),
    (
        "src/doctor.rs",
        "/// the pre-rename default (`~/.local/share/forge2`) because the new one",
    ),
    (
        "src/doctor.rs",
        "\"mv {} {}; then in ~/.config/systemd/user/{{forge-worker,forge-web,forge-portal}}.service change Environment=FORGE_HOME=%h/.local/share/forge2 to Environment=FORGE_HOME=%h/.local/share/forge and run systemctl --user daemon-reload\",",
    ),
    (
        "src/plugins.rs",
        "/// `FORGE_HOME` (and, for one release, `FORGE2_HOME` too — the name",
    ),
    ("src/plugins.rs", ".env(\"FORGE2_HOME\", home)"),
    ("src/sandbox.rs", "|| k.starts_with(\"FORGE2_CLAUDE_BIN_\")"),
    ("src/sandbox.rs", "|| k.starts_with(\"FORGE2_CODEX_BIN\")"),
    (
        "web/src/main.rs",
        "/// Where Forge keeps its data: `FORGE_HOME` (`FORGE2_HOME` for one release),",
    ),
    (
        "web/src/main.rs",
        "/// `~/.local/share/forge2` when the new `~/.local/share/forge` does not",
    ),
    (
        "web/src/main.rs",
        "if let Ok(h) = std::env::var(\"FORGE_HOME\").or_else(|_| std::env::var(\"FORGE2_HOME\")) {",
    ),
    ("web/src/main.rs", "let old = base.join(\"forge2\");"),
    (
        "docs/PLUGINS.md",
        "| `FORGE2_HOME` | The same value as `FORGE_HOME`, kept for one release for a plugin still written against the old name; do not rely on it past that. |",
    ),
];

/// Dated documents that describe what was and are never edited for a rename:
/// their text is history, not current naming.
fn is_dated_history(rel: &Path) -> bool {
    let rel = rel.to_string_lossy();
    rel == "docs/REVIEW.md"
        || rel == "docs/REVIEW-2.md"
        || rel == "docs/LATER.md"
        || rel.starts_with("docs/research/")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn forge2_does_not_creep_back_outside_the_one_release_fallback() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for top in ["src", "web", "tui", "portal", "client", "docs"] {
        walk(&root.join(top), &mut files);
    }

    let mut violations = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        if is_dated_history(&rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        for (n, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            if !lower.contains("forge2") {
                continue;
            }
            let trimmed = line.trim();
            let allowed = ALLOWED
                .iter()
                .any(|(f, l)| *f == rel_str.as_str() && *l == trimmed);
            if !allowed {
                violations.push(format!("{}:{}: {}", rel_str, n + 1, line));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "forge2/FORGE2 found outside the one-release fallback:\n{}",
        violations.join("\n")
    );
}
