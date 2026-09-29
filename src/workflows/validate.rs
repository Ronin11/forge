//! `forge workflows validate` and `forge job test`: checking a project's
//! own `.forge/workflows/` against the same parsers and resolution rules
//! the operator's catalog uses, but from a plain path with no store and no
//! `FORGE_HOME` (docs/WORKFLOWS.md, "Validating a repository's own
//! workflows"; docs/JOBS.md, "Verifying an automation").

use super::*;

/// One problem `forge workflows validate` found in a repository's own
/// workflow or action file: the file, the line the parser could place it
/// at (a syntax or shape error has one; a semantic error found only after
/// a clean parse, like a step naming an unknown action, does not), and
/// the message.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidateProblem {
    pub file: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

/// What `forge workflows validate` found under a path: how many workflow
/// and action files parsed and checked out clean, and every problem
/// otherwise.
pub struct ValidateReport {
    pub workflows: usize,
    pub actions: usize,
    pub problems: Vec<ValidateProblem>,
}

/// The 1-based line a TOML deserialize error points at, read off its span
/// (present for a syntax error or an unknown/mistyped field — exactly the
/// invented-format mistakes this command exists to catch); a `bail!` from
/// this module's own semantic checks, raised after a clean parse, carries
/// no span and gets no line.
pub(super) fn error_line(err: &anyhow::Error, text: &str) -> Option<usize> {
    let span = err
        .chain()
        .find_map(|e| e.downcast_ref::<toml::de::Error>())
        .and_then(|e| e.span())?;
    Some(text[..span.start.min(text.len())].matches('\n').count() + 1)
}

/// `forge workflows validate`: load every `.forge/workflows/*.toml` and
/// `.forge/workflows/actions/*.toml` under `root` with the same parsers
/// the operator's catalog uses (`parse_action`, `parse_workflow`), so a
/// file the catalog cannot load fails the same way here as it would at
/// `forge job start` — but from a plain path, with no store and no
/// FORGE_HOME, so it runs as a repository check on any host that has the
/// `forge` binary (docs/WORKFLOWS.md, "Validating a repository's own
/// workflows"). A run workflow's steps must each resolve to a real action
/// (the repository's own or a built-in) and use `effect` on an operation
/// step only, the same rule `job_steps` enforces for `forge job start`; a
/// build workflow here only needs to parse, since a full catalog to
/// splice it against is not available offline.
pub fn validate_repo(root: &Path) -> Result<ValidateReport> {
    let wf_dir = root.join(".forge").join("workflows");
    let actions_dir = wf_dir.join("actions");

    let mut problems = Vec::new();
    let mut actions = builtin_actions_map()?;
    let mut n_actions = 0;
    for path in toml_files_if_present(&actions_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_action(&path, &text, String::new()) {
            Ok(a) => {
                n_actions += 1;
                actions.insert(a.name.clone(), a);
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }

    // Parsed first, all of them, so a run workflow's step that splices in a
    // sibling run workflow (`job_steps`) resolves against every workflow
    // this repository declares, not just the one named on the command line.
    let mut n_workflows = 0;
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    let mut parsed: Vec<(PathBuf, String, Workflow)> = Vec::new();
    for path in toml_files_if_present(&wf_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_workflow(&path, &text, String::new()) {
            Ok(wf) => {
                workflows.insert(wf.name.clone(), wf.clone());
                parsed.push((file, text, wf));
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }
    for (file, text, wf) in &parsed {
        let result = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            Ok(())
        };
        match result {
            Ok(()) => n_workflows += 1,
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, text),
                file: file.clone(),
                message: format!("{e:#}"),
            }),
        }
    }

    Ok(ValidateReport {
        workflows: n_workflows,
        actions: n_actions,
        problems,
    })
}

