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
