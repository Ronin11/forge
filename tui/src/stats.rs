//! The stats screen: `forge stats --json` as tabs, each a sortable table.

use forge_client::StatsDoc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StatsTab {
    Workflows,
    Quality,
    ByRole,
    HumanAttention,
    TimeToLive,
    Factors,
}

impl StatsTab {
    fn label(self) -> &'static str {
        match self {
            StatsTab::Workflows => "Workflows",
            StatsTab::Quality => "Quality",
            StatsTab::ByRole => "By role",
            StatsTab::HumanAttention => "Human attention",
            StatsTab::TimeToLive => "Time to live",
            StatsTab::Factors => "Factors",
        }
    }
}

/// The tabs a document shows: `Factors` only when the field is present.
pub fn visible_tabs(doc: &StatsDoc) -> Vec<StatsTab> {
    let mut tabs = vec![
        StatsTab::Workflows,
        StatsTab::Quality,
        StatsTab::ByRole,
        StatsTab::HumanAttention,
        StatsTab::TimeToLive,
    ];
    if doc.factors.is_some() {
        tabs.push(StatsTab::Factors);
    }
    tabs
}

/// A cell's sort value: text sorts ascending first, numbers descending first.
#[derive(Clone, PartialEq, PartialOrd)]
enum Val {
    Num(Option<f64>),
    Text(String),
}

struct Col {
    key: &'static str,
    label: &'static str,
}

struct StatsTable {
    title: Option<&'static str>,
    cols: Vec<Col>,
    rows: Vec<Vec<(Val, String)>>,
}

fn pct(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("{:.0}%", v * 100.0))
}
fn usd(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("${v:.2}"))
}
fn secs(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("{v:.0}s"))
}
fn n(v: i64) -> (Val, String) {
    (Val::Num(Some(v as f64)), v.to_string())
}
fn t(v: &str) -> (Val, String) {
    (Val::Text(v.to_owned()), v.to_owned())
}
fn f(v: Option<f64>, text: String) -> (Val, String) {
    (Val::Num(v), text)
}
fn on(v: Option<i64>) -> (Val, String) {
    (
        Val::Num(v.map(|v| v as f64)),
        v.map_or("-".into(), |v| v.to_string()),
    )
}

/// The verified-rate interval as a bar: `-` is the track, `━` the
/// `lo`..`hi` range, `●` the point.
pub fn rate_bar(rate: f64, lo: f64, hi: f64, width: usize) -> String {
    let cell = |v: f64| ((v.clamp(0.0, 1.0) * (width - 1) as f64).round() as usize).min(width - 1);
    let (l, h, p) = (cell(lo), cell(hi), cell(rate));
    (0..width)
        .map(|i| {
            if i == p {
                '●'
            } else if i >= l && i <= h {
                '━'
            } else {
                '─'
            }
        })
        .collect()
}

fn rate_cell(rate: f64, lo: f64, hi: f64, regressed: bool) -> (Val, String) {
    let mark = if regressed { " REGRESSION" } else { "" };
    (
        Val::Num(Some(rate)),
        format!(
            "{} {} {}–{}{mark}",
            rate_bar(rate, lo, hi, 8),
            pct(Some(rate)),
            pct(Some(lo)),
            pct(Some(hi))
        ),
    )
}

fn col(key: &'static str, label: &'static str) -> Col {
    Col { key, label }
}

