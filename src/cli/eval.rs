//! `forge eval jev`: scores the jev runner against what the record already
//! knows (docs/EXECUTION.md, "Measuring the judgment tier"). Three labeled
//! sets are pulled from the store, each item is judged through the named
//! provider, and the report says how often Jev agreed with the label, how
//! well its confidence was calibrated, how long it took and what it cost.
//! Nothing routes through Jev because of this; it only measures.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod report;
mod sets;

/// Lines changed (added plus deleted, base to landing) up to which a landed
/// task is `small`; `MEDIUM_LINES` is the same for `medium`, above is `large`.
pub(super) const SMALL_LINES: u64 = 50;
pub(super) const MEDIUM_LINES: u64 = 300;

#[derive(Subcommand)]
pub(super) enum EvalCmd {
    /// Judge the store's labeled sets through a `jev` provider and report
    /// accuracy, calibration, latency and cost per set; writes the report to
    /// `docs/research/jev-eval-<date>.md` and prints it
    Jev {
        /// The `[providers.<name>]` to judge with; its runner must be `jev`
        #[arg(long)]
        provider: String,
        /// Replay the sets recorded in this JSON file instead of pulling
        /// them from the store: `{"concierge": [{"state", "label"}], "demotions": [...], "size": [...]}`
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// Write the pulled sets to this JSON file, in the `--fixture` form
        #[arg(long)]
        record: Option<PathBuf>,
        /// Judge at most this many items per set (the newest)
        #[arg(long)]
        limit: Option<usize>,
        /// Where to write the report (default `docs/research/jev-eval-<date>.md`)
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// One labeled item: the state Jev is shown and the label the record gives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Item {
    pub state: String,
    pub label: String,
}

/// A labeled set and the one question Jev is asked over each of its items.
pub(super) struct Set {
    pub name: &'static str,
    pub title: &'static str,
    /// `choice`, `noul` or `score`.
    pub kind: &'static str,
    pub instructions: String,
    /// The label each answer may take, with what it means.
    pub criteria: Vec<(&'static str, String)>,
    pub items: Vec<Item>,
}

impl Set {
    /// The request Jev is sent for `state`, in TypeSafe's shape (see
    /// `agent::call`): a `choice`'s criteria are its
    /// labels with their meanings, a `noul`'s are `true` (`yes`) and `false`
    /// (`no`), and a `score`'s are its levels' names in order.
    fn request(&self, model: &str, state: &str) -> Value {
        let mut q = json!({"type": self.kind, "instructions": self.instructions});
        q["criteria"] = match self.kind {
            "score" => self
                .criteria
                .iter()
                .map(|(l, _)| *l)
                .collect::<Vec<_>>()
                .into(),
            "noul" => self
                .criteria
                .iter()
                .map(|(l, d)| {
                    let key = match *l {
                        "yes" => "true",
                        "no" => "false",
                        other => other,
                    };
                    (key.to_string(), Value::from(d.clone()))
                })
                .collect(),
            _ => self.criteria.iter().cloned().collect(),
        };
        json!({"model": model, "state": state, "questions": {
            crate::workflows::OUTCOME_QUESTION: q,
        }})
    }
}

/// What Jev answered for one item.
pub(super) struct Judged {
    pub predicted: Option<String>,
    pub confidence: f64,
    pub latency_ms: u128,
    pub cost_usd: f64,
    pub error: Option<String>,
}

/// The answer's label and confidence (`crate::agent::read_answer`), a
/// noul's `true`/`false` read as the set's `yes`/`no`.
pub(super) fn read_answer(answers: &Value) -> Option<(String, f64)> {
    let a = &answers[crate::workflows::OUTCOME_QUESTION];
    let a = if a.is_object() {
        a
    } else {
        answers.as_object()?.values().next()?
    };
    let read = crate::agent::read_answer(a)?;
    let label = match read.label.to_lowercase().as_str() {
        "true" => "yes".to_string(),
        "false" => "no".to_string(),
        other => other.to_string(),
    };
    Some((label, read.confidence))
}

async fn judge(provider: &crate::agent::Provider, set: &Set, item: &Item) -> Judged {
    let model = provider
        .model
        .clone()
        .unwrap_or_else(|| crate::agent::JEV_DEFAULT_MODEL.to_string());
    let body = set.request(&model, &item.state);
    let start = std::time::Instant::now();
    let asked = crate::agent::ask(provider, &body, Duration::from_secs(60)).await;
    let latency_ms = start.elapsed().as_millis();
    let (predicted, confidence, cost_usd, error) = match asked {
        Ok((answers, cost)) => match read_answer(&answers) {
            Some((l, c)) => (Some(l), c, cost, None),
            None => (None, 0.0, cost, Some("no answer in the response".into())),
        },
        Err(e) => (None, 0.0, 0.0, Some(format!("{e:#}"))),
    };
    Judged {
        predicted,
        confidence,
        latency_ms,
        cost_usd,
        error,
    }
}

async fn jev(
    provider: String,
    fixture: Option<PathBuf>,
    record: Option<PathBuf>,
    limit: Option<usize>,
    out_path: Option<PathBuf>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let p = f
        .providers
        .get(&provider)
        .with_context(|| format!("no provider {provider:?}; `forge providers` lists them"))?
        .clone();
    if p.runner != crate::agent::Runner::Jev {
        bail!(
            "provider {provider:?} runs {}, not jev; `forge eval jev` measures the jev runner",
            p.runner.as_str()
        );
    }
    let mut all = match &fixture {
        Some(path) => sets::from_fixture(path)?,
        None => sets::from_store(&f).await?,
    };
    if let Some(path) = &record {
        sets::record(path, &all)?;
    }
    let mut results = Vec::new();
    for set in &mut all {
        if let Some(n) = limit {
            let keep = set.items.len().saturating_sub(n);
            set.items.drain(..keep);
        }
        let mut judged = Vec::new();
        for item in &set.items {
            judged.push(judge(&p, set, item).await);
        }
        results.push(judged);
    }
    let day = crate::doctor::ymd(unix_now());
    let text = report::render(&provider, &day, fixture.is_some(), &all, &results);
    let path =
        out_path.unwrap_or_else(|| PathBuf::from(format!("docs/research/jev-eval-{day}.md")));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    out!("{}", text.trim_end());
    Ok(())
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Eval {
            cmd:
                EvalCmd::Jev {
                    provider,
                    fixture,
                    record,
                    limit,
                    out,
                },
        } => jev(provider, fixture, record, limit, out).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
