//! Resolving a workflow to the exact versions a task or job will run: a
//! build workflow's steps spliced and checked against the data-flow rule
//! ([`resolve`]), a run workflow's steps resolved against its own vocabulary
//! ([`job_steps`], shared with `project` and `validate`), and the catalog-wide
//! `forge workflows check`.

use super::*;

pub(super) fn splice(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Resolved,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    let pin = Pin {
        kind: "workflow".into(),
        name: wf.name.clone(),
        hash: wf.hash.clone(),
    };
    if !out.pins.contains(&pin) {
        out.pins.push(pin);
    }
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown workflow {:?}",
                    wf.name, name
                )
            })?;
            splice(child, workflows, actions, path, out)?;
        } else if let Some(name) = s.action.as_deref() {
            let a = actions.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown action {:?}",
                    wf.name, name
                )
            })?;

            let pin = Pin {
                kind: "action".into(),
                name: a.name.clone(),
                hash: a.hash.clone(),
            };
            if !out.pins.contains(&pin) {
                out.pins.push(pin);
            }
            out.steps.push(ResolvedStep {
                action: a.clone(),
                model: s.model.clone().or_else(|| a.model.clone()),
                max_turns: s.max_turns.or(a.max_turns),
                timeout_secs: s.timeout_secs.or(a.timeout_secs),
                via: path.clone(),
                node: edges::node_id(out.steps.len(), &a.name),
            });
        }
    }
    path.pop();
    Ok(())
}

