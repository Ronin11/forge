//! Captures the short commit hash at build time so `forge version` can
//! report exactly which checkout produced the binary.

fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=FORGE_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
