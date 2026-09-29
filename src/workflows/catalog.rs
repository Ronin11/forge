//! The operator's catalog: the built-in actions, operations and workflows
//! this binary carries, the `<home>/workflows` directory they seed, and
//! loading every file in it into a [`Catalog`] (docs/WORKFLOWS.md).

use super::*;

pub(super) const BUILTIN_ACTIONS: &[(&str, &str)] = &[
    ("code.toml", include_str!("../builtins/actions/code.toml")),
    ("tests.toml", include_str!("../builtins/actions/tests.toml")),
    (
        "investigate.toml",
        include_str!("../builtins/actions/investigate.toml"),
    ),
    (
        "review.toml",
        include_str!("../builtins/actions/review.toml"),
    ),
    ("docs.toml", include_str!("../builtins/actions/docs.toml")),
    ("fix.toml", include_str!("../builtins/actions/fix.toml")),
    (
        "polish.toml",
        include_str!("../builtins/actions/polish.toml"),
    ),
    (
        "document.toml",
        include_str!("../builtins/actions/document.toml"),
    ),
    ("graph.toml", include_str!("../builtins/actions/graph.toml")),
    (
        "playwright.toml",
        include_str!("../builtins/actions/playwright.toml"),
    ),
    ("setup.toml", include_str!("../builtins/actions/setup.toml")),
    (
        "interview.toml",
        include_str!("../builtins/actions/interview.toml"),
    ),
    (
        "assess.toml",
        include_str!("../builtins/actions/assess.toml"),
    ),
    (
        "deploy-look.toml",
        include_str!("../builtins/actions/deploy-look.toml"),
    ),
    (
        "concierge.toml",
        include_str!("../builtins/actions/concierge.toml"),
    ),
    ("chat.toml", include_str!("../builtins/actions/chat.toml")),
];

pub(super) const BUILTIN_OPERATIONS: &[(&str, &str)] = &[
    (
        "comments-only.toml",
        include_str!("../builtins/operations/comments-only.toml"),
    ),
    (
        "graph-check.toml",
        include_str!("../builtins/operations/graph-check.toml"),
    ),
    (
        "repo-map.toml",
        include_str!("../builtins/operations/repo-map.toml"),
    ),
    (
        "repo-graph.toml",
        include_str!("../builtins/operations/repo-graph.toml"),
    ),
    (
        "diff-size.toml",
        include_str!("../builtins/operations/diff-size.toml"),
    ),
    ("fmt.toml", include_str!("../builtins/operations/fmt.toml")),
    (
        "interface.toml",
        include_str!("../builtins/operations/interface.toml"),
    ),
    (
        "deploy-command.toml",
        include_str!("../builtins/operations/deploy-command.toml"),
    ),
    (
        "deploy-user-service.toml",
        include_str!("../builtins/operations/deploy-user-service.toml"),
    ),
    (
        "deploy-self.toml",
        include_str!("../builtins/operations/deploy-self.toml"),
    ),
    (
        "deploy-static.toml",
        include_str!("../builtins/operations/deploy-static.toml"),
    ),
    (
        "deploy-smoke.toml",
        include_str!("../builtins/operations/deploy-smoke.toml"),
    ),
    (
        "provision-hetzner.toml",
        include_str!("../builtins/operations/provision-hetzner.toml"),
    ),
    (
        "write-file.toml",
        include_str!("../builtins/operations/write-file.toml"),
    ),
    (
        "append-row.toml",
        include_str!("../builtins/operations/append-row.toml"),
    ),
    (
        "http-post.toml",
        include_str!("../builtins/operations/http-post.toml"),
    ),
    (
        "egress-probe.toml",
        include_str!("../builtins/operations/egress-probe.toml"),
    ),
    (
        "send-signal.toml",
        include_str!("../builtins/operations/send-signal.toml"),
    ),
    (
        "send-sms.toml",
        include_str!("../builtins/operations/send-sms.toml"),
    ),
];

