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
    Event, Forge, InitiativeDoc, InitiativeRow, JobDoc, JobRow, Killer, RequestRow, StatsDoc,
    TaskRow, TraceDoc, Worker,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub mod activity;
pub mod ops;
pub mod stats;
pub mod time;

/// What a client keeps of the stream: the last events per task, as text.
const LIVE_PER_TASK: usize = 200;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Screen {
    Queue,
    Requests,
    Initiatives,
    Jobs,
    Stats,
    Deploys,
    Doctor,
    Activity,
    Task,
    JobView,
    Initiative,
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
    initiative: Option<InitiativeDoc>,
    stats: Option<StatsDoc>,
    stats_tab: usize,
    stats_sort: Option<(&'static str, bool)>,
    ops: ops::Ops,
    activity: activity::Activity,
    query: String,
    query_prompt: Option<String>,
    /// A budget (`true`) or stop-after (`false`) being typed for an initiative.
    limit_prompt: Option<(i64, bool, String)>,
    queue_sel: usize,
    req_sel: usize,
    init_sel: usize,
    job_sel: usize,
    scroll: u16,
    status: String,
    prompt: Option<(i64, bool, String)>,
    summaries: HashMap<i64, String>,
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
            initiative: None,
            stats: None,
            stats_tab: 0,
            stats_sort: None,
            ops: ops::Ops::default(),
            activity: activity::Activity::default(),
            query: String::new(),
            query_prompt: None,
            limit_prompt: None,
            queue_sel: 0,
            req_sel: 0,
            init_sel: 0,
            job_sel: 0,
            scroll: 0,
            status: String::new(),
            prompt: None,
            summaries: HashMap::new(),
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
        if !self.query.is_empty() {
            self.run_query(None);
        }
        self.seed_running();
        self.load_inbox();
        self.load_initiatives();
        self.load_jobs();
        self.reload_initiative();
        if self.screen == Screen::Stats {
            self.load_stats();
        }
        self.load_ops();
        self.queue_sel = self.queue_sel.min(self.tasks.len().saturating_sub(1));
        self.req_sel = self.req_sel.min(self.requests.len().saturating_sub(1));
        self.refreshed = Instant::now();
    }

    /// One event from the stream: remembered as live text, and a flag for
    /// what it changed, so the lists and the trace are re-read only then.
    pub fn apply(&mut self, event: Event) {
        self.activity.record(&event);
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
            Event::Other => {
                // Includes task_blocked from newer CLI versions.
                self.dirty_lists = true;
                return;
            }
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
            Event::TaskQueued { .. }
                | Event::TaskStarted { .. }
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
        self.run_query(None);
        match self
            .forge
            .json(&["requests", "--json"])
            .and_then(|v| Ok(serde_json::from_value::<Vec<RequestRow>>(v)?))
        {
            Ok(rows) => self.requests = rows,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.load_inbox();
        self.load_initiatives();
        self.load_jobs();
        self.reload_initiative();
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

    /// `forge stats --json`, read when the stats screen opens and on `g`.
    fn load_stats(&mut self) {
        match self.forge.stats() {
            Ok(doc) => {
                let tabs = stats::visible_tabs(&doc).len();
                self.stats_tab = self.stats_tab.min(tabs - 1);
                self.stats = Some(doc);
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    fn stats_tab_step(&mut self, forward: bool) {
        let Some(doc) = &self.stats else { return };
        let n = stats::visible_tabs(doc).len();
        self.stats_tab = if forward {
            (self.stats_tab + 1) % n
        } else {
            (self.stats_tab + n - 1) % n
        };
        self.stats_sort = None;
        self.scroll = 0;
    }

    /// Moves the sort key along the tab's columns, none at both ends.
    fn stats_sort_step(&mut self, forward: bool) {
        let Some(doc) = &self.stats else { return };
        let tabs = stats::visible_tabs(doc);
        let tab = tabs[self.stats_tab.min(tabs.len() - 1)];
        let keys = stats::sort_keys(doc, tab);
        let at = self
            .stats_sort
            .and_then(|(k, _)| keys.iter().position(|&x| x == k));
        let next = match (at, forward) {
            (None, true) => Some(0),
            (None, false) => keys.len().checked_sub(1),
            (Some(i), true) => (i + 1 < keys.len()).then_some(i + 1),
            (Some(i), false) => i.checked_sub(1),
        };
        self.stats_sort = next.map(|i| (keys[i], stats::starts_descending(doc, tab, keys[i])));
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

    fn reload_initiative(&mut self) {
        if let Some(id) = self.initiative.as_ref().map(|doc| doc.id) {
            match self.forge.initiative_report(id) {
                Ok(doc) => self.initiative = Some(doc),
                Err(e) => self.status = format!("{e:#}"),
            }
        }
    }

    pub fn open_initiative(&mut self, id: i64) {
        match self.forge.initiative_report(id) {
            Ok(doc) => {
                self.initiative = Some(doc);
                self.screen = Screen::Initiative;
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// Raises the open initiative's budget or stop-after through
    /// `forge initiative set`, then re-reads the report.
    fn set_limit(&mut self, id: i64, budget: bool, text: &str) -> bool {
        let text = text.trim();
        let valid = if budget {
            text.parse::<f64>().is_ok_and(|v| v.is_finite() && v > 0.0)
        } else {
            text.parse::<u32>().is_ok_and(|v| v > 0)
        };
        if !valid {
            self.status = format!(
                "not a valid {}: {text}",
                if budget { "budget" } else { "stop-after" }
            );
            return false;
        }
        let id_s = id.to_string();
        let flag = if budget { "--budget" } else { "--stop-after" };
        match self.forge.run(&["initiative", "set", &id_s, flag, text]) {
            Ok(_) => {
                self.status = format!("initiative {id} updated");
                self.open_initiative(id);
                self.load_initiatives();
                true
            }
            Err(e) => {
                self.status = format!("{e:#}");
                false
            }
        }
    }

    /// The task the cursor is on, whichever screen shows it.
    pub fn current_id(&self) -> Option<i64> {
        match self.screen {
            Screen::Queue => self.tasks.get(self.queue_sel).map(|t| t.id),
            Screen::Requests => self.requests.get(self.req_sel).map(|r| r.id),
            Screen::Initiatives => None,
            Screen::Jobs | Screen::Stats | Screen::Deploys | Screen::Doctor | Screen::Activity => {
                None
            }
            Screen::Task => self.trace.as_ref().and_then(|t| t.task["id"].as_i64()),
            Screen::JobView | Screen::Initiative => None,
        }
    }

    fn load_inbox(&mut self) {
        self.requests.retain(|r| r.kind != "unverified");
        let result = (|| -> Result<Vec<TaskRow>> {
            let mut rows = Vec::new();
            let mut before = String::new();
            loop {
                let mut args = vec!["log", "--json", "--state", "unverified", "--limit", "100"];
                if !before.is_empty() {
                    args.extend(["--before", before.as_str()]);
                }
                let page: Vec<TaskRow> = serde_json::from_value(self.forge.json(&args)?)?;
                let count = page.len();
                if let Some(last) = page.last() {
                    before = last.id.to_string();
                }
                rows.extend(page);
                if count < 100 {
                    break;
                }
            }
            Ok(rows)
        })();
        match result {
            Ok(rows) => self.requests.extend(rows.into_iter().map(|t| RequestRow {
                id: t.id,
                kind: "unverified".into(),
                task: t.task,
                workflow: t.workflow,
                text: "Verified work waiting to land".into(),
                ..RequestRow::default()
            })),
            Err(e) => self.status = format!("{e:#}"),
        }
        self.summaries.clear();
        for request in &self.requests {
            match self
                .forge
                .json(&["trace", &request.id.to_string(), "--json"])
            {
                Ok(trace) => {
                    if let Some(last) = trace["attempts"].as_array().and_then(|a| a.last()) {
                        let summary = last["outputs"]["summary"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .or_else(|| last["reason"].as_str())
                            .unwrap_or("");
                        self.summaries.insert(request.id, summary.to_owned());
                    }
                }
                Err(e) => {
                    self.status = format!("{e:#}");
                }
            }
        }
    }

    fn inbox_action(&mut self, id: i64, answer: bool, text: &str) -> bool {
        let id = id.to_string();
        let args = if answer {
            vec!["answer", id.as_str(), text]
        } else {
            vec!["withdraw", id.as_str(), "--reason", text]
        };
        match self.forge.run(&args) {
            Ok(out) => {
                self.status = out.trim().to_owned();
                self.refresh();
                true
            }
            Err(e) => {
                self.status = format!("{e:#}");
                false
            }
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
            Screen::Task
            | Screen::JobView
            | Screen::Initiative
            | Screen::Stats
            | Screen::Deploys
            | Screen::Doctor
            | Screen::Activity => self.scroll = self.scroll.saturating_add(1),
        }
    }

    pub fn up(&mut self) {
        match self.screen {
            Screen::Queue => self.queue_sel = self.queue_sel.saturating_sub(1),
            Screen::Requests => self.req_sel = self.req_sel.saturating_sub(1),
            Screen::Initiatives => self.init_sel = self.init_sel.saturating_sub(1),
            Screen::Jobs => self.job_sel = self.job_sel.saturating_sub(1),
            Screen::Task
            | Screen::JobView
            | Screen::Initiative
            | Screen::Stats
            | Screen::Deploys
            | Screen::Doctor
            | Screen::Activity => self.scroll = self.scroll.saturating_sub(1),
        }
    }

    /// One key: `true` when it should end the program. The same dispatch
    /// `run()`'s event loop uses, exposed so a test can drive it with a
    /// synthetic `KeyCode` and no terminal at all.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if let Some((id, budget, mut text)) = self.limit_prompt.take() {
            match code {
                KeyCode::Esc => return false,
                KeyCode::Enter => {
                    if self.set_limit(id, budget, &text) {
                        return false;
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            }
            self.limit_prompt = Some((id, budget, text));
            return false;
        }
        if let Some((id, answer, mut text)) = self.prompt.take() {
            match code {
                KeyCode::Esc => return false,
                KeyCode::Enter if !text.trim().is_empty() => {
                    if self.inbox_action(id, answer, &text) {
                        return false;
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            }
            self.prompt = Some((id, answer, text));
            return false;
        }
        if self.ops_key(code) || (!mods.contains(KeyModifiers::CONTROL) && self.activity_key(code))
        {
            return false;
        }
        match code {
            KeyCode::Char('a' | 'w') if self.screen == Screen::Requests => {
                if let Some(r) = self.requests.get(self.req_sel) {
                    let answer = code == KeyCode::Char('a');
                    if !answer || r.kind == "question" {
                        self.prompt = Some((r.id, answer, String::new()));
                        self.status.clear();
                    }
                }
            }
            KeyCode::Char('l') if self.screen == Screen::Requests => {
                if let Some(r) = self
                    .requests
                    .get(self.req_sel)
                    .filter(|r| r.kind == "unverified")
                {
                    match self.forge.run(&["land", &r.id.to_string()]) {
                        Ok(out) => {
                            self.status = out.trim().to_owned();
                            self.refresh();
                        }
                        Err(e) => self.status = format!("{e:#}"),
                    }
                }
            }
            KeyCode::Char('b' | 's') if self.screen == Screen::Initiative => {
                if let Some(id) = self.initiative.as_ref().map(|d| d.id) {
                    self.limit_prompt = Some((id, code == KeyCode::Char('b'), String::new()));
                    self.status.clear();
                }
            }
            KeyCode::Char('h') | KeyCode::Left if self.screen == Screen::Stats => {
                self.stats_tab_step(false)
            }
            KeyCode::Char('l') | KeyCode::Right if self.screen == Screen::Stats => {
                self.stats_tab_step(true)
            }
            KeyCode::Char('<') if self.screen == Screen::Stats => self.stats_sort_step(false),
            KeyCode::Char('>') if self.screen == Screen::Stats => self.stats_sort_step(true),
            KeyCode::Char('v') if self.screen == Screen::Stats => {
                if let Some((_, desc)) = &mut self.stats_sort {
                    *desc = !*desc;
                }
            }
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
                    Screen::Jobs => Screen::Stats,
                    Screen::Stats => Screen::Deploys,
                    Screen::Deploys => Screen::Doctor,
                    Screen::Doctor => Screen::Activity,
                    _ => Screen::Queue,
                };
                if self.screen == Screen::Stats {
                    self.scroll = 0;
                    self.load_stats();
                }
                self.scroll = 0;
                self.load_ops();
            }
            KeyCode::Enter => match self.screen {
                Screen::Jobs => {
                    if let Some(id) = self.jobs.get(self.job_sel).map(|j| j.id) {
                        self.scroll = 0;
                        self.open_job(id);
                    }
                }
                Screen::Task
                | Screen::JobView
                | Screen::Initiative
                | Screen::Stats
                | Screen::Deploys
                | Screen::Doctor
                | Screen::Activity => {}
                Screen::Initiatives => {
                    if let Some(id) = self.initiatives.get(self.init_sel).map(|i| i.id) {
                        self.scroll = 0;
                        self.open_initiative(id);
                    }
                }
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
                Screen::Initiative => self.screen = Screen::Initiatives,
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
        if app.screen == s || (s == Screen::Initiatives && app.screen == Screen::Initiative) {
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
        Span::styled("Forge", Style::default().add_modifier(Modifier::BOLD)),
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
        tab("stats", Screen::Stats),
        tab("deploy", Screen::Deploys),
        tab("doctor", Screen::Doctor),
        tab("activity", Screen::Activity),
        tab("task", Screen::Task),
        tab("job", Screen::JobView),
    ]);
    frame.render_widget(Paragraph::new(header), head);
    match app.screen {
        Screen::Queue => draw_queue(frame, app, body),
        Screen::Requests => draw_requests(frame, app, body),
        Screen::Initiatives => draw_initiatives(frame, app, body),
        Screen::Jobs => draw_jobs(frame, app, body),
        Screen::Stats => stats::draw(
            frame,
            &stats::StatsView {
                doc: app.stats.as_ref(),
                tab: app.stats_tab,
                sort: app.stats_sort,
                scroll: app.scroll,
            },
            body,
        ),
        Screen::Deploys => ops::draw_deploys(frame, app, body),
        Screen::Doctor => ops::draw_doctor(frame, app, body),
        Screen::Activity => activity::draw(frame, app, body),
        Screen::Task => draw_task(frame, app, body),
        Screen::JobView => draw_job(frame, app, body),
        Screen::Initiative => draw_initiative(frame, app, body),
    }
    let keys = match app.screen {
        Screen::Task | Screen::JobView => "j/k scroll  Esc back  r retry  R retry chain  q quit",
        Screen::Initiative => "j/k scroll  b budget  s stop-after  Esc back  q quit",
        Screen::Stats => {
            "h/l tab  < > sort column  v reverse  j/k scroll  Tab switch  g refresh  q quit"
        }
        Screen::Deploys => "j/k move  d deploy now  Tab switch  g refresh  q quit",
        Screen::Doctor => "j/k scroll  x gc worktrees  Tab switch  g refresh  q quit",
        Screen::Activity => "j/k scroll  p project  f kind  c clear  Tab switch  q quit",
        Screen::Requests => {
            "j/k move  a answer  w withdraw  l land  Enter open  Tab switch  q quit"
        }
        Screen::Queue => {
            "j/k move  Enter open  / query  n next page  Esc clear  Tab switch  r retry  q quit"
        }
        _ => "j/k move  Enter open  Tab switch  r retry  R retry chain  g refresh  q quit",
    };
    let foot_line = if let Some(text) = &app.query_prompt {
        Line::raw(format!("Query: {text}▏  Enter run  Esc cancel"))
    } else if let Some((id, budget, text)) = &app.limit_prompt {
        Line::raw(format!(
            "{} for initiative {id}: {text}▏  Enter submit  Esc cancel",
            if *budget { "Budget USD" } else { "Stop after" }
        ))
    } else if let Some((id, answer, text)) = &app.prompt {
        Line::raw(format!(
            "{} #{id}: {text}▏  Enter submit  Esc cancel",
            if *answer { "Answer" } else { "Withdraw reason" }
        ))
    } else if let Some(text) = app.ops.prompt() {
        Line::styled(text, Style::default().fg(Color::Yellow))
    } else if app.status.is_empty() {
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
    .block(Block::bordered().title(if app.query.is_empty() {
        "tasks, newest first".to_owned()
    } else {
        format!("forge {}", activity::log_args(&app.query, None).join(" "))
    }));
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
            Cell::from(r.to.clone().unwrap_or_else(|| "operator".into())),
            Cell::from(short(
                if r.question.is_empty() {
                    &r.text
                } else {
                    &r.question
                },
                200,
            )),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(11),
            Constraint::Length(13),
            Constraint::Length(12),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["ID", "KIND", "WF", "TO", "REQUEST"])
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
        lines.push(Line::raw(format!(
            "to: {}",
            r.to.as_deref().unwrap_or("operator")
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            if r.question.is_empty() {
                r.text.clone()
            } else {
                r.question.clone()
            },
            Style::default().fg(Color::Yellow),
        )));
        if let Some(summary) = app.summaries.get(&r.id).filter(|s| !s.is_empty()) {
            lines.push(Line::raw(format!("last attempt: {summary}")));
        }
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

/// Cost against budget as a bar of block characters, `width` cells wide.
fn cost_bar(cost: f64, budget: Option<f64>, width: usize) -> String {
    let Some(budget) = budget else {
        return format!("${cost:.2}, no budget cap");
    };
    let ratio = if budget > 0.0 {
        (cost / budget).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let filled = (ratio * width as f64).round() as usize;
    let over = if cost > budget { "  over budget" } else { "" };
    format!(
        "{}{} ${cost:.2} of ${budget:.2}{over}",
        "█".repeat(filled),
        "░".repeat(width - filled)
    )
}

fn elapsed(secs: i64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (h, m) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

fn draw_initiative(frame: &mut Frame, app: &App, area: Rect) {
    let Some(d) = &app.initiative else {
        frame.render_widget(
            Paragraph::new("no initiative open").block(Block::bordered().title("initiative")),
            area,
        );
        return;
    };
    let head = |t: &str| {
        Line::from(Span::styled(
            t.to_owned(),
            Style::default().add_modifier(Modifier::BOLD),
        ))
    };
    let mut state = vec![Span::styled(d.state.clone(), state_style(&d.state))];
    if let (true, Some(rule)) = (d.state == "held", &d.held_rule) {
        state.push(Span::styled(
            format!("  held: {rule}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(secs) = d.elapsed_secs {
        state.push(Span::raw(format!("  {} elapsed", elapsed(secs))));
    }
    let mut lines = vec![
        Line::raw(d.outcome.clone()),
        Line::from(state),
        Line::raw(format!("cost   {}", cost_bar(d.cost_usd, d.budget_usd, 20))),
        Line::raw(format!(
            "stop after {} failures on the same rule",
            d.stop_after_same_rule
        )),
        Line::raw(""),
        head("Tasks"),
    ];
    for t in &d.tasks {
        let retries = if t.retries > 0 {
            format!(" ({} retries)", t.retries)
        } else {
            String::new()
        };
        let score = t.score.map(|s| format!("  {s}/10")).unwrap_or_default();
        lines.push(Line::from(vec![
            Span::raw(format!("  #{:<4}", t.id)),
            Span::styled(format!("{:<11}", t.state), state_style(&t.state)),
            Span::raw(format!(
                "${:>6.2}{score}{retries}  {}",
                t.cost_usd,
                short(&t.reason, 60)
            )),
        ]));
    }
    if !d.refused.is_empty() {
        lines.push(Line::raw(""));
        lines.push(head("Refused"));
        for r in &d.refused {
            lines.push(Line::raw(format!("  {}: {}", r.rule, r.count)));
        }
    }
    if !d.rulings.is_empty() {
        lines.push(Line::raw(""));
        lines.push(head("Rulings"));
        for r in &d.rulings {
            lines.push(Line::raw(format!(
                "  task {}: {}",
                r.task_id,
                short(&r.question, 80)
            )));
            lines.push(Line::raw(format!("    {}", short(&r.answer, 90))));
        }
    }
    if !d.questions.is_empty() {
        lines.push(Line::raw(""));
        lines.push(head("Questions"));
        for q in &d.questions {
            lines.push(Line::raw(format!(
                "  task {}: {}",
                q.task_id,
                short(&q.question, 80)
            )));
            lines.push(Line::raw(format!(
                "    {}",
                q.answer
                    .as_deref()
                    .map_or("unanswered".to_owned(), |a| short(a, 90))
            )));
        }
    }
    if !d.deployed.is_empty() {
        lines.push(Line::raw(""));
        lines.push(head("Deploys"));
        for x in &d.deployed {
            let status = match (x.check_ok, &x.rolled_back_to) {
                (Some(true), _) => "ok".to_owned(),
                (Some(false), Some(to)) => format!("rolled back to {}", short(to, 8)),
                (Some(false), None) => "failed".to_owned(),
                (None, _) => "running".to_owned(),
            };
            lines.push(Line::raw(format!(
                "  task {} {} {} {status}",
                x.task_id,
                x.target,
                short(&x.sha, 8)
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.scroll, 0))
            .block(Block::bordered().title(format!("initiative {} · {}", d.id, d.project))),
        area,
    );
}

fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let rows = app.jobs.iter().map(|j| {
        Row::new(vec![
            Cell::from(j.id.to_string()),
            Cell::from(j.project.clone()),
            Cell::from(j.workflow.clone()),
            Cell::from(Span::styled(j.state.clone(), state_style(&j.state))),
            Cell::from(format!("${:.2}", j.cost_usd.unwrap_or(0.0))),
            Cell::from(time::fmt_time(j.started_at)),
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
            time::fmt_time(j.started_at),
            j.finished_at.map_or("-".to_string(), time::fmt_time),
            j.cost_usd.unwrap_or(0.0)
        )));
        if let Some(due) = j.due_at {
            lines.push(Line::raw(format!("due {}", time::fmt_time(due))));
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
            ..Forge::new()
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
            r#"[{"id":7,"state":"running","workflow":"tdd","attempts":1,"cost_usd":0.5,"repo":"/r","text":"stars tier","task":"stars tier","created_at":0,"created":"","trust":"operator","project":"forge"},
                {"id":6,"state":"queued","workflow":"direct","attempts":0,"cost_usd":0,"repo":"/r","text":"docs","task":"docs","created_at":0,"created":"","trust":"operator"}]"#,
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
            r#"[{"id":42,"kind":"suite","question":"stars-tier asserts stars never age","text":"stars-tier asserts stars never age","tried":"ran the suite","path":"tests/acceptance/stars-tier.test.ts","workflow":"tdd","repo":"/r","task":"stellar lifetime"}]"#,
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
        app.trace = Some(
            serde_json::from_value(serde_json::json!({
                "task": {"id": 5},
                "attempts": [],
                "ops": [],
                "resolved": null,
                "diagnosis": [],
                "deploys": []
            }))
            .unwrap(),
        );
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
            "attempts": [{"attempt_no": 1, "step": "code", "step_seq": 0, "state": "checks_failed", "started_at": 0, "timed_out": false, "num_turns": 12, "tool_calls": 0, "cost_usd": 0.4, "agent_ms": 0, "commits": 0, "files_changed": 0, "dirty": false, "start_sha": "", "end_sha": "", "log_path": "", "tokens": {}, "rate_limits": {}, "inputs": {}, "outputs": {}, "reason": "L1 failed: test", "verdict": [{"level": "L1", "name": "test", "ok": false, "tail": "FAIL x\nmore"}], "envelope": {}}],
            "ops": [{"id": 1, "seq": 0, "name": "clone", "kernel": true, "started_at": 0, "ms": 0, "ok": true, "detail": "abc", "output": ""}, {"id": 2, "seq": 1, "name": "verify", "kernel": true, "started_at": 0, "ms": 0, "ok": false, "detail": "L1 failed: test", "output": ""}],
            "resolved": null,
            "diagnosis": [{"what": "the repo's test check fails", "action": "read the failing tests"}],
            "deploys": []
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
            "resolved": null,
            "diagnosis": [],
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
    fn tab_cycles_through_every_screen_and_back() {
        let mut app = app_with("[]", "[]");
        assert_eq!(app.screen, Screen::Queue);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Requests);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Initiatives);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Jobs);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Stats);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Deploys);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Doctor);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Activity);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::Queue);
    }

    #[test]
    fn job_events_flag_the_jobs_list_and_the_open_job_as_dirty() {
        let mut app = app_with("[]", "[]");
        app.job = Some(
            serde_json::from_value(serde_json::json!({
                "id": 9,
                "project": "forge",
                "workflow": "nightly",
                "workflow_hash": "",
                "landed_sha": "",
                "trigger_kind": "manual",
                "trigger_ref": "",
                "state": "running",
                "workflow_source": "catalog",
                "dry_run": false,
                "started_at": 0,
                "verdict_json": "",
                "steps": [],
                "effects": []
            }))
            .unwrap(),
        );
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
