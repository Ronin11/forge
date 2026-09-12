//! Clients never touch the kernel. A UI that links the engine, the store,
//! or the verifier stops being a client and starts being a second place
//! the rules live; Forge 1's web server embedded the engine and that is
//! where its worst findings came from.

use std::path::Path;

#[test]
fn the_tui_depends_on_nothing_of_the_kernel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("tui/Cargo.toml")).unwrap();
    let deps = manifest.split("[dependencies]").nth(1).unwrap_or("");
    for forbidden in ["forge =", "forge = {", "rusqlite", "tokio"] {
        assert!(
            !deps.contains(forbidden),
            "tui/Cargo.toml must not depend on {forbidden}: it is a client of the CLI's JSON"
        );
    }
    for entry in std::fs::read_dir(root.join("tui/src")).unwrap() {
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
