//! Re-run the repository's declared checks in the worktree. A claim from the
//! agent is not a result; this is.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

#[derive(Serialize, Debug)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub exit: Option<i32>,
    pub ms: u128,
    pub tail: String,
}

pub fn run_all(cwd: &Path, checks: &BTreeMap<String, Vec<String>>) -> Vec<CheckResult> {
    let mut results = Vec::new();
    for (name, argv) in checks {
        let start = Instant::now();
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .output();
        let ms = start.elapsed().as_millis();
        let r = match output {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                let tail: Vec<&str> = text.lines().rev().take(20).collect();
                CheckResult {
                    name: name.clone(),
                    ok: o.status.success(),
                    exit: o.status.code(),
                    ms,
                    tail: tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
                }
            }
            Err(e) => CheckResult {
                name: name.clone(),
                ok: false,
                exit: None,
                ms,
                tail: e.to_string(),
            },
        };
        eprintln!(
            "  {} {} ({:.1}s)",
            if r.ok { "✓" } else { "✗" },
            r.name,
            ms as f64 / 1000.0
        );
        if !r.ok && !r.tail.is_empty() {
            for l in r.tail.lines() {
                eprintln!("      {l}");
            }
        }
        results.push(r);
    }
    results
}
