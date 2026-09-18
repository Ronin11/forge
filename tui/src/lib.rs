//! forge-tui: the operator's seat. A client of the `forge` CLI and nothing
//! else: it takes a `snapshot`, subscribes to `events --follow` from the
//! offset the snapshot names, re-reads `log`, `requests`, and `trace` as
//! JSON only when an event says something changed, and acts through
//! `forge retry`. It never opens the database and never links the kernel,
//! so a kernel that changes its rules changes nothing here.
//!
//! `App` and `draw` are exposed as a library so `tui/tests/` can drive the
//! rendering against a fake `forge` binary and a synthetic key stream
//! without a real terminal: `main.rs` is a thin binary over this crate.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use forge_client::{
    Event, Forge, InitiativeRow, JobDoc, JobRow, Killer, RequestRow, TaskRow, TraceDoc, Worker,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// What a client keeps of the stream: the last events per task, as text.
const LIVE_PER_TASK: usize = 200;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Screen {
    Queue,
    Requests,
    Initiatives,
    Jobs,
    Task,
    JobView,
}

pub struct App {
    forge: Forge,
    screen: Screen,
    tasks: Vec<TaskRow>,
    requests: Vec<RequestRow>,
    initiatives: Vec<InitiativeRow>,
    jobs: Vec<JobRow>,
    trace: Option<TraceDoc>,
    job: Option<JobDoc>,
    queue_sel: usize,
    req_sel: usize,
    init_sel: usize,
    job_sel: usize,
    scroll: u16,
    status: String,
    refreshed: Instant,
    worker: Worker,
    live: HashMap<i64, VecDeque<String>>,
    recent: VecDeque<String>,
    sub: Option<(Killer, Receiver<Event>)>,
    dirty_lists: bool,
    dirty_trace: bool,
    dirty_jobs: bool,
    dirty_job: bool,
}

