//! Filing an initiative's tasks: parsing and validating a `--from` file's
//! paragraphs, filing them in order, and filing a task's recorded plan.

use super::*;

/// One task parsed from an initiative's `--from` file: a paragraph, its
/// optional dependency on an earlier paragraph (1-based, within the
/// file), its optional repository override, its optional provider
/// override, its optional workflow override, and its text.
#[derive(Debug)]
pub struct FileTask {
    pub after: Option<usize>,
    pub repo: Option<String>,
    pub provider: Option<String>,
    pub workflow: Option<String>,
    pub text: String,
}

/// Parse an initiative's task file: one task per paragraph (blank-line
/// separated), each optionally led by an `after: <n>` line naming an
/// earlier paragraph in the file as a dependency, a `repo: <path>` line
/// naming the repository it runs against instead of the project's first
/// one, a `provider: <name>` line naming the provider it runs under
/// instead of `--provider`'s default, and a `workflow: <name>` line
/// naming the workflow it runs under instead of `--workflow`'s default
/// (see docs/PROJECTS.md, "Verbs"). The lead lines may appear in any
/// order, one per line, at the top of the paragraph; whatever is left
/// is the task's text. A malformed lead line (an `after:` with no
/// parseable, in-range paragraph number, or a `repo:`/`provider:`/
/// `workflow:` with no value) refuses the whole file, naming the
/// paragraph and the offending line, rather than silently dropping the
/// paragraph.
pub fn parse_initiative_file(text: &str) -> Result<Vec<FileTask>> {
    let mut out: Vec<FileTask> = Vec::new();
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        // 1-based, and counted only over paragraphs that hold a task, so
        // it matches the position an `after:` line in a later paragraph
        // means to name.
        let this_no = out.len() + 1;
        let mut after = None;
        let mut repo = None;
        let mut provider = None;
        let mut workflow = None;
        let mut body: Vec<&str> = Vec::new();
        let mut in_lead = true;
        for line in para.lines() {
            if in_lead && let Some(n) = line.strip_prefix("after:") {
                let Ok(n) = n.trim().parse::<usize>() else {
                    bail!(
                        "paragraph {this_no}: malformed header {line:?}: `after:` needs a paragraph number"
                    );
                };
                if n == 0 || n >= this_no {
                    bail!(
                        "paragraph {this_no}: malformed header {line:?}: does not name an earlier paragraph in this file"
                    );
                }
                after = Some(n);
                continue;
            }
            if in_lead && let Some(p) = line.strip_prefix("repo:") {
                let p = p.trim();
                if p.is_empty() {
                    bail!("paragraph {this_no}: malformed header {line:?}: `repo:` needs a path");
                }
                repo = Some(p.to_string());
                continue;
            }
            if in_lead && let Some(p) = line.strip_prefix("provider:") {
                let p = p.trim();
                if p.is_empty() {
                    bail!(
                        "paragraph {this_no}: malformed header {line:?}: `provider:` needs a name"
                    );
                }
                provider = Some(p.to_string());
                continue;
            }
            if in_lead && let Some(w) = line.strip_prefix("workflow:") {
                let w = w.trim();
                if w.is_empty() {
                    bail!(
                        "paragraph {this_no}: malformed header {line:?}: `workflow:` needs a name"
                    );
                }
                workflow = Some(w.to_string());
                continue;
            }
            in_lead = false;
            body.push(line);
        }
        let body = body.join("\n").trim().to_string();
        if body.is_empty() {
            bail!("paragraph {this_no} has no task text");
        }
        out.push(FileTask {
            after,
            repo,
            provider,
            workflow,
            text: body,
        });
    }
    Ok(out)
}

/// Validate every parsed paragraph's `provider:` and `workflow:` override
/// (see [`parse_initiative_file`]) against what's actually configured,
/// refusing with the paragraph's 1-based number and the unknown name
/// rather than filing tasks that would fail once run.
pub fn validate_initiative_file(f: &Forge, parsed: &[FileTask]) -> Result<()> {
    for (i, p) in parsed.iter().enumerate() {
        let n = i + 1;
        if let Some(name) = &p.provider {
            f.providers.get(name).with_context(|| {
                format!(
                    "paragraph {n}: unknown provider {name:?}; see `forge providers` for what is configured"
                )
            })?;
        }
        if let Some(name) = &p.workflow {
            workflows::get(&f.paths.home, name)?.with_context(|| {
                format!(
                    "paragraph {n}: unknown workflow {name:?}; see `forge workflows` for what is configured"
                )
            })?;
        }
    }
    Ok(())
}