/// The data-flow rule: every step's `consumes` must already be produced.
/// Kernel verify after each directive produces `verdict`; the clone
/// produces `branch`.
pub(super) fn check_flow(steps: &[ResolvedStep]) -> Result<()> {
    let mut have: BTreeSet<Product> = [Product::Branch].into_iter().collect();
    for (i, s) in steps.iter().enumerate() {
        for c in &s.action.consumes {
            if !have.contains(c) {
                bail!(
                    "step {} ({}) consumes {:?}, which nothing before it produces (have: {})",
                    i + 1,
                    s.action.name,
                    c.as_str(),
                    have.iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for p in &s.action.produces {
            have.insert(*p);
        }
        if s.action.kind == Kind::Directive || s.action.mutates() {
            have.insert(Product::Verdict);
            if s.action.contract == Contract::Review {
                have.insert(Product::Review);
            }
        }
    }
    if !steps.iter().any(|s| s.action.kind == Kind::Directive) {
        bail!("a workflow needs at least one directive; operations alone produce nothing to push");
    }
    for (i, s) in steps.iter().enumerate() {
        if s.action.kind == Kind::Operation
            && s.action.verifies
            && !steps[..i].iter().any(|p| p.action.kind == Kind::Directive)
        {
            bail!(
                "step {} ({}) verifies the preceding directive, but no directive precedes it",
                i + 1,
                s.action.name
            );
        }
    }
    Ok(())
}

/// Resolve a workflow by name into the exact versions a task will run,
/// splicing referenced workflows inline. Fails on unknown references,
/// cycles, and data-flow violations.
pub fn resolve(home: &Path, name: &str) -> Result<Resolved> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?;
    let mut out = Resolved::default();
    splice(wf, &cat.workflows, &cat.actions, &mut Vec::new(), &mut out)?;
    check_flow(&out.steps)?;
    Ok(out)
}

/// A job step after resolution: the action it names, plus its own `role`
/// (a directive, routed to a provider) and any per-step overrides
/// (docs/JOBS.md, "Steps"). An operation step's `effect` is validated here
/// (see `job_steps`) but carries no further meaning to the executor: what
/// an operation actually did is read back from the effect log it writes,
/// not from what it declared.
#[derive(Clone, Debug)]
pub struct RunStep {
    pub action: ActionDef,
    pub role: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    /// The effect an operation step declares (docs/JOBS.md, "Effects").
    pub effect: Option<EffectKind>,
    /// `<index>-<action>`, stable across the run.
    pub node: String,
    /// Edges, resolved to node ids or `end`.
    pub on: BTreeMap<String, String>,
    /// How many times a loop may enter this step.
    pub max_attempts: u32,
}

/// A job step, resolved to the action it names (docs/JOBS.md, "Steps"). A
/// step may instead name a sibling `kind = "run"` workflow (`workflow =
/// "…"`, the same field a build workflow splices in); its steps are
/// inlined here, recursively, the way a build workflow's own splice works,
/// so a daily automation can be assembled from smaller run workflows
/// (docs/JOBS.md, "Steps") — `doctor-daily` splicing in `disk-and-logs` is
/// the motivating case. A cycle, or a reference to a workflow that is not
/// itself `kind = "run"`, is refused. A directive step must name a `role`
/// (routed to a provider like every role) and its action must declare a
/// `schema`; an operation step must not name a `role`, and a directive
/// step must not name an `effect`.
pub(super) fn job_steps(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
) -> Result<Vec<RunStep>> {
    let mut out = Vec::new();
    job_steps_into(wf, workflows, actions, &mut Vec::new(), &mut out)?;
    edges::resolve(&wf.name, &mut out)?;
    Ok(out)
}

fn job_steps_into(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Vec<RunStep>,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "run workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!("{:?}: job step names unknown workflow {name:?}", wf.name)
            })?;
            if child.kind != WorkflowKind::Run {
                bail!(
                    "{:?}: job step names workflow {name:?}, which is kind = \"build\"; a run workflow may only splice in another run workflow",
                    wf.name
                );
            }
            job_steps_into(child, workflows, actions, path, out)?;
            continue;
        }
        let name = s.action.as_deref().with_context(|| {
            format!(
                "{:?}: a job step names exactly one of `action` or `workflow`",
                wf.name
            )
        })?;
        let action = actions
            .get(name)
            .cloned()
            .with_context(|| format!("{:?}: job step names unknown action {name:?}", wf.name))?;
        match action.kind {
            Kind::Directive => {
                if s.role.as_deref().is_none_or(|r| r.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; it needs `role` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if action.schema.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; its action {name:?} needs a `schema` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if s.effect.is_some() {
                    bail!(
                        "{:?}: job step {name:?} is a directive; `effect` applies to operation steps only",
                        wf.name
                    );
                }
                if s.judgment.as_deref().is_none_or(|j| j.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive and carries no `judgment`; every directive step in a run workflow says what a script cannot do here: judgment = \"<one sentence>\" (docs/EXECUTION.md, rule 4: \"an operation unless judgment is genuinely needed\")",
                        wf.name
                    );
                }
            }
            Kind::Operation if s.judgment.is_some() => {
                bail!(
                    "{:?}: job step {name:?} is an operation; `judgment` applies to directive steps only",
                    wf.name
                );
            }
            Kind::Operation if s.role.is_some() => {
                bail!(
                    "{:?}: job step {name:?} is an operation; `role` applies to directive steps only",
                    wf.name
                );
            }
            Kind::Operation => {}
        }
        out.push(RunStep {
            model: s.model.clone().or_else(|| action.model.clone()),
            max_turns: s.max_turns.or(action.max_turns),
            timeout_secs: s.timeout_secs.or(action.timeout_secs),
            role: s.role.clone(),
            effect: s.effect,
            node: String::new(),
            on: s.on.clone(),
            max_attempts: s.max_attempts.unwrap_or(edges::DEFAULT_MAX_ATTEMPTS),
            action,
        });
    }
    path.pop();
    Ok(())
}