fn tables(doc: &StatsDoc, tab: StatsTab) -> Vec<StatsTable> {
    match tab {
        StatsTab::Workflows => vec![StatsTable {
            title: None,
            cols: vec![
                col("workflow", "workflow"),
                col("hash", "hash"),
                col("pieces", "tasks"),
                col("succeeded", "ok"),
                col("failed", "fail"),
                col("blocked", "blk"),
                col("unverified", "unv"),
                col("attempts", "att"),
                col("mean_cost_usd", "cost"),
                col("cost_per_success_usd", "$/ok"),
                col("landed", "landed"),
                col("rate", "verified rate"),
            ],
            rows: doc
                .workflows
                .iter()
                .map(|r| {
                    vec![
                        t(&r.workflow),
                        t(&r.hash),
                        n(r.pieces),
                        n(r.succeeded),
                        n(r.failed),
                        n(r.blocked),
                        n(r.unverified),
                        n(r.attempts),
                        f(Some(r.mean_cost_usd), usd(Some(r.mean_cost_usd))),
                        f(r.cost_per_success_usd, usd(r.cost_per_success_usd)),
                        n(r.landed),
                        rate_cell(r.rate, r.rate_lo, r.rate_hi, r.regressed),
                    ]
                })
                .collect(),
        }],
        StatsTab::Quality => vec![StatsTable {
            title: None,
            cols: vec![
                col("workflow", "workflow"),
                col("hash", "hash"),
                col("landed", "landed"),
                col("broke_base", "broke base"),
                col("broke_base_share", "broke%"),
                col("repaired", "repaired"),
                col("repaired_share", "repair%"),
                col("repair_cost_usd", "repair cost"),
                col("true_cost_per_landed_usd", "true cost"),
                col("churn_share", "churn%"),
            ],
            rows: doc
                .workflows
                .iter()
                .map(|r| {
                    vec![
                        t(&r.workflow),
                        t(&r.hash),
                        n(r.landed),
                        n(r.broke_base),
                        f(r.broke_base_share, pct(r.broke_base_share)),
                        n(r.repaired),
                        f(r.repaired_share, pct(r.repaired_share)),
                        f(r.repair_cost_usd, usd(r.repair_cost_usd)),
                        f(r.true_cost_per_landed_usd, usd(r.true_cost_per_landed_usd)),
                        f(r.churn_share, pct(r.churn_share)),
                    ]
                })
                .collect(),
        }],
        StatsTab::ByRole => vec![StatsTable {
            title: None,
            cols: vec![
                col("role", "role"),
                col("provider", "provider"),
                col("model", "model"),
                col("kind", "kind"),
                col("attempts", "att"),
                col("succeeded_share", "succeed%"),
                col("mean_turns", "turns"),
                col("mean_cost_usd", "cost"),
                col("mean_secs", "secs"),
                col("landed", "landed"),
                col("broke_base", "broke base"),
                col("broke_base_share", "broke%"),
            ],
            rows: doc
                .by_role
                .iter()
                .map(|r| {
                    vec![
                        t(&r.role),
                        t(&r.provider),
                        t(&r.model),
                        t(&r.kind),
                        n(r.attempts),
                        f(r.succeeded_share, pct(r.succeeded_share)),
                        f(Some(r.mean_turns), format!("{:.1}", r.mean_turns)),
                        f(Some(r.mean_cost_usd), usd(Some(r.mean_cost_usd))),
                        f(Some(r.mean_secs), secs(Some(r.mean_secs))),
                        on(r.landed),
                        on(r.broke_base),
                        f(r.broke_base_share, pct(r.broke_base_share)),
                    ]
                })
                .collect(),
        }],
        StatsTab::HumanAttention => {
            let counts = |r: &forge_client::StatsAttentionRow| {
                vec![
                    n(r.landed),
                    n(r.operator_answers),
                    n(r.hand_landed),
                    n(r.withdrawals),
                    n(r.hand_commits),
                    n(r.events),
                    f(
                        r.events_per_landed,
                        r.events_per_landed
                            .map_or("-".into(), |v| format!("{v:.2}")),
                    ),
                ]
            };
            let count_cols = || {
                vec![
                    col("landed", "landed"),
                    col("operator_answers", "answers"),
                    col("hand_landed", "hand"),
                    col("withdrawals", "wdrawn"),
                    col("hand_commits", "handc"),
                    col("events", "events"),
                    col("events_per_landed", "evt/land"),
                ]
            };
            let mut by_wf = vec![col("workflow", "workflow"), col("hash", "hash")];
            by_wf.extend(count_cols());
            let mut by_project = vec![col("project", "project")];
            by_project.extend(count_cols());
            vec![
                StatsTable {
                    title: Some("By workflow"),
                    cols: by_wf,
                    rows: doc
                        .human_attention
                        .iter()
                        .map(|r| {
                            let mut row = vec![t(&r.workflow), t(&r.hash)];
                            row.extend(counts(r));
                            row
                        })
                        .collect(),
                },
                StatsTable {
                    title: Some("By project"),
                    cols: by_project,
                    rows: doc
                        .human_attention_projects
                        .iter()
                        .map(|r| {
                            let mut row = vec![t(&r.project)];
                            row.extend(counts(r));
                            row
                        })
                        .collect(),
                },
            ]
        }
        StatsTab::TimeToLive => {
            let tail = || {
                vec![
                    col("n", "n"),
                    col("median_secs", "median"),
                    col("p90_secs", "p90"),
                ]
            };
            let nums = |r: &forge_client::StatsTtlRow| {
                vec![
                    n(r.n),
                    f(r.median_secs, secs(r.median_secs)),
                    f(r.p90_secs, secs(r.p90_secs)),
                ]
            };
            let mut by_wf = vec![col("workflow", "workflow"), col("hash", "hash")];
            by_wf.extend(tail());
            let mut by_project = vec![col("project", "project")];
            by_project.extend(tail());
            vec![
                StatsTable {
                    title: Some("By workflow"),
                    cols: by_wf,
                    rows: doc
                        .time_to_live
                        .iter()
                        .map(|r| {
                            let mut row = vec![t(&r.workflow), t(&r.hash)];
                            row.extend(nums(r));
                            row
                        })
                        .collect(),
                },
                StatsTable {
                    title: Some("By project"),
                    cols: by_project,
                    rows: doc
                        .time_to_live_projects
                        .iter()
                        .map(|r| {
                            let mut row = vec![t(&r.project)];
                            row.extend(nums(r));
                            row
                        })
                        .collect(),
                },
            ]
        }
        StatsTab::Factors => vec![StatsTable {
            title: None,
            cols: vec![
                col("factor", "factor"),
                col("level", "level"),
                col("tasks", "tasks"),
                col("landed", "landed"),
                col("rate", "rate (95% CI)"),
                col("mean_true_cost_usd", "true cost"),
                col("is_reference", "ref"),
                col("effect", "effect(log$)"),
                col("effect_se", "se"),
            ],
            rows: doc
                .factors
                .iter()
                .flatten()
                .map(|r| {
                    let four = |v: Option<f64>| v.map_or("-".into(), |v| format!("{v:.4}"));
                    vec![
                        t(&r.factor),
                        t(&r.level),
                        n(r.tasks),
                        n(r.landed),
                        rate_cell(r.rate, r.rate_lo, r.rate_hi, false),
                        f(r.mean_true_cost_usd, usd(r.mean_true_cost_usd)),
                        t(if r.is_reference { "ref" } else { "" }),
                        f(r.effect, four(r.effect)),
                        f(r.effect_se, four(r.effect_se)),
                    ]
                })
                .collect(),
        }],
    }
}

