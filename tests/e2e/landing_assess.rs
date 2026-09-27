use crate::support::*;
use std::path::Path;

#[test]
fn assessment_of_a_merged_landing_only_names_its_own_change() {
    let e = Env::new();
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["add", "answer.txt"]);
    git(&e.repo, &["commit", "-qm", "seed answer"]);
    let command = |fake: &str, args: &[&str]| {
        let mut c = e.cmd(fake);
        for (role, script) in [("REVIEW", "reviewer-ok.sh"), ("ASSESS", "assessor.sh")] {
            c.env(
                format!("FORGE_CLAUDE_BIN_{role}"),
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fakes")
                    .join(script),
            );
        }
        let o = c.args(args).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    // Both branches start on the same base, before either is landed.
    for (fake, task) in [
        ("addfile.sh", "add extra"),
        ("appendhello.sh", "extend hello"),
    ] {
        command(
            fake,
            &[
                "run",
                e.repo.to_str().unwrap(),
                task,
                "--workflow",
                "reviewed",
                "--no-land",
                "--retries",
                "0",
            ],
        );
    }
    command("ok.sh", &["land", "1"]);
    command("ok.sh", &["land", "2"]);
    let score: i64 = e
        .db()
        .query_row("SELECT score FROM assessments WHERE task_id=2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(score, 7);
    let merged = git(&e.origin, &["rev-list", "--parents", "-n", "1", "main"]);
    assert_eq!(merged.split_whitespace().count(), 3, "{merged}");
    let log = std::fs::read_dir(e.home.join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("assess-2-")
        })
        .unwrap();
    let text = std::fs::read_to_string(log).unwrap();
    let prompt: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let diff = prompt["text"]
        .as_str()
        .unwrap()
        .split("The landed diff (base..landed):\n")
        .nth(1)
        .unwrap();
    assert!(diff.contains("diff --git a/hello.sh b/hello.sh"), "{diff}");
    assert!(!diff.contains("extra.txt"), "{diff}");
    assert_eq!(diff.matches("diff --git ").count(), 1, "{diff}");
}