/// A run workflow by name, resolved to the exact action each of its steps
/// runs (docs/JOBS.md, "The executor"). Unlike `resolve`, there is none of
/// `check_flow`'s build-only data-flow rules: a job step's action is used
/// as written, though a step may splice in a sibling run workflow (see
/// `job_steps`). Fails on an unknown workflow, a workflow that is not
/// `kind = "run"`, an unknown action, or a step that names a workflow this
/// catalog does not have.
pub fn resolve_job(home: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?
        .clone();
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job start` runs kind = \"run\" workflows only");
    }
    let steps = job_steps(&wf, &cat.workflows, &cat.actions)?;
    Ok((wf, steps))
}

/// One thing wrong with a file, and whether it blocks use.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub blocking: bool,
    pub what: String,
}

/// Every file, checked structurally; every workflow, resolved. Parse
/// errors are problems rather than errors, so one bad file does not hide
/// the rest. Deterministic: same files, same list.
pub fn check(home: &Path) -> Result<Vec<Problem>> {
    let Catalog {
        workflows,
        actions,
        mut problems,
    } = load_catalog(home)?;
    for (name, a) in &actions {
        if a.kind == Kind::Operation && KERNEL_OPS.contains(&name.as_str()) {
            problems.push(Problem {
                file: format!("actions/{name}.toml"),
                blocking: true,
                what: format!("operation {name:?} shadows a kernel operation"),
            });
        }
    }
    for (name, wf) in &workflows {
        // A run workflow does not follow the build data-flow rules
        // (`check_flow` requires a directive, which an operation-only job
        // never has); it only needs its steps' actions (and any spliced-in
        // sibling run workflow's) to exist (see `job_steps`).
        let r = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            let mut out = Resolved::default();
            splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)
                .and_then(|_| check_flow(&out.steps))
        };
        if let Err(e) = r {
            problems.push(Problem {
                file: format!("{name}.toml"),
                blocking: true,
                what: format!("{e:#}"),
            });
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn inline_composition_splices_and_rejects_cycles() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "outer.toml",
            "name = \"outer\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { workflow = \"tdd\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
        );
        let r = resolve(dir.path(), "outer").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "tests", "setup", "repo-map", "code"]
        );
        assert_eq!(r.steps[1].via, vec!["outer", "tdd"]);
        assert!(
            r.pins
                .iter()
                .any(|p| p.kind == "workflow" && p.name == "tdd")
        );
        write(
            dir.path(),
            "a.toml",
            "name = \"a\"\nsteps = [{ workflow = \"b\" }]\n",
        );
        write(
            dir.path(),
            "b.toml",
            "name = \"b\"\nsteps = [{ workflow = \"a\" }]\n",
        );
        let err = resolve(dir.path(), "a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("references itself")),
            "{problems:?}"
        );
    }

    #[test]
    fn operations_produce_only_branch_or_interface_and_a_mutating_one_yields_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        for (name, body, want) in [
            (
                "verdicting",
                "produces = [\"verdict\"]\nrun = [\"true\"]\n",
                "cannot produce \"verdict\"",
            ),
            (
                "modelled",
                "model = \"haiku\"\nrun = [\"true\"]\n",
                "`model` applies to directives only",
            ),
        ] {
            write(
                dir.path(),
                &format!("actions/{name}.toml"),
                &format!(
                    "name = \"{name}\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n{body}"
                ),
            );
            let err = load_actions(dir.path()).unwrap_err().to_string();
            assert!(err.contains(want), "{name}: {err}");
            std::fs::remove_file(
                dir.path()
                    .join("workflows/actions")
                    .join(format!("{name}.toml")),
            )
            .unwrap();
        }
        // The built-in fmt mutates, so polish (which consumes a verdict)
        // may follow it directly; the built-in interface reads the verify
        // ref and so needs the tests directive first.
        let fmt = load_actions(dir.path()).unwrap().remove("fmt").unwrap();
        assert!(fmt.mutates() && !fmt.yields_interface() && !fmt.reads_verify_ref());
        write(
            dir.path(),
            "fmt-polish.toml",
            "name = \"fmt-polish\"\nsteps = [{ action = \"fmt\" }, { action = \"polish\" }]\n",
        );
        assert!(resolve(dir.path(), "fmt-polish").is_ok());
        write(
            dir.path(),
            "iface-early.toml",
            "name = \"iface-early\"\nsteps = [{ action = \"interface\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "iface-early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "iface.toml",
            "name = \"iface\"\nsteps = [{ action = \"tests\" }, { action = \"interface\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "iface").unwrap();
        assert!(r.steps[1].action.reads_verify_ref() && r.steps[1].action.yields_interface());
    }

    #[test]
    fn data_flow_and_kernel_shadowing_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/needs-ref.toml",
            "name = \"needs-ref\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"verify_ref\"]\nrun = [\"true\"]\n",
        );
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "late.toml",
            "name = \"late\"\nsteps = [{ action = \"tests\" }, { action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        assert!(resolve(dir.path(), "late").is_ok());
        write(
            dir.path(),
            "opsonly.toml",
            "name = \"opsonly\"\nsteps = [{ action = \"setup\" }]\n",
        );
        assert!(
            resolve(dir.path(), "opsonly")
                .unwrap_err()
                .to_string()
                .contains("at least one directive")
        );
        write(
            dir.path(),
            "actions/verify.toml",
            "name = \"verify\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("shadows a kernel operation")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "actions/both.toml",
            "name = \"both\"\nkind = \"operation\"\nrun = [\"true\"]\ncheck = \"test\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.what.contains("exactly one of")),
            "{problems:?}"
        );
    }

    #[test]
    fn typos_are_blocking_and_overrides_have_a_precedence() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/typo.toml",
            "name = \"typo\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\ntimeout_sec = 5\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.blocking
                && p.file == "actions/typo.toml"
                && p.what.contains("unknown field")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "wtypo.toml",
            "name = \"wtypo\"\nsteps = [{ action = \"code\", max_turn = 3 }]\n[meta]\nuse_wen = \"x\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.file == "wtypo.toml"),
            "{problems:?}"
        );
        std::fs::remove_file(dir.path().join("workflows/actions/typo.toml")).unwrap();
        std::fs::remove_file(dir.path().join("workflows/wtypo.toml")).unwrap();
        // step override > action default > task default (None here)
        write(
            dir.path(),
            "over.toml",
            "name = \"over\"\nsteps = [{ action = \"tests\", max_turns = 9, model = \"haiku\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "over").unwrap();
        assert_eq!(r.steps[0].max_turns, Some(9));
        assert_eq!(r.steps[0].model.as_deref(), Some("haiku"));
        assert_eq!(r.steps[1].max_turns, None, "the task's own limit applies");
        // diamond: two paths to the same child splice twice, pin once
        write(
            dir.path(),
            "left.toml",
            "name = \"left\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "right.toml",
            "name = \"right\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "diamond.toml",
            "name = \"diamond\"\nsteps = [{ workflow = \"left\" }, { workflow = \"right\" }]\n",
        );
        let r = resolve(dir.path(), "diamond").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "repo-map", "code", "setup", "repo-map", "code"]
        );
        assert_eq!(r.pins.iter().filter(|p| p.name == "direct").count(), 1);
        assert_eq!(r.pins.iter().filter(|p| p.name == "code").count(), 1);
    }

    #[test]
    fn verifying_operations_need_a_preceding_directive() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let a = load_actions(dir.path()).unwrap();
        assert!(a["playwright"].overlay && a["playwright"].verifies);
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"playwright\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("no directive precedes it"), "{err}");
        write(
            dir.path(),
            "actions/badd.toml",
            "name = \"badd\"\nkind = \"directive\"\ncontract = \"code\"\noverlay = true\n",
        );
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.what.contains("operations only"))
        );
    }

    /// A run workflow's step may name a sibling run workflow instead of an
    /// action, spliced inline recursively (`job_steps`) — the composition
    /// `doctor-daily.toml` uses to pull in `disk-and-logs.toml`. Splicing a
    /// `kind = "build"` workflow, or a cycle of run workflows, is refused.
    #[test]
    fn resolve_job_splices_a_sibling_run_workflow_and_rejects_a_build_kind_child_or_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "inner-run.toml",
            "name = \"inner-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "outer-run.toml",
            "name = \"outer-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }, { workflow = \"inner-run\" }]\n[trigger]\non = \"manual\"\n",
        );
        let (wf, steps) = resolve_job(dir.path(), "outer-run").unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        assert_eq!(
            steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fmt", "fmt"]
        );

        write(
            dir.path(),
            "build-child.toml",
            "name = \"build-child\"\ndescription = \"d\"\nsteps = [{ action = \"code\" }]\n",
        );
        write(
            dir.path(),
            "bad-splice.toml",
            "name = \"bad-splice\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"build-child\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "bad-splice")
            .unwrap_err()
            .to_string();
        assert!(err.contains("kind = \"build\""), "{err}");

        write(
            dir.path(),
            "cycle-a.toml",
            "name = \"cycle-a\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-b\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "cycle-b.toml",
            "name = \"cycle-b\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-a\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "cycle-a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
    }
}