/// The sort keys a tab offers, in column order, without repeats.
pub fn sort_keys(doc: &StatsDoc, tab: StatsTab) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Vec::new();
    for table in tables(doc, tab) {
        for c in table.cols {
            if !keys.contains(&c.key) {
                keys.push(c.key);
            }
        }
    }
    keys
}

/// Whether sorting by `key` starts descending: numbers do, text does not.
pub fn starts_descending(doc: &StatsDoc, tab: StatsTab, key: &str) -> bool {
    tables(doc, tab)
        .iter()
        .find_map(|table| {
            let i = table.cols.iter().position(|c| c.key == key)?;
            Some(matches!(table.rows.first()?[i].0, Val::Num(_)))
        })
        .unwrap_or(false)
}

/// Sorts by `key`; a missing value sorts last whichever way it goes.
fn sort_rows(table: &mut StatsTable, key: &str, desc: bool) {
    let Some(i) = table.cols.iter().position(|c| c.key == key) else {
        return;
    };
    table.rows.sort_by(|a, b| {
        let (av, bv) = (&a[i].0, &b[i].0);
        let missing = |v: &Val| matches!(v, Val::Num(None));
        match (missing(av), missing(bv)) {
            (true, true) => return std::cmp::Ordering::Equal,
            (true, false) => return std::cmp::Ordering::Greater,
            (false, true) => return std::cmp::Ordering::Less,
            _ => {}
        }
        let ord = av.partial_cmp(bv).unwrap_or(std::cmp::Ordering::Equal);
        if desc { ord.reverse() } else { ord }
    });
}