/// File already-parsed paragraphs into an existing initiative: one task
/// per paragraph, honoring each one's own `after`/`repo`/`provider`/
/// `workflow` override, else the given defaults. Shared by `forge
/// initiative new --from` (a hand-written file) and the escalator (a
/// pattern proposal answered yes; see docs/INTAKE.md, "The escalator"),
/// whose paragraphs are generated rather than read from disk. Returns the
/// new tasks' ids, in order.
// Reason: one parameter per default a paragraph may override.
#[allow(clippy::too_many_arguments)]
pub async fn file_initiative_paragraphs(
    f: &Forge,
    project: &str,
    initiative: i64,
    paragraphs: &[FileTask],
    default_repo: Option<&str>,
    provider: Option<&str>,
    workflow: Option<&str>,
    priority: Option<i64>,
) -> Result<Vec<i64>> {
    let mut ids: Vec<i64> = Vec::new();
    for p in paragraphs {
        let repo = match &p.repo {
            Some(r) => r.clone(),
            None => default_repo
                .map(str::to_string)
                .with_context(|| format!("project {project} lists no repository"))?,
        };
        let after = match p.after {
            Some(n) => vec![
                *ids.get(n - 1)
                    .with_context(|| format!("after: {n} names a task not yet queued"))?,
            ],
            None => Vec::new(),
        };
        let req = TaskRequest {
            repo: PathBuf::from(repo),
            task: p.text.clone(),
            provider: p.provider.clone().or_else(|| provider.map(str::to_string)),
            workflow: p.workflow.clone().or_else(|| workflow.map(str::to_string)),
            max_turns: 100,
            retries: 1,
            timeout_secs: 1800,
            after,
            project: Some(project.to_string()),
            initiative: Some(initiative),
            priority,
            ..Default::default()
        };
        let t = enqueue(f, &req, None).await?;
        ids.push(t.id);
    }
    Ok(ids)
}

