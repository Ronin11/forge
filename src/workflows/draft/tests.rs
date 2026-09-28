use super::*;
use std::collections::BTreeSet;

fn known(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn placeholder(inputs: &str, outputs: &str) -> Placeholder {
    Placeholder {
        kind: Kind::Operation,
        inputs: inputs.into(),
        outputs: outputs.into(),
    }
}

fn step(action: &str, ph: Option<Placeholder>) -> DraftStep {
    DraftStep {
        placeholder: ph,
        ..DraftStep::named(action)
    }
}

fn two_step_draft() -> Draft {
    let mut d = Draft::new("docs-lint", WorkflowKind::Build);
    d.description = "lint the docs".into();
    d.steps = vec![
        step("code", None),
        step("lint-docs", Some(placeholder("the tree", "a verdict"))),
    ];
    d
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn a_step_naming_a_missing_action_needs_a_contract_to_be_a_placeholder() {
    let known = known(&["code"]);
    let mut d = Draft::new("w", WorkflowKind::Build);
    d.steps = vec![step("code", None), step("lint-docs", None)];
    let problems = d.placeholder_problems(&known);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].step, Some(1));
    assert!(
        problems[0].message.contains("one-line contract"),
        "{problems:?}"
    );

    d.steps[1].placeholder = Some(placeholder("the tree", "a verdict"));
    assert!(d.placeholder_problems(&known).is_empty());
}

#[test]
fn a_placeholder_needs_inputs_and_outputs_and_a_valid_name() {
    let known = known(&["code"]);
    let mut d = Draft::new("w", WorkflowKind::Build);
    d.steps = vec![step("lint-docs", Some(placeholder("the tree", "  ")))];
    assert_eq!(d.placeholder_problems(&known).len(), 1);
    d.steps = vec![step("Lint Docs", Some(placeholder("a", "b")))];
    assert_eq!(d.placeholder_problems(&known).len(), 1);
}

#[test]
fn two_steps_may_share_a_placeholder_only_with_one_contract() {
    let known = known(&[]);
    let mut d = Draft::new("w", WorkflowKind::Build);
    d.steps = vec![
        step("x", Some(placeholder("a", "b"))),
        step("x", Some(placeholder("a", "b"))),
    ];
    assert!(d.placeholder_problems(&known).is_empty());
    assert_eq!(d.pending(&known).len(), 1, "one task builds it");
    d.steps[1].placeholder = Some(placeholder("a", "c"));
    assert_eq!(d.placeholder_problems(&known).len(), 1);
}

#[test]
fn a_placeholder_whose_action_exists_is_landed_not_pending() {
    let mut d = Draft::new("w", WorkflowKind::Build);
    d.steps = vec![step("x", Some(placeholder("a", "b")))];
    assert_eq!(d.pending(&known(&[])).len(), 1);
    assert!(d.pending(&known(&["x"])).is_empty());
    assert!(d.placeholder_problems(&known(&["x"])).is_empty());
}

#[test]
fn an_incomplete_draft_enables_itself_only_when_complete_and_clean() {
    use Status::*;
    assert_eq!(
        transition(Incomplete, 1, true),
        Incomplete,
        "an action is still missing"
    );
    assert_eq!(
        transition(Incomplete, 0, false),
        Incomplete,
        "the lint does not pass"
    );
    assert_eq!(transition(Incomplete, 0, true), Enabled);
    assert_eq!(
        transition(Draft, 0, true),
        Draft,
        "only an incomplete draft moves"
    );
    assert_eq!(transition(Enabled, 3, false), Enabled);
    assert_eq!(saved_status(Draft, 1), Incomplete);
    assert_eq!(saved_status(Draft, 0), Draft);
    assert_eq!(saved_status(Enabled, 1), Enabled);
}

