//! Captures the short commit hash at build time so `forge version` can
//! report exactly which checkout produced the binary. A build from a
//! tree with no `.git` (the archive `forge deploy` builds a landed
//! commit in, see docs/DEPLOY.md) names its commit in `FORGE_BUILD_SHA`.
//! `FORGE_GIT_SHA_FULL` is the full hash, the id `deploy-self` names its
//! release by, so `init --relink` names the same commit the same way.

fn main() {
    println!("cargo:rerun-if-env-changed=FORGE_BUILD_SHA");
    let named = std::env::var("FORGE_BUILD_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let rev_parse = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let sha = named
        .clone()
        .unwrap_or_else(|| rev_parse(&["rev-parse", "--short", "HEAD"]));
    let full = named.unwrap_or_else(|| rev_parse(&["rev-parse", "HEAD"]));
    println!("cargo:rustc-env=FORGE_GIT_SHA={sha}");
    println!("cargo:rustc-env=FORGE_GIT_SHA_FULL={full}");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
