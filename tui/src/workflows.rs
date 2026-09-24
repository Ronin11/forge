//! The workflows screen: the catalog and every project's own workflows
//! with their measured profiles, and, on Enter, one workflow's file and
//! resolved steps, read-only (editing stays in the web client).

use crate::{App, Screen, short};
use crossterm::event::KeyCode;
use forge_client::{Workflow, WorkflowShowDoc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use serde_json::Value;

#[derive(Default)]
pub struct Workflows {
    /// Each workflow with the project it was found in (`None`: the catalog).
    pub list: Vec<(Workflow, Option<String>)>,
    pub sel: usize,
    pub open: Option<(WorkflowShowDoc, Option<String>)>,
    pub scroll: usize,
}

fn pct(v: &Value) -> String {
    v.as_f64()
        .map_or("-".into(), |v| format!("{:.0}%", v * 100.0))
}

/// The measured profile in one line, as the web list shows it.
pub fn profile(measured: &Value) -> String {
    let c = &measured["current"];
    let Some(n) = c["n"].as_i64().filter(|n| *n > 0) else {
        return "no runs".into();
    };
    if c["known"].as_bool() != Some(true) {
        return format!("{n} run(s), not yet known");
    }
    let cost = c["cost_per_success"]
        .as_f64()
        .map_or("no successes".into(), |v| format!("${v:.2}/success"));
    let mark = if measured["regressed"].as_bool() == Some(true) {
        "REGRESSION "
    } else {
        ""
    };
    format!(
        "{mark}{} ({}-{}) {cost} {n} run(s)",
        pct(&c["rate"]),
        pct(&c["rate_lo"]),
        pct(&c["rate_hi"])
    )
}

fn chain(w: &Workflow) -> String {
    let steps = w
        .resolved
        .as_array()
        .filter(|a| !a.is_empty())
        .or(w.steps.as_array());
    let names: Vec<String> = steps
        .map(|a| {
            a.iter()
                .map(|s| {
                    s["action"].as_str().map(String::from).unwrap_or_else(|| {
                        s["workflow"]
                            .as_str()
                            .map_or("?".into(), |n| format!("({n})"))
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        "no steps".into()
    } else {
        names.join(" > ")
    }
}

impl App {
    pub(crate) fn load_workflows(&mut self) {
        let read = || -> anyhow::Result<Vec<(Workflow, Option<String>)>> {
            let mut out: Vec<_> = self
                .forge
                .workflow_list(None)?
                .into_iter()
                .map(|w| (w, None))
                .collect();
            for p in self.forge.project_list()? {
                for w in self.forge.workflow_list(Some(&p.name))? {
                    if w.source == "repo" {
                        out.push((w, Some(p.name.clone())));
                    }
                }
            }
            Ok(out)
        };
        match read() {
            Ok(list) => self.workflows.list = list,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.workflows.sel = self
            .workflows
            .sel
            .min(self.workflows.list.len().saturating_sub(1));
    }

    pub(crate) fn workflows_key(&mut self, code: KeyCode) -> bool {
        if self.screen != Screen::Workflows {
            return false;
        }
        let w = &mut self.workflows;
        match code {
            KeyCode::Char('j') | KeyCode::Down if w.open.is_some() => w.scroll += 1,
            KeyCode::Char('k') | KeyCode::Up if w.open.is_some() => {
                w.scroll = w.scroll.saturating_sub(1)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                w.sel = (w.sel + 1).min(w.list.len().saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => w.sel = w.sel.saturating_sub(1),
            KeyCode::Esc if w.open.is_some() => w.open = None,
            KeyCode::Enter if w.open.is_none() => {
                let Some((wf, project)) = w.list.get(w.sel).cloned() else {
                    return true;
                };
                match self.forge.workflow_show(&wf.name, project.as_deref()) {
                    Ok(doc) => {
                        self.workflows.open = Some((doc, project));
                        self.workflows.scroll = 0;
                    }
                    Err(e) => self.status = format!("{e:#}"),
                }
            }
            _ => return false,
        }
        true
    }
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let w = &app.workflows;
    if let Some((doc, project)) = &w.open {
        let mut lines = vec![
            Line::styled(
                format!(
                    "{} ({}, {})  {}",
                    doc.name,
                    doc.kind,
                    project.as_deref().unwrap_or("catalog"),
                    doc.path
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(profile(&doc.measured)),
            Line::styled(
                "Resolved steps",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
        if doc.steps.is_empty() {
            lines.push(Line::raw("  no steps"));
        }
        for (i, s) in doc.steps.iter().enumerate() {
            let mut extra = vec![s.kind.clone()];
            extra.extend(s.model.clone());
            extra.extend(s.max_turns.map(|t| format!("{t} turns")));
            extra.extend(s.timeout_secs.map(|t| format!("{t}s")));
            lines.push(Line::raw(format!(
                "  {}. {} [{}]",
                i + 1,
                s.name,
                extra.join(", ")
            )));
            let note = [s.contract.as_str(), s.description.as_str()]
                .iter()
                .filter(|t| !t.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" - ");
            if !note.is_empty() {
                lines.push(Line::raw(format!("       {}", short(&note, 200))));
            }
        }
        lines.push(Line::styled(
            "File",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        lines.extend(doc.text.lines().map(|l| Line::raw(format!("  {l}"))));
        let body: Vec<Line> = lines.into_iter().skip(w.scroll).collect();
        frame.render_widget(
            Paragraph::new(body).block(Block::bordered().title("workflow (read-only)")),
            area,
        );
        return;
    }
    let rows: Vec<Row> = w
        .list
        .iter()
        .map(|(wf, project)| {
            Row::new(vec![
                Cell::from(wf.name.clone()),
                Cell::from(wf.kind.clone()),
                Cell::from(project.clone().unwrap_or_else(|| "catalog".into())),
                Cell::from(short(&chain(wf), 22)),
                Cell::from(profile(&wf.measured)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(22),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["NAME", "KIND", "SOURCE", "STEPS", "MEASURED"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title("workflows"));
    let mut st = TableState::default();
    st.select((!w.list.is_empty()).then_some(w.sel));
    frame.render_stateful_widget(table, area, &mut st);
}
