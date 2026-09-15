//! `forge-client` (`client/`) is a client of the CLI like `tui`, `repomap`
//! and `web`: it must not open the database or link the kernel either.
//! `tests/boundary.rs` already checks those three; it is a protected file
//! (its own header says the checks it already runs must not be edited by
//! the task that adds this crate), so this file runs the same check
//! against `client/` instead of extending it in place.

use std::path::Path;

#[test]
fn the_client_crate_depends_on_nothing_of_the_kernel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("client").join("Cargo.toml")).unwrap();
    let deps = manifest.split("[dependencies]").nth(1).unwrap_or("");
    for forbidden in ["forge =", "forge = {", "rusqlite", "tokio"] {
        assert!(
            !deps.contains(forbidden),
            "client/Cargo.toml must not depend on {forbidden}: it is a client of the CLI's JSON"
        );
    }
    for entry in std::fs::read_dir(root.join("client").join("src")).unwrap() {
        let path = entry.unwrap().path();
        let src = std::fs::read_to_string(&path).unwrap();
        for forbidden in ["forge::", "rusqlite", "forge.db", "sqlite"] {
            assert!(
                !src.contains(forbidden),
                "{} must not reach past the CLI: found {forbidden}",
                path.display()
            );
        }
    }
}
