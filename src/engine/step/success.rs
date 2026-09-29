//! Record the product of a verified directive before advancing the workflow.
use super::*;

pub(super) async fn record_success(
    f: &Forge,
    t: &mut Task,
    step: &workflows::ResolvedStep,
    repo: &Path,
    remote_url: &Option<String>,
    verdict: &verify::Verdict,
) -> Result<Option<StepFlow>, Fault> {
    let id = t.id;
    if step.action.contract == Contract::Tests {
        let tests_dir = tests_clone_dir(&t.worktree);
        git::push_to_repo(&f.paths.home, repo, &tests_dir, &format!("verify/{}", t.id))
            .await
            .task()?;
        if let Some(url) = &remote_url
            && let Err(e) = git::push(
                &f.paths.home,
                repo,
                &tests_dir,
                url,
                &format!("verify/{}", t.id),
            )
            .await
        {
            f.report.emit(
                id,
                Event::Note {
                    text: &format!("tests    push of verify/{} failed: {e:#}", t.id),
                },
            );
        }
        t.interface = verdict
            .envelope
            .as_ref()
            .map(|e| e.summary.clone())
            .unwrap_or_default();
        f.store.update_task(t).env()?;
    }
    if step.action.contract == Contract::Plan {
        // The plan is the product: shown to every later
        // directive, verified only to name real paths.
        t.plan = verdict
            .envelope
            .as_ref()
            .map(|e| e.summary.clone())
            .unwrap_or_default();
        f.store.update_task(t).env()?;
        f.report.emit(
            id,
            Event::Note {
                text: &format!(
                    "plan     {} line(s) from {}",
                    t.plan.lines().count(),
                    step.action.name
                ),
            },
        );
        // file_into_initiative: the plan's items become
        // sibling tasks in the same initiative instead
        // of this task running the code step itself.
        if step.action.file_into_initiative
            && let Some(iid) = t.initiative
        {
            let filed = crate::queue::file_plan(f, t, iid).await.task()?;
            f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "filed    {} task(s) into initiative {iid}: {}",
                        filed.len(),
                        filed
                            .iter()
                            .map(i64::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
            );
            return Ok(Some(StepFlow::End(End::Filed {
                n: filed.len(),
                initiative: iid,
                last: *filed.last().unwrap_or(&id),
            })));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{Fixture, verdict};
    use super::*;

    #[tokio::test]
    async fn plan_product_is_persisted_and_an_absent_envelope_clears_it() {
        let mut x = Fixture::new();
        x.resolved.steps[1].action.contract = Contract::Plan;
        let mut v = verdict();
        v.envelope = Some(crate::envelope::Envelope {
            summary: "new plan".into(),
            ..Default::default()
        });
        for expected in ["new plan", ""] {
            let flow = record_success(
                &x.f,
                &mut x.t,
                &x.resolved.steps[1],
                x.dir.path(),
                &None,
                &v,
            )
            .await
            .ok()
            .unwrap();
            assert!(flow.is_none());
            assert_eq!(x.t.plan, expected);
            assert_eq!(x.f.store.task(x.t.id).unwrap().unwrap().plan, expected);
            v.envelope = None;
        }
    }
}
