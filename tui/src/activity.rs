//! The activity screen (a feed of forge events, filterable by project and
//! kind, over a strip of running attempts) and the task list's query line,
//! which maps `key:value` words onto `forge log --json`'s flags.

use crate::{App, Screen, short, time};
use crossterm::event::KeyCode;
use forge_client::Event;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};
use std::collections::{BTreeMap, VecDeque};

const FEED_MAX: usize = 300;
pub const PAGE: usize = 60;

pub const KINDS: [&str; 8] = [
    "task started",
    "attempt started",
    "attempt done",
    "check",
    "pushed",
    "task done",
    "question",
    "job",
];

pub struct FeedRow {
    pub ts: i64,
    pub task: i64,
    pub kind: &'static str,
    pub text: String,
}

#[derive(Default)]
pub struct Running {
    pub attempt: Option<(i64, i64)>,
    pub calls: i64,
    pub turns: Option<i64>,
    pub cost: Option<f64>,
}

#[derive(Default)]
pub struct Activity {
    pub feed: VecDeque<FeedRow>,
    pub running: BTreeMap<i64, Running>,
    pub project: Option<String>,
    pub kind: Option<&'static str>,
    pub scroll: usize,
}

/// The feed kind of an event, or `None` for one the feed does not show.
pub fn kind_of(event: &Event) -> Option<&'static str> {
    Some(match event {
        Event::TaskStarted { .. } => "task started",
        Event::AttemptStarted { .. } => "attempt started",
        Event::AttemptDone { .. } => "attempt done",
        Event::Check { .. } => "check",
        Event::Pushed { .. } => "pushed",
        Event::TaskDone { state, reason, .. } => {
            if state == "blocked" && reason.starts_with("needs input:") {
                "question"
            } else {
                "task done"
            }
        }
        Event::JobStarted { .. } | Event::JobFinished { .. } => "job",
        _ => return None,
    })
}

impl Activity {
    pub fn record(&mut self, event: &Event) {
        match event {
            Event::AttemptStarted { task, n, of, .. } => {
                self.running.insert(
                    *task,
                    Running {
                        attempt: Some((*n, *of)),
                        ..Running::default()
                    },
                );
            }
            Event::ToolCall { task, .. } => {
                if let Some(r) = self.running.get_mut(task) {
                    r.calls += 1;
                }
            }
            Event::AgentDone {
                task,
                turns,
                cost_usd,
                ..
            } => {
                if let Some(r) = self.running.get_mut(task) {
                    r.turns = Some(*turns);
                    r.cost = *cost_usd;
                }
            }
            Event::AttemptDone { task, .. } | Event::TaskDone { task, .. } => {
                self.running.remove(task);
            }
            _ => {}
        }
        let Some(kind) = kind_of(event) else { return };
        let (task, ts, text) = match event {
            Event::TaskStarted { task, ts, text, .. }
            | Event::AttemptStarted { task, ts, text, .. }
            | Event::AttemptDone { task, ts, text, .. }
            | Event::Check { task, ts, text, .. }
            | Event::Pushed { task, ts, text, .. }
            | Event::TaskDone { task, ts, text, .. }
            | Event::JobStarted { task, ts, text, .. }
            | Event::JobFinished { task, ts, text, .. } => (*task, *ts, text.clone()),
            _ => return,
        };
        self.feed.push_front(FeedRow {
            ts,
            task,
            kind,
            text,
        });
        self.feed.truncate(FEED_MAX);
    }
}

/// Which words of a query line `forge log --json` understands.
const KEYS: [(&str, &str); 6] = [
    ("state", "--state"),
    ("workflow", "--workflow"),
    ("project", "--project"),
    ("initiative", "--initiative"),
    ("repo", "--repo"),
    ("touches", "--touches"),
];

