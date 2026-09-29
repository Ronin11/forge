//! Direct runs keep their slot: redraw held experiment arms before each
//! attempt, and wait only when no eligible provider is free.
use super::*;
use crate::redraw::{self, Routing};
use std::collections::BTreeMap;
use std::time::Duration;

fn routing(f: &Forge, t: &Task, role: &str) -> Result<Routing, Fault> {
    let provider = &f.effective_provider(t, role).env()?.name;
    let Some(held) = crate::worker::window_hold(f, provider).env()? else {
        return Ok(Routing::Free);
    };
    let weights = redraw::arms(&f.paths.home, role, |p| f.providers.contains_key(p));
    let mut holds = BTreeMap::from([(provider.clone(), held)]);
    if let Some(weights) = &weights {
        for arm in weights.keys().filter(|arm| *arm != provider) {
            if let Some(held) = crate::worker::window_hold(f, arm).env()? {
                holds.insert(arm.clone(), held);
            }
        }
    }
    Ok(redraw::decide(t, role, provider, weights.as_ref(), |p| {
        holds.get(p).cloned()
    }))
}

pub(super) async fn before_attempt(
    f: &Forge,
    t: &mut Task,
    role: &str,
    wait: bool,
) -> Result<Option<StepFlow>, Fault> {
    if !wait {
        let provider = &f.effective_provider(t, role).env()?.name;
        return Ok(crate::worker::window_hold(f, provider)
            .env()?
            .map(|(msg, _)| StepFlow::Requeue(msg)));
    }
    let mut announced = None;
    loop {
        if let Some(end) = check_abort(f, t)? {
            return Ok(Some(StepFlow::End(end)));
        }
        match routing(f, t, role)? {
            Routing::Free => return Ok(None),
            Routing::Redrawn { explore, note } => {
                t.explore = explore;
                f.store.update_task(t).env()?;
                eprintln!("task {}: {note}", t.id);
                f.report.emit(t.id, Event::Note { text: &note });
                return Ok(None);
            }
            Routing::Held {
                provider,
                msg,
                until,
            } => {
                if announced.as_ref() != Some(&(provider.clone(), until)) {
                    let text = format!(
                        "{role}: waiting in foreground for {provider} until {until}: {msg}"
                    );
                    eprintln!("task {}: {text}", t.id);
                    f.report.emit(t.id, Event::Note { text: &text });
                    announced = Some((provider, until));
                }
                // Recheck aborts and externally released holds while retaining
                // ownership; never send a direct run back through FIFO.
                let seconds = (until - unix_now()).clamp(1, 60) as u64;
                tokio::time::sleep(Duration::from_secs(seconds)).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "../provider_hold_tests.rs"]
mod tests;
