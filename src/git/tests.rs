/// Every Git constructor is inventoried. All general-purpose helpers must
/// immediately harden their invocation, regardless of who supplies the path.
/// Only mirror metadata inspection is exempt: its sole production caller
/// receives bare origins from the registered project repositories (see the
/// caller guard below). Kernel-only pushes retain their separate path guard.
const CALL_SITES: &[&str] = &[
    // Kernel-owned.
    "kernel_repository:&dir",
    "kernel_tree:&kernel",
    "stage:&kernel",
    "verification_checkout:&kernel",
    "verification_checkout:&fresh",
    "verification_checkout:&fresh",
    "place_branch:&src",
    "published:&kernel",
    "push_sha:&kernel",
    // Hardened fetch into an agent's clone.
    "place_branch:dir",
    // Caller-named: always hardened, including agent clones.
    "current_branch:repo",
    "ref_exists:repo",
    "clone_task:repo",
    "clone_task:dir",
    "fetch_ref:dir",
    "fetch_full_ref:dir",
    "update_ref:repo",
    "merge_base:dir",
    "subject:dir",
    "fetch_branch:repo",
    "rev_parse:repo",
    "merge:dir",
    "merge:dir",
    "is_ancestor:dir",
    "changed_paths_between:wt",
    "file_patch:wt",
    "catch_up_branch:repo",
    "graft:repo",
    "graft:repo",
    "commit_all:dir",
    "commit_all:dir",
    "commit_paths:dir",
    "commit_paths:dir",
    "init_commit_all:dir",
    "reset_hard:dir",
    "reset_tracked:dir",
    "head:dir",
    "show_file:dir",
    "count_commits:wt",
    "changed_paths:wt",
    "committed_paths:wt",
    "diff_lines:repo",
    "changed_with_status:wt",
    "diff_text:dir",
    "diff_line_total:dir",
    "diff_shortstat:dir",
    "diff_stat_tree:dir",
    "log_oneline:dir",
    "dirty_paths:wt",
    "dirty_tracked_paths:wt",
    "dirty_files:wt",
    "unstage:wt",
    "clear_namespace:dest",
    "remote_url:repo",
    "ls_tree:repo",
    "archive_into:repo",
    "archive_all:repo",
    "identity:git_dir",
    "hand_commit_count:repo",
    "remote_branch_exists:\".\"",
    "remote_branch_sha:\".\"",
    // Caller-named: a registered repository's bare origin, which
    // `forge init --mirror` hooks.
    "is_bare:dir",
    "hooks_dir:dir",
    "config_get:dir",
    "config_set:dir",
];

fn call_sites() -> Vec<String> {
    let src = include_str!("../git.rs");
    let src = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
    let mut current = String::new();
    let mut found = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        let t = t
            .strip_prefix("pub(crate) ")
            .or(t.strip_prefix("pub "))
            .unwrap_or(t);
        if let Some(rest) = t.strip_prefix("async fn ").or(t.strip_prefix("fn ")) {
            current = rest.split(['(', '<']).next().unwrap().to_string();
        }
        let mut rest = line;
        while let Some(i) = rest.find("Git::new(") {
            rest = &rest[i + "Git::new(".len()..];
            let arg = &rest[..rest.find(')').unwrap()];
            found.push(format!("{current}:{arg}"));
        }
    }
    found
}

#[test]
fn every_git_call_site_is_on_the_held_list() {
    let mut found = call_sites();
    let mut held: Vec<String> = CALL_SITES.iter().map(|s| s.to_string()).collect();
    found.sort();
    held.sort();
    assert_eq!(found, held);
}

#[test]
fn no_push_or_landing_step_runs_git_in_a_callers_directory() {
    let kernel_only = [
        "stage",
        "published",
        "push_sha",
        "push_ref",
        "push",
        "push_to_repo",
    ];
    for site in call_sites() {
        let (func, arg) = site.split_once(':').unwrap();
        if kernel_only.contains(&func) {
            assert!(arg.contains("kernel"), "{site}");
        }
    }
}