/// Split a task's recorded plan into items: paragraphs (blank-line
/// separated), trimmed, empties dropped. A plan is prose from the
/// investigate directive rather than a delimited list, so this uses the
/// same split `parse_initiative_file` gives a hand-written file.
pub fn plan_items(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// File a task's recorded plan into an initiative: one task per plan
/// item, in order, each depending on the previous, against the
/// originating task's repository, with the originating task recorded as
/// a reference of kind `plan` on each (see docs/PROJECTS.md, "Tasks").
/// Returns the new tasks' ids, in order. The caller checks the plan is
/// non-empty; called only once one is known to exist.
pub async fn file_plan(f: &Forge, origin: &Task, initiative: i64) -> Result<Vec<i64>> {
    let items = plan_items(&origin.plan);
    let mut ids: Vec<i64> = Vec::new();
    for item in &items {
        let req = TaskRequest {
            repo: PathBuf::from(&origin.repo),
            task: item.clone(),
            model: None, // the provider's default; only an explicit --model pins one
            max_turns: 100,
            retries: 1,
            timeout_secs: 1800,
            after: ids.last().copied().into_iter().collect(),
            initiative: Some(initiative),
            ..Default::default()
        };
        let t = enqueue(f, &req, None).await?;
        f.store.insert_task_ref(
            t.id,
            "plan",
            &format!("forge://task/{}", origin.id),
            "",
            "operator",
        )?;
        ids.push(t.id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::tests::fixture_forge;

    #[test]
    fn parse_initiative_file_reads_after_and_repo_lines_and_leaves_the_rest_as_text() {
        let tasks = parse_initiative_file(
            "repo: /a\nfirst task\nsecond line\n\nafter: 1\nsecond task\n\nafter: 1\nrepo: /b\nthird task",
        )
        .unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].repo.as_deref(), Some("/a"));
        assert_eq!(tasks[0].after, None);
        assert_eq!(tasks[0].text, "first task\nsecond line");
        assert_eq!(tasks[1].repo, None);
        assert_eq!(tasks[1].after, Some(1));
        assert_eq!(tasks[1].text, "second task");
        assert_eq!(tasks[2].repo.as_deref(), Some("/b"));
        assert_eq!(tasks[2].after, Some(1));
        assert_eq!(tasks[2].text, "third task");
    }

    #[test]
    fn parse_initiative_file_reads_a_provider_line() {
        let tasks = parse_initiative_file("provider: devhome\nfirst task\n\nsecond task").unwrap();
        assert_eq!(tasks[0].provider.as_deref(), Some("devhome"));
        assert_eq!(tasks[0].text, "first task");
        assert_eq!(tasks[1].provider, None);
    }

    #[test]
    fn plan_items_splits_on_blank_lines_and_drops_empties() {
        let items = plan_items("first item\nmore of it\n\n\nsecond item\n\nthird item\n");
        assert_eq!(
            items,
            vec!["first item\nmore of it", "second item", "third item"]
        );
        assert_eq!(plan_items("  \n\n  "), Vec::<String>::new());
    }

    #[test]
    fn parse_initiative_file_refuses_after_that_names_itself_or_the_future() {
        assert!(parse_initiative_file("after: 1\nonly task").is_err());
        assert!(parse_initiative_file("first task\n\nafter: 2\nsecond task").is_err());
    }

    #[test]
    fn parse_initiative_file_reads_all_four_headers_in_any_order() {
        // Every permutation of after:, repo:, provider:, workflow: leads
        // the fifth paragraph the same way, regardless of which order
        // the four lines appear in.
        let headers = [
            ("after: 4", "after"),
            ("repo: /path", "repo"),
            ("provider: devhome", "provider"),
            ("workflow: direct", "workflow"),
        ];
        let mut orders: Vec<Vec<usize>> = Vec::new();
        fn permute(cur: &mut Vec<usize>, remaining: &[usize], out: &mut Vec<Vec<usize>>) {
            if remaining.is_empty() {
                out.push(cur.clone());
                return;
            }
            for (i, &r) in remaining.iter().enumerate() {
                cur.push(r);
                let mut rest = remaining.to_vec();
                rest.remove(i);
                permute(cur, &rest, out);
                cur.pop();
            }
        }
        permute(&mut Vec::new(), &[0, 1, 2, 3], &mut orders);

        for order in orders {
            let lead: String = order
                .iter()
                .map(|&i| headers[i].0)
                .collect::<Vec<_>>()
                .join("\n");
            let text = format!("one\n\ntwo\n\nthree\n\nfour\n\n{lead}\nthe fifth task");
            let tasks = parse_initiative_file(&text)
                .unwrap_or_else(|e| panic!("order {order:?} failed: {e}"));
            assert_eq!(tasks.len(), 5, "order {order:?}");
            let fifth = &tasks[4];
            assert_eq!(fifth.after, Some(4), "order {order:?}");
            assert_eq!(fifth.repo.as_deref(), Some("/path"), "order {order:?}");
            assert_eq!(
                fifth.provider.as_deref(),
                Some("devhome"),
                "order {order:?}"
            );
            assert_eq!(fifth.workflow.as_deref(), Some("direct"), "order {order:?}");
            assert_eq!(fifth.text, "the fifth task", "order {order:?}");
        }
    }

    #[test]
    fn parse_initiative_file_refuses_a_malformed_header_naming_the_paragraph_and_line() {
        let err = parse_initiative_file("one\n\nafter: 4\nrepo: /path\nthe fifth task")
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 2"), "{err}");
        assert!(err.contains("after: 4"), "{err}");

        let err = parse_initiative_file("repo:\nfirst task")
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 1"), "{err}");
        assert!(err.contains("repo:"), "{err}");

        let err = parse_initiative_file("provider: \nfirst task")
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 1"), "{err}");
        assert!(err.contains("provider:"), "{err}");

        let err = parse_initiative_file("workflow: \nfirst task")
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 1"), "{err}");
        assert!(err.contains("workflow:"), "{err}");
    }

    #[test]
    fn validate_initiative_file_refuses_an_unknown_provider_naming_the_paragraph() {
        let (_dir, f) = fixture_forge();
        let parsed = parse_initiative_file(
            "first task\n\nsecond task\n\nprovider: does-not-exist\nthird task",
        )
        .unwrap();
        let err = validate_initiative_file(&f, &parsed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 3"), "{err}");
        assert!(err.contains("does-not-exist"), "{err}");
    }

    #[test]
    fn validate_initiative_file_refuses_an_unknown_workflow_naming_the_paragraph() {
        let (_dir, f) = fixture_forge();
        let parsed = parse_initiative_file(
            "first task\n\nsecond task\n\nworkflow: does-not-exist\nthird task",
        )
        .unwrap();
        let err = validate_initiative_file(&f, &parsed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("paragraph 3"), "{err}");
        assert!(err.contains("does-not-exist"), "{err}");
    }

    #[test]
    fn validate_initiative_file_passes_a_file_naming_a_configured_provider_and_workflow() {
        let (_dir, f) = fixture_forge();
        let parsed = parse_initiative_file(
            "provider: anthropic\nfirst task\n\nworkflow: direct\nsecond task",
        )
        .unwrap();
        validate_initiative_file(&f, &parsed).unwrap();
    }
}