pub(crate) const BUILTIN_WORKFLOWS: &[(&str, &str)] = &[
    (
        "planned.toml",
        include_str!("../builtins/workflows/planned.toml"),
    ),
    (
        "direct.toml",
        include_str!("../builtins/workflows/direct.toml"),
    ),
    ("tdd.toml", include_str!("../builtins/workflows/tdd.toml")),
    ("docs.toml", include_str!("../builtins/workflows/docs.toml")),
    (
        "cheap.toml",
        include_str!("../builtins/workflows/cheap.toml"),
    ),
    (
        "polish.toml",
        include_str!("../builtins/workflows/polish.toml"),
    ),
    (
        "reviewed.toml",
        include_str!("../builtins/workflows/reviewed.toml"),
    ),
    (
        "playable.toml",
        include_str!("../builtins/workflows/playable.toml"),
    ),
    (
        "documented.toml",
        include_str!("../builtins/workflows/documented.toml"),
    ),
    (
        "mapped.toml",
        include_str!("../builtins/workflows/mapped.toml"),
    ),
    (
        "tdd-reviewed.toml",
        include_str!("../builtins/workflows/tdd-reviewed.toml"),
    ),
    (
        "intake.toml",
        include_str!("../builtins/workflows/intake.toml"),
    ),
    (
        "concierge.toml",
        include_str!("../builtins/workflows/concierge.toml"),
    ),
];

pub(super) fn dir_of(home: &Path) -> PathBuf {
    home.join("workflows")
}