#[test]
fn caller_named_git_sites_are_hardened_or_have_a_trusted_caller() {
    let src = include_str!("../git.rs");
    let src = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
    let sites = call_sites();
    for (site, tail) in sites.iter().zip(src.split("Git::new(").skip(1)) {
        let after = tail.split_once(')').unwrap().1.trim_start();
        if !["hooks_dir:dir", "config_get:dir", "config_set:dir"].contains(&site.as_str()) {
            assert!(after.starts_with(".hardened()"), "unhardened Git at {site}");
        }
    }

    // These exceptions must keep their single caller in mirror installation.
    // Scan every production source, so adding a caller requires a new audit.
    fn callers(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_stem().is_some_and(|name| name == "tests") {
                continue;
            }
            if path.is_dir() {
                callers(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let production = text.split("#[cfg(test)]").next().unwrap();
                for line in production.lines() {
                    if ["git::hooks_dir(", "git::config_get(", "git::config_set("]
                        .iter()
                        .any(|needle| line.contains(needle))
                    {
                        found.push(format!(
                            "{}:{}",
                            path.file_name().unwrap().to_str().unwrap(),
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
    let mut found = Vec::new();
    callers(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    found.sort();
    let mut expected = vec![
        "init.rs:let hooks = git::hooks_dir(bare).await?;".to_string(),
        "init.rs:if git::config_get(bare, \"forge.mirror\").await.as_deref() != Some(mirror) {"
            .to_string(),
        "init.rs:git::config_set(bare, \"forge.mirror\", mirror).await?;".to_string(),
    ];
    expected.sort();
    assert_eq!(found, expected);
    let init = include_str!("../init.rs");
    assert_eq!(
        init.matches("install_mirror_hook(&bare, mirror)").count(),
        1
    );
    assert!(init.contains("for r in store.project_repos(&p.name)?"));
    assert!(init.contains("let repo = Path::new(&r.repo)"));
    assert!(init.contains("let Some(url) = git::remote_url(repo, &remote).await"));
    assert!(init.contains("if let Some(bare) = local_bare(&url).await"));
}

#[tokio::test]
async fn kernel_config_is_exactly_kernel_written() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let kernel = super::kernel_repository(home.path(), repo.path())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(kernel.join("config")).unwrap(),
        "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n\thooksPath = /dev/null\n"
    );
    assert!(!kernel.join("hooks").exists());
    assert_eq!(
        super::kernel_repository(home.path(), repo.path())
            .await
            .unwrap(),
        kernel
    );
}

use super::*;

#[test]
fn github_remotes_get_compare_urls() {
    let want = "https://github.com/nate/repo/compare/main...forge/7-x?expand=1";
    for url in [
        "git@github.com:nate/repo.git",
        "https://github.com/nate/repo",
        "ssh://git@github.com/nate/repo.git",
    ] {
        assert_eq!(
            compare_url(url, "main", "forge/7-x").as_deref(),
            Some(want),
            "{url}"
        );
    }
}

#[test]
fn other_remotes_get_none() {
    assert_eq!(compare_url("/srv/git/repo.git", "main", "b"), None);
    assert_eq!(
        compare_url("git@gitlab.com:nate/repo.git", "main", "b"),
        None
    );
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir.path())
        .status()
        .unwrap();
    dir
}

#[tokio::test]
async fn a_failing_git_call_reports_the_args_and_dir() {
    let dir = init_repo();
    let err = Git::new(dir.path())
        .line(&["rev-parse", "--verify", "does-not-exist"])
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.starts_with("git rev-parse --verify does-not-exist failed in "),
        "{msg}"
    );
    assert!(msg.contains(&dir.path().display().to_string()), "{msg}");
}

#[tokio::test]
async fn diff_lines_reports_content_added_and_removed_by_path() {
    let dir = init_repo();
    let wt = dir.path();
    std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let base = commit_all(wt, "base").await.unwrap().unwrap();
    std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    let tip = commit_all(wt, "edit").await.unwrap().unwrap();

    let (added, removed) = diff_lines(wt, &base, &tip).await.unwrap();
    assert_eq!(
        added,
        vec![
            ("a.txt".to_string(), "TWO".to_string()),
            ("a.txt".to_string(), "four".to_string()),
        ]
    );
    assert_eq!(removed, vec![("a.txt".to_string(), "two".to_string())]);
}

#[tokio::test]
async fn commit_path_leaves_other_dirty_files_uncommitted() {
    let dir = init_repo();
    let wt = dir.path();
    std::fs::write(wt.join("a.toml"), "a\n").unwrap();
    std::fs::write(wt.join("b.toml"), "b\n").unwrap();
    let sha = commit_path(wt, "a.toml", "add a").await.unwrap().unwrap();
    let status = Git::new(wt).line(&["status", "--porcelain"]).await.unwrap();
    assert_eq!(status, "?? b.toml");
    let log = Git::new(wt)
        .line(&["log", "--format=%s", &sha])
        .await
        .unwrap();
    assert_eq!(log, "add a");
    let files = Git::new(wt)
        .line(&["show", "--name-only", "--format=", &sha])
        .await
        .unwrap();
    assert_eq!(files, "a.toml");
}

#[tokio::test]
async fn commit_path_is_none_when_that_path_is_unchanged() {
    let dir = init_repo();
    let wt = dir.path();
    std::fs::write(wt.join("a.toml"), "a\n").unwrap();
    commit_path(wt, "a.toml", "add a").await.unwrap().unwrap();
    let again = commit_path(wt, "a.toml", "add a again").await.unwrap();
    assert_eq!(again, None);
}

#[tokio::test]
async fn dirty_tracked_paths_excludes_untracked_but_keeps_modified_and_staged() {
    let dir = init_repo();
    let wt = dir.path();
    std::fs::write(wt.join("tracked.txt"), "one\n").unwrap();
    commit_all(wt, "base").await.unwrap();
    std::fs::write(wt.join("tracked.txt"), "two\n").unwrap();
    std::fs::write(wt.join("untracked.txt"), "new\n").unwrap();
    let dirty = dirty_paths(wt).await.unwrap();
    assert_eq!(dirty, vec!["tracked.txt", "untracked.txt"]);
    let tracked_only = dirty_tracked_paths(wt).await.unwrap();
    assert_eq!(tracked_only, vec!["tracked.txt"]);
}

#[test]
fn restore_metadata_removes_commondir_redirects_and_resets_config() {
    let dir = init_repo();
    let git_dir = dir.path().join(".git");
    std::fs::write(git_dir.join("commondir"), "/tmp/evil\n").unwrap();
    std::fs::write(git_dir.join("gitdir"), "/tmp/evil/worktrees/x\n").unwrap();
    std::fs::write(
        git_dir.join("config"),
        "[filter \"x\"]\n\tclean = touch /tmp/should-not-run\n",
    )
    .unwrap();
    std::fs::write(git_dir.join("hooks").join("pre-commit"), "#!/bin/sh\n").unwrap();
    std::fs::write(git_dir.join("info").join("exclude"), "planted\n").unwrap();

    restore_metadata(dir.path()).unwrap();

    assert!(!git_dir.join("commondir").exists());
    assert!(!git_dir.join("gitdir").exists());
    let config = std::fs::read_to_string(git_dir.join("config")).unwrap();
    assert!(!config.contains("filter"), "{config}");
    assert_eq!(std::fs::read_dir(git_dir.join("hooks")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(git_dir.join("info")).unwrap().count(), 0);
}

#[test]
fn restore_metadata_refuses_a_git_that_is_not_a_plain_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/tmp", dir.path().join(".git")).unwrap();
    assert!(restore_metadata(dir.path()).is_err());
}

#[tokio::test]
async fn identity_falls_back_to_the_constant_when_unset() {
    let dir = init_repo();
    let git_dir = dir.path().join(".git");
    // Isolated per spawn, via `Command::env_remove`/`.env()`, not a
    // process-wide `std::env::set_var`: this is the same test binary
    // every other test in it runs in, and a global mutation here would
    // blind or redirect any git spawn racing it — `agent::run_codex`'s
    // own dirty/head checks among them, which read the ambient
    // environment because they carry no identity of their own.
    let g = Git::new(&git_dir)
        .without_env([
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_KEY_1",
            "GIT_CONFIG_VALUE_1",
        ])
        .with_env([
            ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
            ("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string()),
        ]);
    assert_eq!(config_or(&g, "user.name", IDENTITY.0).await, IDENTITY.0);
    assert_eq!(config_or(&g, "user.email", IDENTITY.1).await, IDENTITY.1);
}

#[tokio::test]
async fn suite_catch_up_preserves_ahead_tips_and_rejects_divergence() {
    let local = init_repo();
    let remote = init_repo();
    let url = remote.path().to_str().unwrap();
    catch_up_branch(local.path(), url, "forge-verify")
        .await
        .unwrap();
    let rg = Git::new(remote.path()).with_identity();
    rg.line(&["checkout", "-b", "forge-verify"]).await.unwrap();
    rg.line(&["commit", "--allow-empty", "-m", "suite"])
        .await
        .unwrap();
    let first = head(remote.path()).await.unwrap();
    catch_up_branch(local.path(), url, "forge-verify")
        .await
        .unwrap();
    assert_eq!(
        rev_parse(local.path(), "forge-verify").await.unwrap(),
        first
    );
    let lg = Git::new(local.path()).with_identity();
    lg.line(&["checkout", "forge-verify"]).await.unwrap();
    lg.line(&["commit", "--allow-empty", "-m", "local ahead"])
        .await
        .unwrap();
    let ahead = head(local.path()).await.unwrap();
    catch_up_branch(local.path(), url, "forge-verify")
        .await
        .unwrap();
    assert_eq!(
        rev_parse(local.path(), "forge-verify").await.unwrap(),
        ahead
    );
    rg.line(&["commit", "--allow-empty", "-m", "remote diverged"])
        .await
        .unwrap();
    let diverged = head(remote.path()).await.unwrap();
    let error = catch_up_branch(local.path(), url, "forge-verify")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&ahead) && error.contains(&diverged),
        "{error}"
    );
    assert_eq!(
        rev_parse(local.path(), "forge-verify").await.unwrap(),
        ahead
    );
    assert_eq!(head(remote.path()).await.unwrap(), diverged);
}
