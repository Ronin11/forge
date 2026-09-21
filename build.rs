//! Captures the short commit hash at build time so `forge version` can
//! report exactly which checkout produced the binary. A build from a
//! tree with no `.git` (the archive `forge deploy` builds a landed
//! commit in, see docs/DEPLOY.md) names its commit in `FORGE_BUILD_SHA`.

fn main() {
    println!("cargo:rerun-if-env-changed=FORGE_BUILD_SHA");
    let named = std::env::var("FORGE_BUILD_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let sha = named.unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    });
    println!("cargo:rustc-env=FORGE_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