/// The directory exists, is a git repository, and holds the built-ins if
/// it holds nothing at all.
fn ensure(home: &Path) -> Result<PathBuf> {
    let dir = dir_of(home);
    let actions = dir.join("actions");
    std::fs::create_dir_all(&actions)?;
    if !dir.join(".git").exists() {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .output()?;
        if !o.status.success() {
            bail!(
                "git init in {} failed: {}",
                dir.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    // Built-in workflows are written when missing and never overwritten.
    // Built-in actions and operations are never written: the catalog holds
    // only the ones the operator authored (see `shadow`).
    for (file, text) in BUILTIN_WORKFLOWS {
        let p = dir.join(file);
        if !p.exists() {
            std::fs::write(&p, text)?;
        }
    }
    let fragments = dir.join(FRAGMENTS_DIR);
    std::fs::create_dir_all(&fragments)?;
    let untrusted = fragments.join("untrusted-data.md");
    if !untrusted.exists() {
        std::fs::write(&untrusted, UNTRUSTED_DATA)?;
    }
    Ok(dir)
}

/// The operator's catalog directory (`<home>/workflows`), created, made a
/// git repository, and seeded with built-ins if it doesn't hold them yet
/// — the same directory `load_catalog` reads. `forge workflows put`
/// writes a candidate file directly into this path.
pub fn catalog_dir(home: &Path) -> Result<PathBuf> {
    ensure(home)
}

/// The `name` a candidate workflow's TOML declares, if it parses far
/// enough to have one — the same fallback `lint` uses when `forge
/// workflows lint --stdin` is given no `--name`, reused by `forge
/// workflows put` to refuse a NAME that doesn't match the file's own.
pub fn declared_name(text: &str) -> Option<String> {
    toml::from_str::<toml::Value>(text)
        .ok()
        .and_then(|v| v.get("name")?.as_str().map(str::to_string))
}

/// The git blob hash of a file: the identity git gives this version.
pub(super) fn blob_hash(dir: &Path, path: &Path) -> Result<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("hash-object")
        .arg(path)
        .output()?;
    if !o.status.success() {
        bail!(
            "git hash-object {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub(super) fn toml_files(d: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        // `experiment.toml` (piece 4, docs/ECONOMIST.md) is a reserved
        // name: it lives beside the workflow files in the same catalog
        // directory but is never one itself, so it is never offered to
        // the workflow parser (which would otherwise flag it as a
        // broken workflow and block every task, `check`'s `blocking`).
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some("experiment.toml"))
        .collect();
    v.sort();
    Ok(v)
}

pub(super) fn toml_files_if_present(d: &Path) -> Result<Vec<PathBuf>> {
    if !d.exists() {
        return Ok(Vec::new());
    }
    toml_files(d)
}

/// Every built-in action and operation, parsed once with no disk access:
/// the reference set a repository's own workflow steps may resolve
/// against alongside its own `.forge/workflows/actions/*.toml` (docs/JOBS.md,
/// "Where an automation lives"), without `ensure`'s side effect of
/// writing them into a home directory.
pub(super) fn builtin_actions_map() -> Result<BTreeMap<String, ActionDef>> {
    let mut out = BTreeMap::new();
    for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let a = parse_action(Path::new(file), text, String::new())
            .with_context(|| format!("built-in {file}"))?;
        out.insert(a.name.clone(), a);
    }
    Ok(out)
}

/// Every action and every workflow, loaded once: one read of each
/// directory, one git blob hash per file. A file that fails to parse
/// contributes a problem instead of aborting the load, so one bad file
/// does not hide the rest. `check` adds the problems that need every
/// action and workflow already loaded (kernel shadowing, and resolving
/// every workflow) on top of this.
pub struct Catalog {
    pub workflows: BTreeMap<String, Workflow>,
    pub actions: BTreeMap<String, ActionDef>,
    pub problems: Vec<Problem>,
}

fn load_dir<T>(
    files: Vec<PathBuf>,
    file_of: impl Fn(&Path) -> String,
    mut parse: impl FnMut(&Path, &str) -> Result<T>,
) -> Result<(Vec<T>, Vec<Problem>)> {
    let mut items = Vec::new();
    let mut problems = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        match parse(&path, &text) {
            Ok(item) => items.push(item),
            Err(e) => problems.push(Problem {
                file: file_of(&path),
                blocking: true,
                what: format!("{e:#}"),
            }),
        }
    }
    Ok((items, problems))
}

pub fn load_catalog(home: &Path) -> Result<Catalog> {
    let dir = ensure(home)?;
    let stale = shadow::stale_seeds(&dir);
    let (raw_actions, mut problems) = load_dir(
        toml_files(&dir.join("actions"))?
            .into_iter()
            .filter(|p| !stale.contains(p.file_name().unwrap().to_string_lossy().as_ref()))
            .collect(),
        |p| format!("actions/{}", p.file_name().unwrap().to_string_lossy()),
        |p, t| {
            let mut a = parse_action(p, t, blob_hash(&dir, p)?)?;
            load_prompt_file(&dir, p, &mut a)?;
            Ok(a)
        },
    )?;
    problems.extend(fragment_problems(&dir));
    let mut actions = BTreeMap::new();
    for a in raw_actions {
        if a.description.trim().is_empty() {
            problems.push(Problem {
                file: format!("actions/{}.toml", a.name),
                blocking: false,
                what: "no description".into(),
            });
        }
        actions.insert(a.name.clone(), a);
    }
    // A built-in the catalog does not override (or only shadows with a
    // stale seed) applies as it is in this binary.
    for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let name = file.trim_end_matches(".toml");
        if !actions.contains_key(name) {
            let a = parse_action(Path::new(file), text, shadow::text_blob_hash(text)?)
                .with_context(|| format!("built-in {file}"))?;
            actions.insert(a.name.clone(), a);
        }
    }

    let (raw_workflows, wf_problems) = load_dir(
        toml_files(&dir)?,
        |p| p.file_name().unwrap().to_string_lossy().into_owned(),
        |p, t| parse_workflow(p, t, blob_hash(&dir, p)?),
    )?;
    problems.extend(wf_problems);
    let mut workflows = BTreeMap::new();
    for w in raw_workflows {
        let file = format!("{}.toml", w.name);
        if w.meta.cost_factor.is_some() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "[meta] cost_factor is ignored: costs are measured from runs, never declared; remove it".into(),
            });
        }
        if w.description.trim().is_empty() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "no description".into(),
            });
        }
        if w.meta.use_when.trim().is_empty() || w.meta.avoid_when.trim().is_empty() {
            problems.push(Problem {
                file,
                blocking: false,
                what: "[meta] use_when and avoid_when are empty; a chooser has nothing to read"
                    .into(),
            });
        }
        workflows.insert(w.name.clone(), w);
    }

    Ok(Catalog {
        workflows,
        actions,
        problems,
    })
}

