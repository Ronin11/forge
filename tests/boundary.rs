//! Clients never touch the kernel. A UI that links the engine, the store,
//! or the verifier stops being a client and starts being a second place
//! the rules live; Forge 1's web server embedded the engine and that is
//! where its worst findings came from. `forge.toml` marks this file "the
//! one rule a task may not relax": it parses every member manifest with
//! the `toml` crate (so a real dependency table, not a text search,
//! decides what's forbidden — `forge-client` never collides with `forge`
//! the way a substring check risks), covers `dev-dependencies` and
//! `build-dependencies` too, tests `repomap` separately as a tool rather
//! than a client, and checks that no client source file invokes a `forge`
//! verb `docs/CLIENT.md` doesn't document.

use std::collections::BTreeSet;
use std::path::Path;

const SOURCE_FORBIDDEN: &[&str] = &["forge::", "rusqlite", "forge.db", "sqlite"];

fn dep_tables(manifest: &toml::Value) -> impl Iterator<Item = &toml::Table> {
    ["dependencies", "dev-dependencies", "build-dependencies"]
        .into_iter()
        .filter_map(|k| manifest.get(k))
        .filter_map(|v| v.as_table())
}

/// Every workspace member that is a client: the root Cargo.toml's member
/// list minus `repomap`, which is a tool and tested separately below. Read
/// from the manifest so the next client is covered the day it is added;
/// the portal went a week unchecked under a hand-written list.
fn clients(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    let members: Vec<String> = manifest["workspace"]["members"]
        .as_array()
        .expect("[workspace] members is an array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .filter(|m| m != "repomap")
        .collect();
    for known in ["tui", "web", "client", "portal"] {
        assert!(
            members.iter().any(|m| m == known),
            "{known} is a client and must be a workspace member"
        );
    }
    members
}

fn assert_manifest_forbids(member: &str, manifest_path: &Path, forbidden: &[&str]) {
    let text = std::fs::read_to_string(manifest_path).unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    for table in dep_tables(&manifest) {
        for name in table.keys() {
            assert!(
                !forbidden.contains(&name.as_str()),
                "{member}'s manifest ({}) must not depend on {name}: it is a client of the CLI's JSON",
                manifest_path.display()
            );
        }
    }
}

fn assert_sources_clean(src_dir: &Path) {
    for entry in std::fs::read_dir(src_dir).unwrap() {
        let path = entry.unwrap().path();
        let src = std::fs::read_to_string(&path).unwrap();
        for forbidden in SOURCE_FORBIDDEN {
            assert!(
                !src.contains(forbidden),
                "{} must not reach past the CLI: found {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn the_clients_depend_on_nothing_of_the_kernel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for member in clients(root) {
        let member = member.as_str();
        assert_manifest_forbids(
            member,
            &root.join(member).join("Cargo.toml"),
            &["forge", "rusqlite", "tokio"],
        );
        assert_sources_clean(&root.join(member).join("src"));
    }
}

#[test]
fn repomap_is_a_tool_not_a_client() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // No kernel, no store; unlike the clients above, tokio isn't forbidden
    // for a standalone tool — it's fine as long as repomap declares it
    // itself, which today it doesn't.
    assert_manifest_forbids(
        "repomap",
        &root.join("repomap").join("Cargo.toml"),
        &["forge", "rusqlite"],
    );
    assert_sources_clean(&root.join("repomap").join("src"));
}

#[test]
fn client_sources_only_invoke_documented_verbs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let documented = documented_verbs(root);
    assert!(
        !documented.is_empty(),
        "docs/CLIENT.md's verb fence parsed empty"
    );
    for member in clients(root) {
        for entry in std::fs::read_dir(root.join(&member).join("src")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            for verb in invoked_verbs(&src) {
                assert!(
                    documented.contains(verb.as_str()),
                    "{} invokes `forge {verb}`, which docs/CLIENT.md's verb list does not document",
                    path.display()
                );
            }
        }
    }
}

/// The fenced verb list docs/CLIENT.md carries under `## Verbs` for exactly
/// this test to read, instead of scraping the prose bullets above it.
fn documented_verbs(root: &Path) -> BTreeSet<String> {
    let doc = std::fs::read_to_string(root.join("docs/CLIENT.md")).unwrap();
    let after_heading = doc
        .split("## Verbs")
        .nth(1)
        .expect("docs/CLIENT.md has a ## Verbs section");
    let fence_start = after_heading
        .find("```text")
        .expect("## Verbs has a ```text fence listing the verb names")
        + "```text".len();
    let fenced = &after_heading[fence_start..];
    let fence_end = fenced.find("```").expect("the verb fence is closed");
    fenced[..fence_end]
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Verb names a client source file passes as the first argument to
/// `forge`, e.g. `forge.json(&["snapshot"])`, `.args(["events", ...])`, or
/// `vec!["retry", ...]` built up for `forge.run`.
fn invoked_verbs(src: &str) -> Vec<String> {
    let mut verbs = Vec::new();
    for marker in ["&[\"", ".args([\"", "vec![\""] {
        let mut rest = src;
        while let Some(i) = rest.find(marker) {
            let after = &rest[i + marker.len()..];
            if let Some(end) = after.find('"') {
                let token = &after[..end];
                if !token.is_empty() && token.chars().all(|c| c.is_ascii_lowercase()) {
                    verbs.push(token.to_string());
                }
            }
            rest = after;
        }
    }
    verbs
}