/// Every run workflow under `<root>/.forge/workflows`, each resolved the
/// way `validate_repo` resolves one — against the built-in actions and the
/// tree's own `.forge/workflows/actions/*.toml`, with no home directory —
/// for `forge job test` (docs/JOBS.md, "Verifying an automation"). `only`
/// narrows it to one workflow, refused when the tree has no run workflow
/// of that name. A workflow that does not resolve is returned as its
/// error rather than dropped, so a broken automation is a failure and not
/// a silent absence; a file that does not even parse is returned only
/// when `only` names it or fixtures exist for it, since without either it
/// cannot be told from a build workflow's typo. Sorted by name.
#[allow(clippy::type_complexity)]
pub fn resolve_jobs_in_tree(
    root: &Path,
    only: Option<&str>,
) -> Result<Vec<(String, Result<(Workflow, Vec<RunStep>)>)>> {
    let wf_dir = root.join(".forge").join("workflows");
    let mut actions = builtin_actions_map()?;
    for path in toml_files_if_present(&wf_dir.join("actions"))? {
        let text = std::fs::read_to_string(&path)?;
        let a = parse_action(&path, &text, String::new())?;
        actions.insert(a.name.clone(), a);
    }
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    let mut unparsed = Vec::new();
    for path in toml_files_if_present(&wf_dir)? {
        let text = std::fs::read_to_string(&path)?;
        match parse_workflow(&path, &text, String::new()) {
            Ok(wf) => {
                workflows.insert(wf.name.clone(), wf);
            }
            Err(e) => {
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                unparsed.push((stem, e));
            }
        }
    }
    let mut out = Vec::new();
    for (name, wf) in &workflows {
        if wf.kind != WorkflowKind::Run || only.is_some_and(|o| o != name) {
            continue;
        }
        let steps = job_steps(wf, &workflows, &actions).map(|steps| (wf.clone(), steps));
        out.push((name.clone(), steps));
    }
    for (name, e) in unparsed {
        let wanted = match only {
            Some(o) => o == name,
            None => root.join(".forge").join("fixtures").join(&name).is_dir(),
        };
        if wanted {
            out.push((name, Err(e)));
        }
    }
    if let Some(o) = only
        && out.is_empty()
    {
        if workflows.contains_key(o) {
            bail!(
                "{o:?} is kind = \"build\"; `forge job test` replays kind = \"run\" workflows only"
            );
        }
        bail!(
            "no run workflow {o:?} under {} (see docs/JOBS.md, \"Where an automation lives\")",
            wf_dir.display()
        );
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_repo_accepts_a_clean_tree_with_no_store_and_no_forge_home() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();

        // Reading FORGE_HOME here would panic the test process (it does not
        // exist); `validate_repo` takes only `r`, never a home.
        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 1);
        assert_eq!(report.actions, 1);
    }

    /// `doctor-daily.toml` splicing in `disk-and-logs.toml` (docs/JOBS.md,
    /// "Steps"): a run workflow's step may name a sibling run workflow
    /// instead of an action, and `forge workflows validate` resolves it
    /// against every workflow the repository declares, not just the one
    /// named.
    #[test]
    fn validate_repo_accepts_a_run_workflow_splicing_a_sibling_run_workflow() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/outer.toml"),
            "name = \"outer\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n  { workflow = \"inner\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/inner.toml"),
            "name = \"inner\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 2);
    }

    #[test]
    fn validate_repo_accepts_an_empty_or_absent_forge_directory() {
        let repo = tempfile::tempdir().unwrap();
        let report = validate_repo(repo.path()).unwrap();
        assert!(report.problems.is_empty());
        assert_eq!(report.workflows, 0);
        assert_eq!(report.actions, 0);
    }

    /// The three invented shapes equitizr's `.forge/workflows/publish-snapshot.toml`
    /// landed twice, that a repository's own checks never validated
    /// because nothing ran the catalog's loader against it.
    #[test]
    fn validate_repo_catches_a_string_trigger() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented string trigger\"\ntrigger = \"manual\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.file,
            PathBuf::from(".forge/workflows/publish-snapshot.toml")
        );
        assert_eq!(p.line, Some(4), "{}", p.message);
        assert!(p.message.contains("invalid type"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_a_steps_table_with_an_inline_run_command() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented [[steps]] table with an inline run command\"\n\n[[steps]]\nrun = \"echo hi\"\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(p.message.contains("unknown field `run`"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_an_http_get_step_with_url_and_field() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented http_get step with url and field\"\n\nsteps = [\n  { action = \"http_get\", url = \"https://example.com\", field = \"x\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(
            p.message.contains("unknown field `url`")
                || p.message.contains("unknown field `field`"),
            "{}",
            p.message
        );
    }

    #[test]
    fn validate_repo_catches_a_step_referencing_an_unknown_action() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"references an action nothing declares\"\n\nsteps = [\n  { action = \"does-not-exist\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.line, None,
            "a semantic check after a clean parse has no span"
        );
        assert!(p.message.contains("does-not-exist"), "{}", p.message);
    }
}