/// Every problem in a catalog that means a file failed to load at all
/// (as opposed to a convention `check` alone enforces, like kernel
/// shadowing): the same gate `load_all`, `load_actions`, `resolve`, and
/// `get` used to get for free from `?` on a per-file parse.
pub(super) fn ensure_sound(cat: &Catalog) -> Result<()> {
    if let Some(p) = cat.problems.iter().find(|p| p.blocking) {
        bail!("{}", p.what);
    }
    Ok(())
}

/// Every action file, by name.
pub fn load_actions(home: &Path) -> Result<BTreeMap<String, ActionDef>> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.actions)
}

/// The built-in operation or action `name` as this binary carries it,
/// whatever the catalog holds.
pub(crate) fn builtin_action(name: &str) -> Result<ActionDef> {
    let file = format!("{name}.toml");
    let (_, text) = BUILTIN_ACTIONS
        .iter()
        .chain(BUILTIN_OPERATIONS)
        .find(|(f, _)| *f == file)
        .with_context(|| format!("no built-in {name}"))?;
    parse_action(Path::new(&file), text, shadow::text_blob_hash(text)?)
        .with_context(|| format!("built-in {file}"))
}

/// Every workflow, sorted by name.
pub fn load_all(home: &Path) -> Result<Vec<Workflow>> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.workflows.into_values().collect())
}

pub fn get(home: &Path, name: &str) -> Result<Option<Workflow>> {
    let mut cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.workflows.remove(name))
}

/// Files changed since the last commit of the directory, or all files if
/// it has never been committed.
pub fn uncommitted(home: &Path) -> Result<Vec<String>> {
    let dir = ensure(home)?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()?;
    Ok(crate::git::porcelain_paths(&String::from_utf8_lossy(
        &o.stdout,
    )))
}