pub struct StatsView<'a> {
    pub doc: Option<&'a StatsDoc>,
    pub tab: usize,
    pub sort: Option<(&'static str, bool)>,
    pub scroll: u16,
}

pub fn draw(frame: &mut Frame, view: &StatsView, area: Rect) {
    let Some(doc) = view.doc else {
        frame.render_widget(
            Paragraph::new("no stats loaded").block(Block::bordered().title("stats")),
            area,
        );
        return;
    };
    let tabs = visible_tabs(doc);
    let tab = tabs[view.tab.min(tabs.len() - 1)];
    let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let labels: Vec<Span> = tabs
        .iter()
        .map(|&x| {
            if x == tab {
                Span::styled(
                    format!(" {} ", x.label()),
                    Style::default().add_modifier(Modifier::REVERSED),
                )
            } else {
                Span::raw(format!(" {} ", x.label()))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(labels)), bar);

    let mut extra = 0;
    let mut lines: Vec<Line> = Vec::new();
    if tab == StatsTab::Quality && !doc.assessment_correlation.is_empty() {
        let rho = |v: Option<f64>| v.map_or("-".into(), |v| format!("{v:.2}"));
        lines.push(Line::raw(
            doc.assessment_correlation
                .iter()
                .map(|r| format!("score vs {}: rho {} (n={})", r.measure, rho(r.rho), r.n))
                .collect::<Vec<_>>()
                .join(" · "),
        ));
        extra = 1;
    }
    let [corr, body] =
        Layout::vertical([Constraint::Length(extra), Constraint::Min(1)]).areas(body);
    if extra > 0 {
        frame.render_widget(Paragraph::new(lines), corr);
    }

    let mut all = tables(doc, tab);
    let count = all.len() as u32;
    let areas = Layout::vertical(vec![Constraint::Ratio(1, count); all.len()]).split(body);
    for (table, area) in all.iter_mut().zip(areas.iter()) {
        if let Some((key, desc)) = view.sort {
            sort_rows(table, key, desc);
        }
        let sorted_by = view.sort.map(|(k, _)| k);
        let widths: Vec<usize> = table
            .cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                table
                    .rows
                    .iter()
                    .map(|r| r[i].1.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(c.label.chars().count() + 2 * usize::from(sorted_by == Some(c.key)))
            })
            .collect();
        let header = Row::new(table.cols.iter().map(|c| {
            let arrow = match view.sort {
                Some((k, true)) if k == c.key => " ▼",
                Some((k, false)) if k == c.key => " ▲",
                _ => "",
            };
            Cell::from(format!("{}{arrow}", c.label))
        }))
        .style(Style::default().add_modifier(Modifier::BOLD));
        let skip = (view.scroll as usize).min(table.rows.len().saturating_sub(1));
        let rows: Vec<Row> = if table.rows.is_empty() {
            vec![Row::new(vec![Cell::from("no data")])]
        } else {
            table
                .rows
                .iter()
                .skip(skip)
                .map(|r| Row::new(r.iter().map(|(_, text)| Cell::from(text.clone()))))
                .collect()
        };
        let widget = Table::new(rows, widths.iter().map(|&w| Constraint::Length(w as u16)))
            .header(header)
            .block(Block::bordered().title(table.title.unwrap_or("stats")));
        frame.render_widget(widget, *area);
    }
}