/// The `forge log --json` arguments a query line means: `key:value` words
/// become their flag; everything else is one `--grep` phrase.
pub fn log_args(query: &str, before: Option<i64>) -> Vec<String> {
    let mut args: Vec<String> = ["log", "--json", "--limit"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(PAGE.to_string());
    let mut grep = Vec::new();
    for word in query.split_whitespace() {
        match word.split_once(':') {
            Some((k, v)) if !v.is_empty() && KEYS.iter().any(|(name, _)| *name == k) => {
                let flag = KEYS.iter().find(|(name, _)| *name == k).unwrap().1;
                args.push(flag.into());
                args.push(v.into());
            }
            _ => grep.push(word),
        }
    }
    if !grep.is_empty() {
        args.push("--grep".into());
        args.push(grep.join(" "));
    }
    if let Some(b) = before {
        args.push("--before".into());
        args.push(b.to_string());
    }
    args
}

impl App {
    fn task_project(&self, task: i64) -> Option<&str> {
        self.tasks
            .iter()
            .find(|t| t.id == task)
            .and_then(|t| t.project.as_deref())
    }

    pub(crate) fn seed_running(&mut self) {
        for t in self.tasks.iter().filter(|t| t.state == "running") {
            self.activity.running.entry(t.id).or_default();
        }
    }

    fn projects(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .tasks
            .iter()
            .filter_map(|t| t.project.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn cycle_project(&mut self) {
        let projects = self.projects();
        let at = self
            .activity
            .project
            .as_ref()
            .and_then(|p| projects.iter().position(|x| x == p));
        self.activity.project = match at {
            None if self.activity.project.is_none() => projects.first().cloned(),
            Some(i) => projects.get(i + 1).cloned(),
            None => projects.first().cloned(),
        };
    }

    fn cycle_kind(&mut self) {
        let at = self
            .activity
            .kind
            .and_then(|k| KINDS.iter().position(|x| *x == k));
        self.activity.kind = match at {
            None if self.activity.kind.is_none() => Some(KINDS[0]),
            Some(i) => KINDS.get(i + 1).copied(),
            None => Some(KINDS[0]),
        };
    }

    /// Keys of the activity screen and the query line; `true` when consumed.
    pub(crate) fn activity_key(&mut self, code: KeyCode) -> bool {
        if let Some(mut text) = self.query_prompt.take() {
            match code {
                KeyCode::Esc => return true,
                KeyCode::Enter => {
                    self.query = text.trim().to_owned();
                    self.queue_sel = 0;
                    self.run_query(None);
                    return true;
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            }
            self.query_prompt = Some(text);
            return true;
        }
        match (self.screen, code) {
            (Screen::Queue, KeyCode::Char('/')) => {
                self.query_prompt = Some(self.query.clone());
                self.status.clear();
            }
            (Screen::Queue, KeyCode::Char('n')) => {
                if let Some(last) = self.tasks.last().map(|t| t.id) {
                    self.run_query(Some(last));
                }
            }
            (Screen::Queue, KeyCode::Esc) if !self.query.is_empty() => {
                self.query.clear();
                self.run_query(None);
            }
            (Screen::Activity, KeyCode::Char('p')) => self.cycle_project(),
            (Screen::Activity, KeyCode::Char('f')) => self.cycle_kind(),
            (Screen::Activity, KeyCode::Char('c')) => {
                self.activity.project = None;
                self.activity.kind = None;
            }
            (Screen::Activity, KeyCode::Char('j') | KeyCode::Down) => {
                self.activity.scroll += 1;
            }
            (Screen::Activity, KeyCode::Char('k') | KeyCode::Up) => {
                self.activity.scroll = self.activity.scroll.saturating_sub(1);
            }
            _ => return false,
        }
        true
    }

    /// The first page of the query, or the page before `before`, appended.
    pub(crate) fn run_query(&mut self, before: Option<i64>) {
        let args = log_args(&self.query, before);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match self
            .forge
            .json(&refs)
            .and_then(|v| Ok(serde_json::from_value::<Vec<forge_client::TaskRow>>(v)?))
        {
            Ok(rows) => {
                if before.is_some() {
                    self.status = format!("{} more", rows.len());
                    self.tasks.extend(rows);
                } else {
                    self.tasks = rows;
                    self.status.clear();
                }
                self.queue_sel = self.queue_sel.min(self.tasks.len().saturating_sub(1));
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let a = &app.activity;
    let [strip, feed] = Layout::vertical([Constraint::Length(8), Constraint::Min(5)]).areas(area);

    let rows: Vec<Row> = a
        .running
        .iter()
        .map(|(task, r)| {
            let step = r
                .attempt
                .map_or("-".to_owned(), |(n, of)| format!("attempt {n} of {of}"));
            let num = |v: Option<i64>| v.map_or("-".to_owned(), |v| v.to_string());
            Row::new(vec![
                Cell::from(task.to_string()),
                Cell::from(step),
                Cell::from(num(r.turns)),
                Cell::from(r.calls.to_string()),
                Cell::from(r.cost.map_or("-".to_owned(), |c| format!("${c:.2}"))),
            ])
        })
        .collect();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("no attempts running")
                .block(Block::bordered().title("running attempts")),
            strip,
        );
    } else {
        let table = Table::new(
            rows,
            [
                Constraint::Length(6),
                Constraint::Length(18),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(8),
            ],
        )
        .header(
            Row::new(vec!["TASK", "STEP", "TURNS", "CALLS", "COST"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::bordered().title("running attempts"));
        frame.render_widget(table, strip);
    }

    let shown: Vec<&FeedRow> = a
        .feed
        .iter()
        .filter(|r| a.kind.is_none_or(|k| k == r.kind))
        .filter(|r| {
            a.project
                .as_deref()
                .is_none_or(|p| app.task_project(r.task) == Some(p))
        })
        .collect();
    let lines: Vec<Line> = shown
        .iter()
        .skip(a.scroll)
        .map(|r| {
            let color = match r.kind {
                "question" => Color::Yellow,
                "task done" | "pushed" => Color::Green,
                _ => Color::White,
            };
            Line::from(vec![
                Span::raw(format!("{}  #{:<4} ", time::fmt_time(r.ts), r.task)),
                Span::styled(format!("{:<16}", r.kind), Style::default().fg(color)),
                Span::raw(short(&r.text, 200)),
            ])
        })
        .collect();
    let title = format!(
        "activity  project: {}  kind: {}",
        a.project.as_deref().unwrap_or("all"),
        a.kind.unwrap_or("all")
    );
    let body = if lines.is_empty() {
        vec![Line::raw("no events yet")]
    } else {
        lines
    };
    frame.render_widget(
        Paragraph::new(body).block(Block::bordered().title(title)),
        feed,
    );
}
