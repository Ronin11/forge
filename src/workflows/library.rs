//! The directive library (docs/EXECUTION.md): a directive's prompt may live
//! in a file beside its action, in the catalog repository, and include
//! fragments from `<catalog>/fragments/` by name. The text an attempt is
//! given is hashed, and so is each fragment it includes.

use super::{ActionDef, Problem};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The untrusted-data sentence every Forge prompt opens with: the catalog's
/// first fragment (`fragments/untrusted-data.md`), and the text every
/// built-in directive includes rather than repeats.
pub const UNTRUSTED_DATA: &str = include_str!("../builtins/fragments/untrusted-data.md");

/// The catalog directory that holds prompt fragments, one `<name>.md` each.
pub const FRAGMENTS_DIR: &str = "fragments";

/// A prompt fragment a directive's prompt includes (`{{> name}}`), with the
/// hash of the fragment's own text as it was when the prompt was rendered,
/// so an A/B on a fragment attributes to it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Include {
    pub name: String,
    pub hash: String,
}

/// The identity of a piece of prompt text: sha256, in hex.
pub fn text_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Expands every `{{> name}}` in `text` with the fragment `name` from
/// `fragments`, recursively; no parameters, no conditionals. A missing
/// fragment or a cycle is an error. `stack` is the chain being expanded;
/// `seen` collects each distinct fragment once, in order of first use.
fn expand_includes(
    fragments: &Path,
    text: &str,
    stack: &mut Vec<String>,
    seen: &mut Vec<Include>,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("{{>") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 3..];
        let Some(end) = after.find("}}") else {
            bail!("an include `{{{{>` is never closed with `}}}}`");
        };
        let name = after[..end].trim();
        rest = &after[end + 2..];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("include {{{{> {name}}}}}: a fragment name is letters, digits, `-` and `_`");
        }
        if stack.iter().any(|n| n == name) {
            bail!("include cycle: {} → {name}", stack.join(" → "));
        }
        let file = fragments.join(format!("{name}.md"));
        let raw = std::fs::read_to_string(&file).with_context(|| {
            format!(
                "include {{{{> {name}}}}}: no fragment {FRAGMENTS_DIR}/{name}.md in the catalog"
            )
        })?;
        if !seen.iter().any(|i| i.name == name) {
            seen.push(Include {
                name: name.to_string(),
                hash: text_hash(&raw),
            });
        }
        stack.push(name.to_string());
        let body = expand_includes(fragments, &raw, stack, seen)?;
        stack.pop();
        out.push_str(body.trim_end());
    }
    out.push_str(rest);
    Ok(out)
}

/// Reads a directive's `prompt_file` from the catalog (the action's own
/// directory, so it lives beside it), expands its includes from
/// `<catalog>/fragments/`, and records the text, its hash and each
/// include's hash on the action. A no-op for an action without one.
pub(super) fn load_prompt_file(dir: &Path, action_path: &Path, a: &mut ActionDef) -> Result<()> {
    let Some(file) = a.prompt_file.clone() else {
        return Ok(());
    };
    let beside = action_path.parent().unwrap_or(dir);
    let raw = std::fs::read_to_string(beside.join(&file)).with_context(|| {
        format!(
            "{}: `prompt_file` {file:?} cannot be read",
            action_path.display()
        )
    })?;
    let mut seen = Vec::new();
    let text = expand_includes(
        &dir.join(FRAGMENTS_DIR),
        raw.trim_end(),
        &mut Vec::new(),
        &mut seen,
    )
    .with_context(|| format!("{}: prompt file {file:?}", action_path.display()))?;
    a.prompt_hash = text_hash(&text);
    a.prompt = Some(text);
    a.includes = seen;
    Ok(())
}

/// Every fragment must expand: a fragment that includes a missing one, or
/// itself through others, is a problem of its own, even before a prompt
/// uses it.
pub(super) fn fragment_problems(dir: &Path) -> Vec<Problem> {
    let fragments = dir.join(FRAGMENTS_DIR);
    let Ok(rd) = std::fs::read_dir(&fragments) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    let mut problems = Vec::new();
    for p in files {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let res = std::fs::read_to_string(&p)
            .map_err(anyhow::Error::from)
            .and_then(|text| {
                expand_includes(&fragments, &text, &mut vec![name.clone()], &mut Vec::new())
            });
        if let Err(e) = res {
            problems.push(Problem {
                file: format!("{FRAGMENTS_DIR}/{name}.md"),
                blocking: true,
                what: format!("{e:#}"),
            });
        }
    }
    problems
}

/// A `prompt_file` is a path beside the action and is the action's prompt,
/// not an addition to an inline one.
pub(super) fn check_prompt_file(path: &Path, inline: bool, file: Option<&str>) -> Result<()> {
    let Some(file) = file else {
        return Ok(());
    };
    if inline {
        bail!(
            "{}: `prompt` and `prompt_file` are two ways to say one thing; use one",
            path.display()
        );
    }
    let p = Path::new(file);
    if file.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        bail!(
            "{}: `prompt_file` {file:?} must be a path beside the action, with no `..`",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn home_with(fragment: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = catalog_dir(home.path()).unwrap();
        std::fs::write(dir.join("fragments/house.md"), fragment).unwrap();
        for name in ["one", "two"] {
            std::fs::write(
                dir.join(format!("actions/{name}.toml")),
                format!(
                    "name = \"{name}\"\nkind = \"directive\"\ncontract = \"plan\"\ndescription = \"d\"\nprompt_file = \"{name}.md\"\n"
                ),
            )
            .unwrap();
            std::fs::write(
                dir.join(format!("actions/{name}.md")),
                format!("{{{{> untrusted-data}}}}\n\n{name}: {{{{> house}}}}\n"),
            )
            .unwrap();
        }
        home
    }

    #[test]
    fn a_fragment_change_changes_every_including_directives_hash() {
        let home = home_with("be brief");
        let before = load_catalog(home.path()).unwrap();
        assert!(
            before.problems.iter().all(|p| !p.blocking),
            "{:?}",
            before.problems
        );
        let one = &before.actions["one"];
        assert!(one.prompt.as_deref().unwrap().starts_with(UNTRUSTED_DATA));
        assert!(one.prompt.as_deref().unwrap().ends_with("one: be brief"));
        let names: Vec<_> = one.includes.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["untrusted-data", "house"]);
        std::fs::write(home.path().join("workflows/fragments/house.md"), "be terse").unwrap();
        let after = load_catalog(home.path()).unwrap();
        for n in ["one", "two"] {
            assert_ne!(before.actions[n].prompt_hash, after.actions[n].prompt_hash);
        }
        assert_ne!(
            before.actions["one"].prompt_hash,
            before.actions["two"].prompt_hash
        );
    }

    #[test]
    fn a_missing_include_and_a_cycle_are_blocking_problems() {
        let home = home_with("{{> ghost}}");
        let cat = load_catalog(home.path()).unwrap();
        assert!(
            cat.problems
                .iter()
                .any(|p| p.blocking && p.what.contains("ghost"))
        );
        let frag = home.path().join("workflows/fragments");
        std::fs::write(frag.join("house.md"), "{{> loop}}").unwrap();
        std::fs::write(frag.join("loop.md"), "{{> house}}").unwrap();
        let cat = load_catalog(home.path()).unwrap();
        assert!(
            cat.problems
                .iter()
                .any(|p| p.blocking && p.what.contains("cycle"))
        );
    }
}