#[test]
fn a_draft_renders_to_a_file_the_catalog_parses_and_reads_back() {
    let d = two_step_draft();
    let (text, lines) = d.render().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(
        text.lines()
            .nth(lines[1] - 1)
            .unwrap()
            .contains("lint-docs"),
        "{text}"
    );
    let back = Draft::from_text(&text).unwrap();
    assert_eq!(back.name, "docs-lint");
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.steps[1].action, "lint-docs");
}

#[test]
fn a_draft_with_a_placeholder_lints_clean_and_reports_it_pending() {
    let h = home();
    let a = two_step_draft().check(h.path()).unwrap();
    assert!(a.clean, "{:?}", a.problems);
    assert_eq!(a.pending, vec!["lint-docs".to_string()]);
    assert!(a.info[1].placeholder && !a.info[1].landed);
    assert_eq!(
        a.info[0].kind,
        Some(Kind::Directive),
        "an existing step shows its action's contract"
    );
}

#[test]
fn a_problem_in_the_rest_of_the_flow_is_annotated_on_its_step() {
    let h = home();
    let mut d = two_step_draft();
    d.steps.push(step("no-such-action", None));
    let a = d.check(h.path()).unwrap();
    assert!(!a.clean);
    assert_eq!(
        a.problems.iter().filter(|p| p.step == Some(2)).count(),
        1,
        "{:?}",
        a.problems
    );
}

#[test]
fn a_run_draft_carries_edges_and_a_trigger() {
    let h = home();
    let mut d = Draft::new("nightly", WorkflowKind::Run);
    d.steps = vec![DraftStep {
        judgment: Some("deciding what a script cannot".into()),
        role: Some("summarise".into()),
        on: [("failure".to_string(), "end".to_string())].into(),
        ..step(
            "summarise-x",
            Some(Placeholder {
                kind: Kind::Directive,
                inputs: "a diff".into(),
                outputs: "a line".into(),
            }),
        )
    }];
    let a = d.check(h.path()).unwrap();
    assert!(a.clean, "{:?}\n{}", a.problems, a.toml);
    assert!(a.toml.contains("on = { failure = \"end\" }"), "{}", a.toml);
    assert!(a.toml.contains("[trigger]"), "{}", a.toml);
}

#[test]
fn drafts_save_load_list_and_refuse_a_path_like_name() {
    let h = home();
    let mut d = two_step_draft();
    d.status = Status::Incomplete;
    save(h.path(), &d).unwrap();
    assert_eq!(load(h.path(), "docs-lint").unwrap(), Some(d.clone()));
    assert_eq!(list(h.path()).unwrap().len(), 1);
    assert!(load(h.path(), "../x").is_err());
    remove(h.path(), "docs-lint").unwrap();
    assert_eq!(load(h.path(), "docs-lint").unwrap(), None);
}

#[tokio::test]
async fn reconcile_enables_an_incomplete_draft_once_its_action_lands() {
    let h = home();
    let mut d = two_step_draft();
    d.status = Status::Incomplete;
    save(h.path(), &d).unwrap();

    assert!(
        reconcile(h.path()).await.unwrap().is_empty(),
        "the action is not there yet"
    );
    assert_eq!(
        load(h.path(), "docs-lint").unwrap().unwrap().status,
        Status::Incomplete
    );

    let dir = super::super::catalog_dir(h.path()).unwrap();
    std::fs::create_dir_all(dir.join("actions")).unwrap();
    std::fs::write(
        dir.join("actions/lint-docs.toml"),
        "name = \"lint-docs\"\nkind = \"operation\"\nrun = [\"true\"]\n",
    )
    .unwrap();

    assert_eq!(
        reconcile(h.path()).await.unwrap(),
        vec!["docs-lint".to_string()]
    );
    assert_eq!(
        load(h.path(), "docs-lint").unwrap().unwrap().status,
        Status::Enabled
    );
    assert!(
        dir.join("docs-lint.toml").exists(),
        "enabled means in the catalog"
    );
    assert!(
        reconcile(h.path()).await.unwrap().is_empty(),
        "and it happens once"
    );
}
