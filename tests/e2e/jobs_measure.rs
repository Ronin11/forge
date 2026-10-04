use crate::support::*;
use std::path::Path;

/// `forge job bench`: the repository's own `.forge/workflows/changelog-line.toml`
/// and two of its four real fixtures (`.forge/fixtures/changelog-line/`),
/// run once per provider in dry-run mode. Two fake providers, told apart
/// by `JOB_BENCH_PROVIDER` (set through each one's own `[providers.<name>].env`,
/// since the agent binary is chosen by step name alone — see
/// `tests/fakes/job-bench.sh`): "anthropic" classifies both fixtures
/// right, "devhome" is free and mislabels the fix as a chore. `bench`
/// measures exactly that gap (docs/JOBS.md, "Steps": "the bounded
/// judgment the local model is fit for").
#[test]
fn forge_job_bench_measures_two_fake_providers_over_two_fixtures() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[providers.anthropic]\n\
         runner = \"claude-cli\"\n\
         env = { JOB_BENCH_PROVIDER = \"anthropic\" }\n\
         [providers.devhome]\n\
         runner = \"claude-cli\"\n\
         env = { JOB_BENCH_PROVIDER = \"devhome\" }\n",
    )
    .unwrap();

    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "equitizr",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );

    // The real automation this task ships, copied into the project's own
    // repository the way docs/JOBS.md says an automation lives — not
    // rewritten for the test, so the test exercises exactly the files
    // that are checked in.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let wf_dir = e.repo.join(".forge/workflows");
    std::fs::create_dir_all(wf_dir.join("actions")).unwrap();
    std::fs::copy(
        root.join(".forge/workflows/changelog-line.toml"),
        wf_dir.join("changelog-line.toml"),
    )
    .unwrap();
    for f in [
        "summarise-changelog-line.toml",
        "append-changelog-line.toml",
    ] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }
    let fx_dir = e.repo.join(".forge/fixtures/changelog-line");
    std::fs::create_dir_all(&fx_dir).unwrap();
    for f in ["01-fix.json", "03-docs.json"] {
        std::fs::copy(
            root.join(".forge/fixtures/changelog-line").join(f),
            fx_dir.join(f),
        )
        .unwrap();
    }

    let o = e
        .cmd("job-bench.sh")
        .args([
            "job",
            "bench",
            "equitizr",
            "changelog-line",
            "--providers",
            "anthropic,devhome",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout);
    eprintln!("{stdout}");
    assert!(stdout.contains("anthropic"), "{stdout}");
    assert!(stdout.contains("devhome"), "{stdout}");
    assert!(
        stdout.matches("2/2 (100%)").count() >= 3,
        "both providers: 2/2 schema-valid, and anthropic also 2/2 expected-kind: {stdout}"
    );
    assert!(
        stdout.contains("1/2 (50%)"),
        "devhome mislabels the fix as a chore: {stdout}"
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "list", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 4, "two providers over two fixtures: {rows:?}");
    assert!(
        rows.iter().all(|r| r["dry_run"] == true),
        "bench never performs a real effect: {rows:?}"
    );
}

