use std::process::{Command, Output};

fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success()
    );
    for (path, text) in files {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    assert!(
        Command::new("git")
            .current_dir(dir.path())
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    dir
}

fn run(dir: &tempfile::TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge-repomap"))
        .arg("--dir")
        .arg(dir.path())
        .args(args)
        .output()
        .unwrap()
}

fn output(dir: &tempfile::TempDir, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn one_match_exact_before_partial_and_current_body() {
    let dir = fixture(&[("a.rs", "fn work() {\n    old();\n}\nfn worker() {}\n")]);
    std::fs::write(
        dir.path().join("a.rs"),
        "fn work() {\n    new();\n}\nfn worker() {}\n",
    )
    .unwrap();
    assert_eq!(
        output(&dir, &["def", "work"]),
        "a.rs:1-3\n     1\tfn work() {\n     2\t    new();\n     3\t}\n"
    );
    assert_eq!(
        output(&dir, &["def", "worker"]),
        "a.rs:4-4\n     4\tfn worker() {}\n"
    );
}

#[test]
fn ambiguous_matches_have_no_bodies_and_can_be_scoped() {
    let dir = fixture(&[
        ("a.py", "def work():\n    return 1\n"),
        ("b.py", "def work():\n    return 2\n"),
    ]);
    assert_eq!(
        output(&dir, &["def", "work"]),
        "a.py:1-2 def def work():\nb.py:1-2 def def work():\n"
    );
    assert_eq!(
        output(&dir, &["def", "work", "--in", "b.py"]),
        "b.py:1-2\n     1\tdef work():\n     2\t    return 2\n"
    );
    assert_eq!(
        output(&dir, &["def", "wor"]),
        output(&dir, &["def", "work"])
    );
}

#[test]
fn none_suggests_outline_and_unknown_file_fails() {
    let dir = fixture(&[("a.rs", "fn work() {}\n")]);
    assert_eq!(
        output(&dir, &["def", "absent"]),
        "No match for absent; try forge-repomap outline <path>.\n"
    );
    let out = run(&dir, &["outline", "missing.rs"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown tracked file: missing.rs"));
    std::fs::write(dir.path().join("untracked.rs"), "fn other() {}\n").unwrap();
    assert!(!run(&dir, &["outline", "untracked.rs"]).status.success());
}

#[test]
fn truncated_body_is_capped_at_250_numbered_lines() {
    let text = format!("fn long() {{\n{}}}\n", "    work();\n".repeat(250));
    let dir = fixture(&[("a.rs", &text)]);
    let out = output(&dir, &["def", "long"]);
    assert!(out.starts_with("a.rs:1-252\n     1\tfn long() {\n"));
    assert_eq!(out.lines().count(), 252);
    assert!(
        out.ends_with("   250\t    work();\n...truncated, Read a.rs offset/limit for the rest\n")
    );
}

#[test]
fn outline_preserves_repeated_methods_and_qualified_def_resolves_impl() {
    let dir = fixture(&[(
        "a.rs",
        "impl A {\n    fn work() {}\n}\nimpl B {\n    fn work() {}\n}\nimpl A {\n    fn other() {}\n}\n",
    )]);
    assert_eq!(
        output(&dir, &["outline", "a.rs"]),
        "1-3 impl impl A {\n2-2 fn fn work() {}\n4-6 impl impl B {\n5-5 fn fn work() {}\n7-9 impl impl A {\n8-8 fn fn other() {}\n"
    );
    assert_eq!(
        output(&dir, &["def", "work"]),
        "a.rs:2-2 fn fn work() {}\na.rs:5-5 fn fn work() {}\n"
    );
    assert_eq!(
        output(&dir, &["def", "B::work"]),
        "a.rs:5-5\n     5\t    fn work() {}\n"
    );
    assert_eq!(
        output(&dir, &["def", "A::other"]),
        "a.rs:8-8\n     8\t    fn other() {}\n"
    );
}

#[test]
fn outline_supports_each_extractor_and_empty_files() {
    let dir = fixture(&[
        ("a.rs", "fn work() {}\n"),
        ("a.ts", "function work() {}\n"),
        ("a.py", "def work():\n    pass\n"),
        ("a.sh", "work() {\n  true\n}\n"),
        ("a.go", "func work() {}\n"),
        ("empty.rs", ""),
        ("a.txt", "hello"),
    ]);
    for path in ["a.rs", "a.ts", "a.py", "a.sh", "a.go"] {
        let out = output(&dir, &["outline", path]);
        assert!(out.starts_with("1-"), "{path}: {out}");
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("work"));
    }
    assert_eq!(output(&dir, &["outline", "empty.rs"]), "");
    let out = run(&dir, &["outline", "a.txt"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no extractor"));
}
