use crate::support::*;
use std::path::Path;

#[test]
fn engineering_weekly_function_threshold_alias_and_all_crossings() {
    let scratch = tempfile::tempdir().unwrap();
    let measurements = serde_json::json!({
        "longest_functions": [
            {"at": "src/other.rs:1", "signature": "fn first()", "lines": 450},
            {"at": "src/other.rs:452", "signature": "fn second()", "lines": 425}
        ]
    });
    std::fs::write(
        scratch.path().join("measurements.json"),
        measurements.to_string(),
    )
    .unwrap();
    let log = scratch.path().join("effects");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".forge/workflows/actions/compare-thresholds.sh");
    for (primary, alias, bound) in [
        (None, None, 400),
        (None, Some("420"), 420),
        (None, Some("450"), 450),
        (Some("400"), Some("500"), 400),
        (Some("450"), Some("400"), 450),
    ] {
        std::fs::write(&log, "").unwrap();
        let mut command = std::process::Command::new("bash");
        command
            .arg(&script)
            .current_dir(scratch.path())
            .env("FORGE_EFFECT_LOG", &log)
            .env("FORGE_DRY_RUN", "1")
            .env_remove("FUNCTION_MAX_LINES")
            .env_remove("RUN_TASK_MAX_LINES");
        if let Some(value) = primary {
            command.env("FUNCTION_MAX_LINES", value);
        }
        if let Some(value) = alias {
            command.env("RUN_TASK_MAX_LINES", value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let effect = std::fs::read_to_string(&log).unwrap();
        if bound == 450 {
            assert_eq!(effect, "row\tthresholds\tno threshold crossed\n");
        } else {
            for (at, signature, lines) in [
                ("src/other.rs:1", "fn first()", 450),
                ("src/other.rs:452", "fn second()", 425),
            ] {
                let expected = format!("{at} {signature} is {lines} lines (over {bound})");
                assert!(effect.contains(&expected), "{effect}");
            }
            assert_eq!(effect.lines().count(), 1, "one task carries all crossings");
            assert!(effect.ends_with("(dry run)\n"), "{effect}");
        }
    }
}

/// `compare-thresholds.sh` reads `.longest_functions[]` from
/// measurements.json rather than re-deriving `run_task`'s own length
/// with a second awk pass over `src/engine.rs` — so a function over
/// `FUNCTION_MAX_LINES` (400 by default, `RUN_TASK_MAX_LINES` only an
/// alias for the default) names itself in the filed task regardless of
/// which file it lives in. Same real workflow and scripts as
/// `engineering_weekly_dry_run_measures_and_files_nothing`, copied
/// unmodified, against a fixture file (not engine.rs) holding a single
/// function: first 450 lines, over the bound, then 300, under it.
#[test]
fn engineering_weekly_dry_run_names_a_long_function_over_threshold() {
    let e = Env::new();
    setup_engineering_weekly_project(&e, "acme");
    std::fs::create_dir_all(e.repo.join("src")).unwrap();

    // A 450-line function (over the 400-line default) in a file that
    // is not engine.rs, and no Cargo.toml, so measure.sh's cargo
    // test/clippy block never runs.
    let mut over = String::from("fn big_fn() {\n");
    over.push_str(&"    let _x = 0;\n".repeat(448));
    over.push_str("}\n");
    std::fs::write(e.repo.join("src/longfn.rs"), &over).unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "over-threshold fixture"]);

    let doc = run_engineering_weekly(&e, "acme", true);
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[1]["target"], "task");
    let filed = effects[1]["summary"].as_str().unwrap();
    assert!(
        filed.contains("src/longfn.rs:1 fn big_fn() is 450 lines (over 400)"),
        "{filed}"
    );
    assert!(filed.contains("(dry run)"), "{filed}");

    // Replace the fixture with a 300-line function, under the bound,
    // and a #[cfg(test)] block so the file doesn't also trip the
    // modules-without-tests check.
    let mut under = String::from("fn mid_fn() {\n");
    under.push_str(&"    let _x = 0;\n".repeat(298));
    under.push_str("}\n");
    under.push_str("#[cfg(test)]\nmod tests {\n    #[test]\n    fn trivial() {}\n}\n");
    std::fs::write(e.repo.join("src/longfn.rs"), &under).unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "under-threshold fixture"]);

    let doc = run_engineering_weekly(&e, "acme", true);
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[1]["target"], "thresholds");
    assert_eq!(effects[1]["summary"], "no threshold crossed");
}

/// compare-thresholds.sh collects every crossing rather than stopping at
/// the first: a fixture file over both `SRC_FILE_MAX_LINES` (lowered to
/// 100) and `FUNCTION_MAX_LINES` (400) yields one dry-run task line that
/// counts and names both, with no stale "REVIEW-3" prefix.
#[test]
fn engineering_weekly_dry_run_names_every_crossed_threshold() {
    let e = Env::new();
    setup_engineering_weekly_project(&e, "acme");
    let wf_path = e.repo.join(".forge/workflows/engineering-weekly.toml");
    let wf_text = std::fs::read_to_string(&wf_path).unwrap().replace(
        "SRC_FILE_MAX_LINES = \"3000\"",
        "SRC_FILE_MAX_LINES = \"100\"",
    );
    assert!(wf_text.contains("SRC_FILE_MAX_LINES = \"100\""));
    std::fs::write(&wf_path, wf_text).unwrap();

    // 450-line function plus a #[cfg(test)] block: 455 lines in all.
    std::fs::create_dir_all(e.repo.join("src")).unwrap();
    let mut both = String::from("fn big_fn() {\n");
    both.push_str(&"    let _x = 0;\n".repeat(448));
    both.push_str("}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn trivial() {}\n}\n");
    std::fs::write(e.repo.join("src/both.rs"), &both).unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "two-threshold fixture"]);

    let doc = run_engineering_weekly(&e, "acme", true);
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[1]["target"], "task");
    let filed = effects[1]["summary"].as_str().unwrap();
    assert!(
        filed.starts_with("engineering-weekly crossed 2 threshold(s)"),
        "{filed}"
    );
    assert!(
        filed.contains("src/both.rs:1 fn big_fn() is 450 lines (over 400)"),
        "{filed}"
    );
    assert!(
        filed.contains("src file src/both.rs is 455 lines (over 100)"),
        "{filed}"
    );
    assert!(!filed.contains("REVIEW-3"), "{filed}");
    assert!(filed.ends_with("(dry run)"), "{filed}");
}