/// The engineering-weekly job (scheduler task 7 of 7; docs/JOBS.md, "Where
/// an automation lives"): the real workflow and its three scripts, copied
/// unmodified into the project's own repository except for
/// `SRC_FILE_MAX_LINES`, lowered from 3000 to 5 so a small fixture file
/// crosses it deterministically and fast, with no Cargo.toml in the
/// fixture to make measure.sh actually build or lint anything. A dry run
/// still measures — the first `row` effect carries the JSON document — but
/// files nothing: the second `row` effect says it would have, marked
/// "(dry run)", and `forge log` afterward still shows no task on the
/// project, proving `forge add` itself was never called.
#[test]
fn engineering_weekly_dry_run_measures_and_files_nothing() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "acme", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let wf_dir = e.repo.join(".forge/workflows");
    std::fs::create_dir_all(wf_dir.join("actions")).unwrap();

    let wf_text =
        std::fs::read_to_string(root.join(".forge/workflows/engineering-weekly.toml")).unwrap();
    let wf_text = wf_text.replace(
        "SRC_FILE_MAX_LINES = \"3000\"",
        "SRC_FILE_MAX_LINES = \"5\"",
    );
    assert!(
        wf_text.contains("SRC_FILE_MAX_LINES = \"5\""),
        "the real workflow's threshold line changed shape; update this test's replace"
    );
    std::fs::write(wf_dir.join("engineering-weekly.toml"), wf_text).unwrap();

    for f in ["measure-engineering.toml", "review-if-crossed.toml"] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }
    for f in ["measure.sh", "compare-thresholds.sh", "skip-if-reviewed.sh"] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }

    // A fixture file over the test's lowered threshold, nowhere near a
    // real 3000-line one, and no Cargo.toml: measure.sh's cargo
    // test/clippy block never runs.
    std::fs::create_dir_all(e.repo.join("src")).unwrap();
    std::fs::write(
        e.repo.join("src/big.rs"),
        "// a fixture line, over the lowered threshold\n".repeat(10),
    )
    .unwrap();

    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "engineering-weekly fixture"]);

    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "acme",
            "engineering-weekly",
            "--dry-run",
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["dry_run"], true);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");

    assert_eq!(effects[0]["kind"], "row");
    assert_eq!(effects[0]["target"], "measurements.json");
    let measured = effects[0]["summary"].as_str().unwrap();
    assert!(measured.contains("kernel_lines"), "{measured}");
    assert!(measured.contains("clippy_warnings"), "{measured}");

    assert_eq!(effects[1]["kind"], "row");
    assert_eq!(effects[1]["target"], "task");
    let filed = effects[1]["summary"].as_str().unwrap();
    assert!(filed.contains("docs/REVIEW"), "{filed}");
    assert!(filed.contains("over 5 lines"), "{filed}");
    assert!(filed.contains("(dry run)"), "{filed}");

    // Nothing was actually filed: `forge add` was never called.
    let tasks: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["log", "--project", "acme", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 0, "{tasks:?}");

    // A real run (the dry run above never counted against `per_day`)
    // really does call `forge add`: the crossed threshold lands as a
    // queued task whose text starts with "docs/REVIEW".
    let o = e.forge(
        "ok.sh",
        &["job", "start", "acme", "engineering-weekly", "--now"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let tasks: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["log", "--project", "acme", "--json"])
            .stdout,
    )
    .unwrap();
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0]["state"], "queued");
    assert!(
        tasks[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("docs/REVIEW"),
        "{tasks:?}"
    );
}

/// measure.sh's `modules_without_tests` names only kernel modules: a test
/// file (`*_tests.rs`, `tests.rs`) is not itself a module without tests,
/// and a module its parent declares as `#[cfg(test)] mod <name>;` is test
/// code. With foo.rs declaring `foo_tests`, foo_tests.rs, and a bar.rs with
/// no tests, only bar.rs is listed. No Cargo.toml, so no cargo runs.
#[test]
fn measure_lists_only_modules_without_tests_not_test_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("foo.rs"),
        "pub fn foo() {}\n\n#[cfg(test)]\nmod foo_tests;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("foo_tests.rs"),
        "#[test]\nfn foo_runs() {\n    super::foo();\n}\n",
    )
    .unwrap();
    std::fs::write(src.join("bar.rs"), "pub fn bar() {}\n").unwrap();

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let effects = dir.path().join("effects.log");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join(".forge/workflows/actions/measure.sh");
    let o = std::process::Command::new("bash")
        .arg(&script)
        .current_dir(&work)
        .env("FORGE_REPO_DIR", &repo)
        .env("FORGE_EFFECT_LOG", &effects)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let measured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(work.join("measurements.json")).unwrap()).unwrap();
    assert_eq!(
        measured["modules_without_tests"],
        serde_json::json!(["src/bar.rs"]),
        "{measured}"
    );
}