/// The commit that introduced a blob into the directory's history, if it
/// has been committed. For "which commit do I check out to revert".
pub fn commit_for(home: &Path, hash: &str) -> Option<String> {
    let dir = dir_of(home);
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["log", "--format=%h %cs", "--find-object", hash, "--reverse"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .next()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn every_built_in_parses_on_its_own_and_names_are_unique() {
        // A built-in no workflow references would otherwise fail only at
        // first load in production; and a duplicate entry is written once
        // and never noticed. Actions and operations share a directory;
        // workflows have their own, so `docs` may be both.
        let dir = tempfile::tempdir().unwrap();
        let mut actions = std::collections::HashSet::new();
        for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
            std::fs::write(dir.path().join(file), text).unwrap();
            let hash = blob_hash(dir.path(), &dir.path().join(file)).unwrap();
            let a = parse_action(&dir.path().join(file), text, hash)
                .unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert!(
                actions.insert(a.name.clone()),
                "{file}: duplicate built-in action {}",
                a.name
            );
        }
        let mut workflows = std::collections::HashSet::new();
        for (file, text) in BUILTIN_WORKFLOWS {
            std::fs::write(dir.path().join(file), text).unwrap();
            let hash = blob_hash(dir.path(), &dir.path().join(file)).unwrap();
            let w = parse_workflow(&dir.path().join(file), text, hash)
                .unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert!(
                workflows.insert(w.name.clone()),
                "{file}: duplicate built-in workflow {}",
                w.name
            );
        }
    }

    #[test]
    fn builtins_resolve_and_carry_blob_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let all = load_all(dir.path()).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec![
                "cheap",
                "concierge",
                "direct",
                "docs",
                "documented",
                "intake",
                "mapped",
                "planned",
                "playable",
                "polish",
                "reviewed",
                "tdd",
                "tdd-reviewed"
            ]
        );
        for w in &all {
            resolve(dir.path(), &w.name).unwrap_or_else(|e| panic!("{}: {e:#}", w.name));
        }
        let r = resolve(dir.path(), "tdd").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "repo-map", "code"]
        );
        assert_eq!(
            r.steps[0].max_turns,
            Some(40),
            "the action's own default applies"
        );
        assert_eq!(r.steps[1].action.kind, Kind::Operation);
        assert_eq!(r.pins.len(), 5, "the workflow and four actions");
        let rr = resolve(dir.path(), "tdd-reviewed").unwrap();
        assert_eq!(
            rr.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "repo-map", "code", "review"]
        );
        assert!(r.pins.iter().all(|p| p.hash.len() == 40), "git blob hashes");
        assert!(check(dir.path()).unwrap().iter().all(|p| !p.blocking));
        // Editing a file changes only its own version.
        let before = resolve(dir.path(), "direct").unwrap();
        write(
            dir.path(),
            "actions/code.toml",
            "name = \"code\"\nkind = \"directive\"\ndescription = \"x\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\nmax_turns = 50\n",
        );
        // Only an operator commit makes a copy of a built-in win.
        for args in [
            &["add", "actions/code.toml"][..],
            &[
                "-c",
                "user.name=op",
                "-c",
                "user.email=op@x",
                "commit",
                "-qm",
                "tune code",
            ],
        ] {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path().join("workflows"))
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        }
        let after = resolve(dir.path(), "direct").unwrap();
        assert_ne!(before.pins, after.pins);
        assert_eq!(
            after.steps[2].max_turns,
            Some(50),
            "code is now the third step, after repo-map"
        );
        assert_eq!(
            before.pins.iter().find(|p| p.name == "setup"),
            after.pins.iter().find(|p| p.name == "setup"),
            "setup is unchanged"
        );
    }

    #[test]
    fn the_docs_name_every_built_in() {
        // Agents read docs/ACTIONS.md as instructions; a mechanism the docs
        // do not name might as well not exist, and one they name that does
        // not exist is worse.
        let docs = include_str!("../../docs/ACTIONS.md");
        for (file, text) in BUILTIN_ACTIONS
            .iter()
            .chain(BUILTIN_OPERATIONS)
            .chain(BUILTIN_WORKFLOWS)
        {
            let name = text
                .lines()
                .find_map(|l| l.strip_prefix("name = "))
                .map(|n| n.trim_matches('"'))
                .unwrap_or_else(|| panic!("{file} has no name"));
            assert!(
                docs.contains(&format!("`{name}`")),
                "docs/ACTIONS.md does not mention `{name}` ({file})"
            );
        }

        // The built-in workflows table must list each workflow's steps in
        // the exact order the kernel resolves them, compositions spliced
        // inline: a table that lied about the order would mislead whoever
        // picks a workflow by reading it.
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        for (file, _) in BUILTIN_WORKFLOWS {
            let name = file.strip_suffix(".toml").unwrap();
            let resolved = resolve(dir.path(), name).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let steps: Vec<&str> = resolved
                .steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect();
            let row = docs
                .lines()
                .find(|l| l.starts_with(&format!("| `{name}` |")))
                .unwrap_or_else(|| {
                    panic!("docs/ACTIONS.md has no built-in workflows row for `{name}`")
                });
            let cell = row
                .split('|')
                .nth(2)
                .unwrap_or_else(|| panic!("malformed built-in workflows row for `{name}`: {row}"));
            let listed: Vec<&str> = cell
                .split('→')
                .map(|s| s.split('(').next().unwrap().trim())
                .collect();
            assert_eq!(
                listed, steps,
                "docs/ACTIONS.md built-in workflows row for `{name}` does not match how it resolves"
            );
        }
    }
}