impl App {
    pub fn new(forge: Forge) -> App {
        App {
            forge,
            screen: Screen::Queue,
            tasks: Vec::new(),
            requests: Vec::new(),
            initiatives: Vec::new(),
            jobs: Vec::new(),
            trace: None,
            job: None,
            queue_sel: 0,
            req_sel: 0,
            init_sel: 0,
            job_sel: 0,
            scroll: 0,
            status: String::new(),
            refreshed: Instant::now() - Duration::from_secs(60),
            worker: Worker::default(),
            live: HashMap::new(),
            recent: VecDeque::new(),
            sub: None,
            dirty_lists: false,
            dirty_trace: false,
            dirty_jobs: false,
            dirty_job: false,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The whole state at one instant, and a subscription from that instant on.
    pub fn snapshot(&mut self) {
        match self.forge.snapshot() {
            Ok(s) => {
                self.tasks = s.tasks;
                self.requests = s.requests;
                self.worker = s.worker;
                if let Some((killer, _)) = self.sub.take() {
                    killer.kill();
                }
                match subscribe(&self.forge, s.events_offset) {
                    Ok(sub) => self.sub = Some(sub),
                    Err(e) => self.status = format!("{e:#}"),
                }
            }
            Err(e) => self.status = format!("{e:#}"),
        }
        self.load_initiatives();
        self.load_jobs();
        self.queue_sel = self.queue_sel.min(self.tasks.len().saturating_sub(1));
        self.req_sel = self.req_sel.min(self.requests.len().saturating_sub(1));
        self.refreshed = Instant::now();
    }

    /// One event from the stream: remembered as live text, and a flag for
    /// what it changed, so the lists and the trace are re-read only then.
    pub fn apply(&mut self, event: Event) {
        let job_id = match &event {
            Event::JobStarted { job_id, .. } | Event::JobFinished { job_id, .. } => Some(*job_id),
            _ => None,
        };
        let (task, kind, text) = match &event {
            Event::TaskStarted { task, text, .. } => (*task, "task_started", text.as_str()),
            Event::TaskQueued { task, text, .. } => (*task, "task_queued", text.as_str()),
            Event::AttemptStarted { task, text, .. } => (*task, "attempt_started", text.as_str()),
            Event::ToolCall { task, text, .. } => (*task, "tool_call", text.as_str()),
            Event::AgentDone { task, text, .. } => (*task, "agent_done", text.as_str()),
            Event::GitCounted { task, text, .. } => (*task, "git_counted", text.as_str()),
            Event::Check { task, text, .. } => (*task, "check", text.as_str()),
            Event::AttemptDone { task, text, .. } => (*task, "attempt_done", text.as_str()),
            Event::Pushed { task, text, .. } => (*task, "pushed", text.as_str()),
            Event::PushFailed { task, text, .. } => (*task, "push_failed", text.as_str()),
            Event::PushSkipped { task, text, .. } => (*task, "push_skipped", text.as_str()),
            Event::TaskDone { task, text, .. } => (*task, "task_done", text.as_str()),
            Event::Note { task, text, .. } => (*task, "note", text.as_str()),
            Event::Op { task, text, .. } => (*task, "op", text.as_str()),
            Event::JobStarted { task, text, .. } => (*task, "job_started", text.as_str()),
            Event::JobFinished { task, text, .. } => (*task, "job_finished", text.as_str()),
            Event::Other => return,
        };
        let line = format!("{kind:<15} {text}");
        let buf = self.live.entry(task).or_default();
        buf.push_back(line.clone());
        if buf.len() > LIVE_PER_TASK {
            buf.pop_front();
        }
        self.recent.push_back(format!("[{task}] {line}"));
        if self.recent.len() > 12 {
            self.recent.pop_front();
        }
        if matches!(
            event,
            Event::TaskStarted { .. }
                | Event::TaskDone { .. }
                | Event::AttemptStarted { .. }
                | Event::AttemptDone { .. }
                | Event::Op { .. }
                | Event::Pushed { .. }
                | Event::PushFailed { .. }
        ) {
            self.dirty_lists = true;
            if self.trace.as_ref().and_then(|t| t.task["id"].as_i64()) == Some(task) {
                self.dirty_trace = true;
            }
        }
        if let Some(id) = job_id {
            self.dirty_jobs = true;
            if self.job.as_ref().map(|j| j.id) == Some(id) {
                self.dirty_job = true;
            }
        }
    }

    /// Drain the stream, then re-read only what it said changed.
    pub fn pump(&mut self) {
        let mut events = Vec::new();
        if let Some((_, rx)) = &self.sub {
            while let Ok(e) = rx.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            self.apply(e);
        }
        if self.dirty_lists {
            self.dirty_lists = false;
            self.refresh();
        }
        if self.dirty_trace {
            self.dirty_trace = false;
            if let Some(id) = self.trace.as_ref().and_then(|t| t.task["id"].as_i64()) {
                self.open_task(id);
            }
        }
        if self.dirty_jobs {
            self.dirty_jobs = false;
            self.load_jobs();
        }
        if self.dirty_job {
            self.dirty_job = false;
            if let Some(id) = self.job.as_ref().map(|j| j.id) {
                self.open_job(id);
            }
        }
        if self.refreshed.elapsed() > Duration::from_secs(60) {
            self.snapshot();
        }
    }

    pub fn refresh(&mut self) {
        match self
            .forge
            .json(&["log", "--json", "--limit", "60"])
            .and_then(|v| Ok(serde_json::from_value::<Vec<TaskRow>>(v)?))
        {
            Ok(rows) => self.tasks = rows,
            Err(e) => self.status = format!("{e:#}"),
        }
        match self
            .forge
            .json(&["requests", "--json"])
            .and_then(|v| Ok(serde_json::from_value::<Vec<RequestRow>>(v)?))
        {
            Ok(rows) => self.requests = rows,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.load_initiatives();
        self.load_jobs();
        self.queue_sel = self.queue_sel.min(self.tasks.len().saturating_sub(1));
        self.req_sel = self.req_sel.min(self.requests.len().saturating_sub(1));
        self.refreshed = Instant::now();
    }

    /// `forge initiative list --json`, across every project: the same call
    /// on `snapshot()` and on `refresh()`, since `forge snapshot` doesn't
    /// carry initiatives.
    fn load_initiatives(&mut self) {
        match self.forge.initiative_list(None) {
            Ok(rows) => self.initiatives = rows,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.init_sel = self.init_sel.min(self.initiatives.len().saturating_sub(1));
    }

    /// `forge job list --json`, across every project: the same call on
    /// `snapshot()` and on `refresh()`, and again whenever a `job_started`
    /// or `job_finished` event says the jobs list changed.
    fn load_jobs(&mut self) {
        match self.forge.job_list(None) {
            Ok(rows) => self.jobs = rows,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.job_sel = self.job_sel.min(self.jobs.len().saturating_sub(1));
    }

    pub fn open_task(&mut self, id: i64) {
        match self
            .forge
            .json(&["trace", &id.to_string(), "--json"])
            .and_then(|v| Ok(serde_json::from_value::<TraceDoc>(v)?))
        {
            Ok(doc) => {
                self.trace = Some(doc);
                self.screen = Screen::Task;
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    pub fn open_job(&mut self, id: i64) {
        match self.forge.job_show(id) {
            Ok(doc) => {
                self.job = Some(doc);
                self.screen = Screen::JobView;
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// The task the cursor is on, whichever screen shows it.
    pub fn current_id(&self) -> Option<i64> {
        match self.screen {
            Screen::Queue => self.tasks.get(self.queue_sel).map(|t| t.id),
            Screen::Requests => self.requests.get(self.req_sel).map(|r| r.id),
            Screen::Initiatives => None,
            Screen::Jobs => None,
            Screen::Task => self.trace.as_ref().and_then(|t| t.task["id"].as_i64()),
            Screen::JobView => None,
        }
    }

    pub fn retry(&mut self, chain: bool) {
        let Some(id) = self.current_id() else {
            return;
        };
        let id_s = id.to_string();
        let mut args = vec!["retry", id_s.as_str()];
        if chain {
            args.push("--chain");
        }
        match self.forge.run(&args) {
            Ok(out) => {
                self.status = out.lines().collect::<Vec<_>>().join("; ");
                self.refresh();
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    pub fn down(&mut self) {
        match self.screen {
            Screen::Queue => {
                self.queue_sel = (self.queue_sel + 1).min(self.tasks.len().saturating_sub(1))
            }
            Screen::Requests => {
                self.req_sel = (self.req_sel + 1).min(self.requests.len().saturating_sub(1))
            }
            Screen::Initiatives => {
                self.init_sel = (self.init_sel + 1).min(self.initiatives.len().saturating_sub(1))
            }
            Screen::Jobs => {
                self.job_sel = (self.job_sel + 1).min(self.jobs.len().saturating_sub(1))
            }
            Screen::Task | Screen::JobView => self.scroll = self.scroll.saturating_add(1),
        }
    }

    pub fn up(&mut self) {
        match self.screen {
            Screen::Queue => self.queue_sel = self.queue_sel.saturating_sub(1),
            Screen::Requests => self.req_sel = self.req_sel.saturating_sub(1),
            Screen::Initiatives => self.init_sel = self.init_sel.saturating_sub(1),
            Screen::Jobs => self.job_sel = self.job_sel.saturating_sub(1),
            Screen::Task | Screen::JobView => self.scroll = self.scroll.saturating_sub(1),
        }
    }

    /// One key: `true` when it should end the program. The same dispatch
    /// `run()`'s event loop uses, exposed so a test can drive it with a
    /// synthetic `KeyCode` and no terminal at all.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char('j') | KeyCode::Down => self.down(),
            KeyCode::Char('k') | KeyCode::Up => self.up(),
            KeyCode::Char('g') => self.snapshot(),
            KeyCode::Char('r') => self.retry(false),
            KeyCode::Char('R') => self.retry(true),
            KeyCode::Tab => {
                self.screen = match self.screen {
                    Screen::Queue => Screen::Requests,
                    Screen::Requests => Screen::Initiatives,
                    Screen::Initiatives => Screen::Jobs,
                    _ => Screen::Queue,
                }
            }
            KeyCode::Enter => match self.screen {
                Screen::Jobs => {
                    if let Some(id) = self.jobs.get(self.job_sel).map(|j| j.id) {
                        self.scroll = 0;
                        self.open_job(id);
                    }
                }
                Screen::Task | Screen::JobView => {}
                _ => {
                    if let Some(id) = self.current_id() {
                        self.scroll = 0;
                        self.open_task(id);
                    }
                }
            },
            KeyCode::Esc => match self.screen {
                Screen::Task => self.screen = Screen::Queue,
                Screen::JobView => self.screen = Screen::Jobs,
                _ => {}
            },
            _ => {}
        }
        false
    }

    /// Kills the event subscription, if one is running. Every exit path —
    /// the real `run()` loop, `--dump`, and a test that built an `App` —
    /// calls this so no `forge events --follow` child outlives it.
    pub fn shutdown(&mut self) {
        if let Some((killer, _)) = self.sub.take() {
            killer.kill();
        }
    }
}

/// `forge.subscribe(offset)`, drained on a background thread so `pump` can
/// read it without blocking; the `Killer` lets the App tear the
/// subordinate process down promptly even while that thread is blocked
/// waiting on the next line.
fn subscribe(forge: &Forge, offset: u64) -> Result<(Killer, Receiver<Event>)> {
    let sub = forge.subscribe(offset)?;
    let killer = sub.killer();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for event in sub {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    Ok((killer, rx))
}

fn state_style(state: &str) -> Style {
    let color = match state {
        "succeeded" | "ok" => Color::Green,
        "running" => Color::Cyan,
        "queued" | "scheduled" => Color::Gray,
        "blocked" | "needs_human" => Color::Yellow,
        "failed" => Color::Red,
        "unverified" => Color::Magenta,
        "withdrawn" | "dropped" => Color::DarkGray,
        _ => Color::White,
    };
    Style::default().fg(color)
}

pub fn draw(frame: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let queued = app.tasks.iter().filter(|t| t.state == "queued").count();
    let running = app.tasks.iter().filter(|t| t.state == "running").count();
    let tab = |name: &str, s: Screen| {
        if app.screen == s {
            Span::styled(
                format!(" {name} "),
                Style::default().add_modifier(Modifier::REVERSED),
            )
        } else {
            Span::raw(format!(" {name} "))
        }
    };
    let worker = if app.worker.running && app.worker.stale_binary {
        Span::styled(
            "worker: stale binary, restart it",
            Style::default().fg(Color::Yellow),
        )
    } else if app.worker.running {
        Span::styled(
            format!("worker {}", app.worker.pid),
            Style::default().fg(Color::Green),
        )
    } else {
        Span::styled("no worker", Style::default().fg(Color::Red))
    };
    let header = Line::from(vec![
        Span::styled("Forge 2", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {queued} queued, {running} running  ")),
        worker,
        Span::raw("  "),
        tab("queue", Screen::Queue),
        tab(
            &format!("requests ({})", app.requests.len()),
            Screen::Requests,
        ),
        tab(
            &format!("initiatives ({})", app.initiatives.len()),
            Screen::Initiatives,
        ),
        tab(&format!("jobs ({})", app.jobs.len()), Screen::Jobs),
        tab("task", Screen::Task),
        tab("job", Screen::JobView),
    ]);
    frame.render_widget(Paragraph::new(header), head);
    match app.screen {
        Screen::Queue => draw_queue(frame, app, body),
        Screen::Requests => draw_requests(frame, app, body),
        Screen::Initiatives => draw_initiatives(frame, app, body),
        Screen::Jobs => draw_jobs(frame, app, body),
        Screen::Task => draw_task(frame, app, body),
        Screen::JobView => draw_job(frame, app, body),
    }
    let keys = match app.screen {
        Screen::Task | Screen::JobView => "j/k scroll  Esc back  r retry  R retry chain  q quit",
        _ => "j/k move  Enter open  Tab switch  r retry  R retry chain  g refresh  q quit",
    };
    let foot_line = if app.status.is_empty() {
        Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))
    } else {
        Line::from(vec![
            Span::styled(app.status.clone(), Style::default().fg(Color::Yellow)),
            Span::styled(format!("   {keys}"), Style::default().fg(Color::DarkGray)),
        ])
    };
    frame.render_widget(Paragraph::new(foot_line), foot);
}

fn short(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    s.chars().take(n).collect()
}

fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let [area, live] = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).areas(area);
    let recent: Vec<Line> = app.recent.iter().map(|l| Line::raw(l.clone())).collect();
    frame.render_widget(
        Paragraph::new(recent).block(Block::bordered().title("live")),
        live,
    );
    let rows = app.tasks.iter().map(|t| {
        Row::new(vec![
            Cell::from(t.id.to_string()),
            Cell::from(Span::styled(t.state.clone(), state_style(&t.state))),
            Cell::from(t.workflow.clone()),
            Cell::from(t.attempts.to_string()),
            Cell::from(format!("${:.2}", t.cost_usd)),
            Cell::from(t.project.clone().unwrap_or_default()),
            Cell::from(short(&t.task, 200)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(11),
            Constraint::Length(13),
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(12),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["ID", "STATE", "WF", "ATT", "COST", "PROJECT", "TASK"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title("tasks, newest first"));
    let mut st = TableState::default();
    st.select((!app.tasks.is_empty()).then_some(app.queue_sel));
    frame.render_stateful_widget(table, area, &mut st);
}

fn draw_requests(frame: &mut Frame, app: &App, area: Rect) {
    let [list, detail] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let rows = app.requests.iter().map(|r| {
        Row::new(vec![
            Cell::from(r.id.to_string()),
            Cell::from(r.kind.clone()),
            Cell::from(r.workflow.clone()),
            Cell::from(short(&r.text, 200)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(11),
            Constraint::Length(13),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["ID", "KIND", "WF", "REQUEST"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title("blocked tasks: decisions waiting on you"));
    let mut st = TableState::default();
    st.select((!app.requests.is_empty()).then_some(app.req_sel));
    frame.render_stateful_widget(table, list, &mut st);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(r) = app.requests.get(app.req_sel) {
        lines.push(Line::from(Span::styled(
            format!("task {}  {}", r.id, r.task),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            r.text.clone(),
            Style::default().fg(Color::Yellow),
        )));
        if !r.path.is_empty() {
            lines.push(Line::raw(format!("path: {}", r.path)));
        }
        if !r.tried.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::raw(format!("did: {}", r.tried)));
        }
    } else {
        lines.push(Line::raw("nothing is waiting on you"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title("request")),
        detail,
    );
}

fn draw_initiatives(frame: &mut Frame, app: &App, area: Rect) {
    let rows = app.initiatives.iter().map(|i| {
        Row::new(vec![
            Cell::from(i.id.to_string()),
            Cell::from(Span::styled(i.state.clone(), state_style(&i.state))),
            Cell::from(i.project.clone()),
            Cell::from(i.queued.to_string()),
            Cell::from(i.running.to_string()),
            Cell::from(i.succeeded.to_string()),
            Cell::from(i.failed.to_string()),
            Cell::from(format!("${:.2}", i.cost_usd)),
            Cell::from(short(&i.outcome, 200)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(11),
            Constraint::Length(12),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(7),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec![
            "ID", "STATE", "PROJECT", "Q", "R", "S", "F", "COST", "OUTCOME",
        ])
        .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title("initiatives"));
    let mut st = TableState::default();
    st.select((!app.initiatives.is_empty()).then_some(app.init_sel));
    frame.render_stateful_widget(table, area, &mut st);
}

fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let rows = app.jobs.iter().map(|j| {
        Row::new(vec![
            Cell::from(j.id.to_string()),
            Cell::from(j.project.clone()),
            Cell::from(j.workflow.clone()),
            Cell::from(Span::styled(j.state.clone(), state_style(&j.state))),
            Cell::from(format!("${:.2}", j.cost_usd.unwrap_or(0.0))),
            Cell::from(j.started_at.to_string()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(16),
            Constraint::Length(11),
            Constraint::Length(7),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec![
            "ID", "PROJECT", "WORKFLOW", "STATE", "COST", "STARTED",
        ])
        .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title("jobs, newest first"));
    let mut st = TableState::default();
    st.select((!app.jobs.is_empty()).then_some(app.job_sel));
    frame.render_stateful_widget(table, area, &mut st);
}

fn draw_job(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(j) = &app.job {
        lines.push(Line::from(vec![
            Span::styled(
                format!("job {}  ", j.id),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(j.state.clone(), state_style(&j.state)),
            Span::raw(format!(
                "  {}  {}{}",
                j.project,
                j.workflow,
                if j.dry_run { "  (dry run)" } else { "" }
            )),
        ]));
        lines.push(Line::raw(format!(
            "trigger {} {}   source {}",
            j.trigger_kind, j.trigger_ref, j.workflow_source
        )));
        lines.push(Line::raw(format!(
            "started {}   finished {}   cost ${:.2}",
            j.started_at,
            j.finished_at.map_or("-".to_string(), |f| f.to_string()),
            j.cost_usd.unwrap_or(0.0)
        )));
        if let Some(due) = j.due_at {
            lines.push(Line::raw(format!("due {due}")));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "steps",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for s in &j.steps {
            lines.push(Line::raw(format!(
                "  {:>2} {:<20} {:<10} {}",
                s.seq,
                s.action,
                s.kind,
                s.cost_usd.map_or(String::new(), |c| format!("${c:.2}"))
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "effects",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for e in &j.effects {
            lines.push(Line::raw(format!(
                "  {:<10} {} {}",
                e.kind, e.target, e.summary
            )));
        }
    } else {
        lines.push(Line::raw("no job open; pick one in jobs and press Enter"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(Block::bordered().title("job")),
        area,
    );
}

fn draw_task(frame: &mut Frame, app: &App, area: Rect) {
    let [area, live] =
        Layout::vertical([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(area);
    let id = app
        .trace
        .as_ref()
        .and_then(|t| t.task["id"].as_i64())
        .unwrap_or(-1);
    let buf = app.live.get(&id);
    let shown = live.height.saturating_sub(2) as usize;
    let recent: Vec<Line> = buf
        .map(|b| {
            b.iter()
                .rev()
                .take(shown)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|l| Line::raw(l.clone()))
                .collect()
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(recent).block(Block::bordered().title("live")),
        live,
    );
    let mut lines: Vec<Line> = Vec::new();
    if let Some(tr) = &app.trace {
        let t = &tr.task;
        let state = t["state"].as_str().unwrap_or("");
        lines.push(Line::from(vec![
            Span::styled(
                format!("task {}  ", t["id"]),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(state.to_string(), state_style(state)),
            Span::raw(format!(
                "  {}  {}",
                t["workflow"].as_str().unwrap_or(""),
                t["reason"].as_str().unwrap_or("")
            )),
        ]));
        lines.push(Line::raw(format!(
            "branch {}   base {}",
            t["branch"].as_str().unwrap_or(""),
            &t["base_sha"].as_str().unwrap_or("")
                [..8.min(t["base_sha"].as_str().unwrap_or("").len())]
        )));
        if !t["project"].is_null() || !t["initiative"].is_null() {
            lines.push(Line::raw(format!(
                "project {}   initiative {}",
                t["project"].as_str().unwrap_or("-"),
                t["initiative"]
                    .as_i64()
                    .map_or("-".to_string(), |i| i.to_string()),
            )));
        }
        for (k, v) in [("after", &t["after"]), ("retry of", &t["retry_of"])] {
            if !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()) {
                lines.push(Line::raw(format!("{k} {v}")));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(t["text"].as_str().unwrap_or("").to_string()));
        if let Some(j) = t["journal"].as_str() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "journal",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for l in j.lines().skip(1) {
                lines.push(Line::raw(l.to_string()));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "attempts",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for a in &tr.attempts {
            let st = a.state.as_str();
            lines.push(Line::from(vec![
                Span::raw(format!("  {:>2} {:<9} ", a.attempt_no, a.step)),
                Span::styled(format!("{st:<13}"), state_style(st)),
                Span::raw(format!(
                    " {:>3} turns  ${:.2}  {}",
                    a.num_turns,
                    a.cost_usd.unwrap_or(0.0),
                    a.reason
                )),
            ]));
            if let Some(checks) = a.verdict.as_array() {
                for c in checks.iter().filter(|c| c["ok"] == false) {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "       ✗ {} {}: {}",
                            c["level"].as_str().unwrap_or(""),
                            c["name"].as_str().unwrap_or(""),
                            c["tail"]
                                .as_str()
                                .unwrap_or("")
                                .lines()
                                .next()
                                .unwrap_or("")
                        ),
                        Style::default().fg(Color::Red),
                    )));
                }
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "operations",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for o in &tr.ops {
            lines.push(Line::from(vec![
                Span::styled(
                    if o.ok { "  ✓ " } else { "  ✗ " }.to_string(),
                    Style::default().fg(if o.ok { Color::Green } else { Color::Red }),
                ),
                Span::raw(format!(
                    "{:<11} {}",
                    o.name,
                    o.detail.lines().next().unwrap_or("")
                )),
            ]));
        }
        if !tr.deploys.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "deploys",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for d in &tr.deploys {
                let (mark, color) = match d.check_ok {
                    Some(true) => ("✓", Color::Green),
                    Some(false) => ("✗", Color::Red),
                    None => ("…", Color::Gray),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {mark} "), Style::default().fg(color)),
                    Span::raw(format!(
                        "{} -> {}  {}",
                        d.project,
                        d.target,
                        &d.sha[..8.min(d.sha.len())]
                    )),
                ]));
                if let Some(to) = &d.rolled_back_to {
                    lines.push(Line::raw(format!("       rolled back to {to}")));
                }
                if !d.reason.is_empty() {
                    lines.push(Line::raw(format!("       {}", d.reason)));
                }
            }
        }
        if let Some(a) = &tr.assessment {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "assessment",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::raw(format!(
                "  score {}   {} {}   ${:.2}",
                a.score,
                a.provider,
                a.model,
                a.cost_usd.unwrap_or(0.0)
            )));
            for f in &a.findings {
                lines.push(Line::raw(format!(
                    "  {:<8} {}: {}",
                    f.severity, f.path, f.finding
                )));
            }
        }
        if let Some(d) = tr.diagnosis.as_array().filter(|d| !d.is_empty()) {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "diagnosis",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for x in d {
                lines.push(Line::from(Span::styled(
                    format!("  what   {}", x["what"].as_str().unwrap_or("")),
                    Style::default().fg(Color::Yellow),
                )));
                lines.push(Line::raw(format!(
                    "  action {}",
                    x["action"].as_str().unwrap_or("")
                )));
            }
        }
    } else {
        lines.push(Line::raw(
            "no task open; pick one in the queue and press Enter",
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(Block::bordered().title("trace")),
        area,
    );
}

pub fn run() -> Result<()> {
    let mut app = App::new(Forge::new());
    app.snapshot();
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw(f, &app))?;
            if crossterm::event::poll(Duration::from_millis(250))?
                && let crossterm::event::Event::Key(k) = crossterm::event::read()?
                && k.kind == crossterm::event::KeyEventKind::Press
                && app.handle_key(k.code, k.modifiers)
            {
                break;
            }
            app.pump();
        }
        Ok(())
    })();
    ratatui::restore();
    app.shutdown();
    result
}

/// The backend's buffer as lines of text.
pub fn render_text(backend: &ratatui::backend::TestBackend) -> String {
    let buf = backend.buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(tasks: &str, requests: &str) -> App {
        let mut app = App::new(Forge {
            bin: "/nonexistent".into(),
        });
        app.tasks = serde_json::from_str(tasks).unwrap();
        app.requests = serde_json::from_str(requests).unwrap();
        app
    }

    fn frame_of(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(100, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        render_text(terminal.backend())
    }

    #[test]
    fn the_queue_lists_tasks_with_their_state_and_counts_them() {
        let app = app_with(
            r#"[{"id":7,"state":"running","workflow":"tdd","attempts":1,"cost_usd":0.5,"task":"stars tier","project":"forge"},
                {"id":6,"state":"queued","workflow":"direct","attempts":0,"cost_usd":0,"task":"docs"}]"#,
            "[]",
        );
        let text = frame_of(&app);
        assert!(text.contains("1 queued, 1 running"), "{text}");
        assert!(text.contains("7     running"), "{text}");
        assert!(text.contains("stars tier"), "{text}");
        assert!(text.contains("PROJECT"), "{text}");
        assert!(text.contains("forge"), "{text}");
        assert!(text.contains("Enter open"), "{text}");
    }

    #[test]
    fn requests_show_the_question_what_was_tried_and_the_path() {
        let mut app = app_with(
            "[]",
            r#"[{"id":42,"kind":"suite","text":"stars-tier asserts stars never age","tried":"ran the suite","path":"tests/acceptance/stars-tier.test.ts","workflow":"tdd","repo":"/r","task":"stellar lifetime"}]"#,
        );
        app.screen = Screen::Requests;
        let text = frame_of(&app);
        assert!(text.contains("suite"), "{text}");
        assert!(text.contains("did: ran the suite"), "{text}");
        assert!(
            text.contains("path: tests/acceptance/stars-tier.test.ts"),
            "{text}"
        );
        assert_eq!(app.current_id(), Some(42));
    }

    #[test]
    fn an_event_is_kept_as_live_text_and_flags_what_it_changed() {
        let mut app = app_with("[]", "[]");
        app.trace = Some(serde_json::from_value(serde_json::json!({"task": {"id": 5}})).unwrap());
        app.apply(
            serde_json::from_str(
                r#"{"ts":1,"task":5,"type":"tool_call","name":"Bash","text":"tool Bash"}"#,
            )
            .unwrap(),
        );
        assert!(
            !app.dirty_lists && !app.dirty_trace,
            "a tool call changes no list"
        );
        assert_eq!(app.live[&5].len(), 1);
        app.apply(
            serde_json::from_str(
                r#"{"ts":2,"task":5,"type":"attempt_done","state":"succeeded","reason":"","text":"attempt succeeded"}"#,
            )
            .unwrap(),
        );
        assert!(
            app.dirty_lists && app.dirty_trace,
            "an attempt ending changes the lists and the open trace"
        );
        assert_eq!(app.recent.len(), 2);
        app.screen = Screen::Task;
        let text = frame_of(&app);
        assert!(text.contains("tool_call       tool Bash"), "{text}");
    }

    #[test]
    fn a_trace_renders_attempts_operations_and_the_diagnosis() {
        let mut app = app_with("[]", "[]");
        app.trace = Some(serde_json::from_value(serde_json::json!({
            "task": {"id": 3, "state": "failed", "workflow": "tdd", "reason": "L1 failed: test (after 2 attempt(s))", "branch": "forge/3-x", "base_sha": "abcdef1234567890", "text": "do the thing", "after": [], "retry_of": null, "project": "forge", "initiative": 9},
            "attempts": [{"attempt_no": 1, "step": "code", "state": "checks_failed", "num_turns": 12, "cost_usd": 0.4, "reason": "L1 failed: test", "verdict": [{"level": "L1", "name": "test", "ok": false, "tail": "FAIL x\nmore"}]}],
            "ops": [{"name": "clone", "ok": true, "detail": "abc"}, {"name": "verify", "ok": false, "detail": "L1 failed: test"}],
            "diagnosis": [{"what": "the repo's test check fails", "action": "read the failing tests"}]
        })).unwrap());
        app.screen = Screen::Task;
        let text = frame_of(&app);
        assert!(text.contains("task 3  failed"), "{text}");
        assert!(text.contains("✗ L1 test: FAIL x"), "{text}");
        assert!(text.contains("✗ verify"), "{text}");
        assert!(text.contains("action read the failing tests"), "{text}");
        assert!(text.contains("project forge   initiative 9"), "{text}");
    }

    #[test]
    fn a_trace_renders_a_deploy_and_an_assessment() {
        let mut app = app_with("[]", "[]");
        app.trace = Some(serde_json::from_value(serde_json::json!({
            "task": {"id": 4, "state": "succeeded", "workflow": "tdd", "reason": "", "branch": "forge/4-x", "base_sha": "abcdef1234567890", "text": "ship it", "after": [], "retry_of": null},
            "attempts": [],
            "ops": [],
            "deploys": [{"id": 1, "project": "forge", "target": "prod", "sha": "abcdef1234567890", "started_at": 1, "finished_at": 2, "check_ok": true, "check_output": "ok", "rolled_back_to": null, "reason": ""}],
            "assessment": {"score": 82, "findings": [{"path": "tui/src/lib.rs", "finding": "missing coverage", "severity": "minor"}], "model": "claude-sonnet-5", "provider": "anthropic", "cost_usd": 0.05, "created_at": 1}
        })).unwrap());
        app.screen = Screen::Task;
        let text = frame_of(&app);
        assert!(text.contains("deploys"), "{text}");
        assert!(text.contains("forge -> prod"), "{text}");
        assert!(text.contains("assessment"), "{text}");
        assert!(text.contains("score 82"), "{text}");
        assert!(text.contains("minor"), "{text}");
    }

    #[test]
    fn tab_cycles_queue_requests_initiatives_jobs_and_back() {
        let mut app = app_with("[]", "[]");
        assert_eq!(app.screen, Screen::Queue);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Requests);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Initiatives);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Jobs);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Queue);
    }

    #[test]
    fn job_events_flag_the_jobs_list_and_the_open_job_as_dirty() {
        let mut app = app_with("[]", "[]");
        app.job = Some(serde_json::from_value(serde_json::json!({"id": 9})).unwrap());
        app.apply(
            serde_json::from_str(
                r#"{"ts":1,"task":0,"type":"job_started","project":"forge","workflow":"nightly","job_id":9,"dry_run":false,"text":"running forge/nightly (job 9)"}"#,
            )
            .unwrap(),
        );
        assert!(
            app.dirty_jobs && app.dirty_job,
            "a job event for the open job dirties both the list and the open job"
        );
        app.dirty_jobs = false;
        app.dirty_job = false;
        app.apply(
            serde_json::from_str(
                r#"{"ts":2,"task":0,"type":"job_finished","project":"forge","workflow":"nightly","job_id":42,"state":"ok","cost_usd":0.1,"text":"job 42 (forge/nightly) ok ($0.10)"}"#,
            )
            .unwrap(),
        );
        assert!(
            app.dirty_jobs && !app.dirty_job,
            "a different job's event dirties the list but leaves the open job clean"
        );
    }
}
