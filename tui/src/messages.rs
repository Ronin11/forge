//! The messages screen: one project's message record (`forge message list
//! PROJECT --json`) and the concierge's own decisions on inbound messages
//! (`forge decisions --json --project PROJECT`), filterable by contact.

use crate::{App, Screen, short, time};
use crossterm::event::KeyCode;
use forge_client::DecisionRow;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct MessageRow {
    pub id: i64,
    pub channel: String,
    pub contact: String,
    pub direction: String,
    pub text: String,
    pub at: i64,
    pub task_id: Option<i64>,
}

#[derive(Default)]
pub struct Messages {
    pub project: Option<String>,
    pub contact: Option<String>,
    pub messages: Vec<MessageRow>,
    pub decisions: Vec<DecisionRow>,
    pub scroll: usize,
}

impl MessageRow {
    fn from_json(v: &Value) -> MessageRow {
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        MessageRow {
            id: v["id"].as_i64().unwrap_or_default(),
            channel: text("channel"),
            contact: text("contact"),
            direction: text("direction"),
            text: text("text"),
            at: v["at"].as_i64().unwrap_or_default(),
            task_id: v["task_id"].as_i64(),
        }
    }
}

impl Messages {
    fn contacts(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for m in &self.messages {
            if !m.contact.is_empty() && !seen.contains(&m.contact) {
                seen.push(m.contact.clone());
            }
        }
        seen
    }
}

impl App {
    pub(crate) fn load_messages(&mut self) {
        let read = || -> anyhow::Result<(Vec<String>, Vec<MessageRow>, Vec<DecisionRow>)> {
            let names: Vec<String> = self
                .forge
                .project_list()?
                .into_iter()
                .map(|p| p.name)
                .collect();
            let Some(project) = self
                .messages
                .project
                .clone()
                .filter(|p| names.contains(p))
                .or_else(|| names.first().cloned())
            else {
                return Ok((names, vec![], vec![]));
            };
            let messages = self.forge.json(&["message", "list", &project, "--json"])?;
            let decisions = self
                .forge
                .json(&["decisions", "--json", "--project", &project])?;
            Ok((
                names,
                messages
                    .as_array()
                    .map(|a| a.iter().map(MessageRow::from_json).collect())
                    .unwrap_or_default(),
                serde_json::from_value(decisions)?,
            ))
        };
        match read() {
            Ok((names, messages, decisions)) => {
                if self
                    .messages
                    .project
                    .as_ref()
                    .is_none_or(|p| !names.contains(p))
                {
                    self.messages.project = names.first().cloned();
                }
                self.messages.messages = messages;
                self.messages.decisions = decisions
                    .into_iter()
                    .filter(|d: &DecisionRow| d.answered_by == "concierge")
                    .collect();
                if self
                    .messages
                    .contact
                    .as_ref()
                    .is_some_and(|c| !self.messages.contacts().contains(c))
                {
                    self.messages.contact = None;
                }
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    pub(crate) fn messages_key(&mut self, code: KeyCode) -> bool {
        if self.screen != Screen::Messages {
            return false;
        }
        match code {
            KeyCode::Char('p') => {
                let names: Vec<String> = match self.forge.project_list() {
                    Ok(p) => p.into_iter().map(|p| p.name).collect(),
                    Err(e) => {
                        self.status = format!("{e:#}");
                        return true;
                    }
                };
                let at = self
                    .messages
                    .project
                    .as_ref()
                    .and_then(|p| names.iter().position(|n| n == p));
                self.messages.project = names
                    .get(at.map_or(0, |i| i + 1))
                    .or(names.first())
                    .cloned();
                self.messages.contact = None;
                self.messages.scroll = 0;
                self.load_messages();
            }
            KeyCode::Char('f') => {
                let contacts = self.messages.contacts();
                let at = self
                    .messages
                    .contact
                    .as_ref()
                    .and_then(|c| contacts.iter().position(|x| x == c));
                self.messages.contact = match (at, &self.messages.contact) {
                    (Some(i), _) => contacts.get(i + 1).cloned(),
                    (None, None) => contacts.first().cloned(),
                    (None, Some(_)) => None,
                };
                self.messages.scroll = 0;
            }
            KeyCode::Char('c') => self.messages.contact = None,
            KeyCode::Char('j') | KeyCode::Down => self.messages.scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.messages.scroll = self.messages.scroll.saturating_sub(1)
            }
            _ => return false,
        }
        true
    }
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    )
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let m = &app.messages;
    let mut lines = vec![heading("Messages")];
    let shown: Vec<&MessageRow> = m
        .messages
        .iter()
        .filter(|r| m.contact.as_deref().is_none_or(|c| c == r.contact))
        .collect();
    if shown.is_empty() {
        lines.push(Line::raw("  no messages"));
    }
    for r in shown {
        let dir = if r.direction == "in" { "from" } else { "to" };
        let task = r.task_id.map_or(String::new(), |t| format!(" task {t}"));
        lines.push(Line::raw(format!(
            "  {}  {dir} {} ({}{task})",
            time::fmt_time(r.at),
            r.contact,
            r.channel
        )));
        lines.push(Line::raw(format!("      {}", short(&r.text, 200))));
    }
    lines.push(heading("Concierge decisions"));
    if m.decisions.is_empty() {
        lines.push(Line::raw("  no concierge decisions"));
    }
    for d in &m.decisions {
        lines.push(Line::raw(format!("  {}", time::fmt_time(d.created_at))));
        lines.push(Line::raw(format!("      Q: {}", short(&d.question, 200))));
        lines.push(Line::raw(format!("      A: {}", short(&d.answer, 200))));
    }
    let title = format!(
        "messages  project: {}  contact: {}",
        m.project.as_deref().unwrap_or("-"),
        m.contact.as_deref().unwrap_or("all")
    );
    let body: Vec<Line> = lines.into_iter().skip(m.scroll).collect();
    frame.render_widget(
        Paragraph::new(body).block(Block::bordered().title(title)),
        area,
    );
}
