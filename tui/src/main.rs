//! forge-tui: the operator's seat. A client of the `forge` CLI and nothing
//! else: it reads `log --json`, `requests --json`, and `trace --json`, and
//! acts through `forge retry`. It never opens the database and never links
//! the kernel, so a kernel that changes its rules changes nothing here.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use serde_json::Value;
use std::process::Command;
use std::time::{Duration, Instant};

/// The forge binary: `FORGE_BIN`, else `forge` on PATH. `FORGE2_HOME`
/// passes through the environment like any other.
struct Forge {
    bin: String,
}

impl Forge {
    fn new() -> Forge {
        Forge {
            bin: std::env::var("FORGE_BIN").unwrap_or_else(|_| "forge".into()),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.bin)
            .args(args)
            .output()
            .with_context(|| format!("running {} {}", self.bin, args.join(" ")))?;
        if !out.status.success() {
            anyhow::bail!(
                "forge {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        let text = self.run(args)?;
        serde_json::from_str(&text).with_context(|| format!("parsing forge {}", args.join(" ")))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Queue,
    Requests,
    Task,
}

struct App {
    forge: Forge,
    screen: Screen,
    tasks: Vec<Value>,
    requests: Vec<Value>,
    trace: Option<Value>,
    queue_sel: usize,
    req_sel: usize,
    scroll: u16,
    status: String,
    refreshed: Instant,
}

impl App {
    fn new(forge: Forge) -> App {
        App {
            forge,
            screen: Screen::Queue,
            tasks: Vec::new(),
            requests: Vec::new(),
            trace: None,
            queue_sel: 0,
            req_sel: 0,
            scroll: 0,
            status: String::new(),
            refreshed: Instant::now() - Duration::from_secs(60),
        }
    }

    fn refresh(&mut self) {
        match self.forge.json(&["log", "--json", "--limit", "60"]) {
            Ok(v) => self.tasks = v.as_array().cloned().unwrap_or_default(),
            Err(e) => self.status = format!("{e:#}"),
        }
        match self.forge.json(&["requests", "--json"]) {
            Ok(v) => self.requests = v.as_array().cloned().unwrap_or_default(),
            Err(e) => self.status = format!("{e:#}"),
        }
        if self.screen == Screen::Task
            && let Some(id) = self.trace.as_ref().and_then(|t| t["task"]["id"].as_i64())
        {
            self.open_task(id);
        }
        self.queue_sel = self.queue_sel.min(self.tasks.len().saturating_sub(1));
        self.req_sel = self.req_sel.min(self.requests.len().saturating_sub(1));
        self.refreshed = Instant::now();
    }

    fn open_task(&mut self, id: i64) {
        match self.forge.json(&["trace", &id.to_string(), "--json"]) {
            Ok(v) => {
                self.trace = Some(v);
                self.screen = Screen::Task;
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// The task the cursor is on, whichever screen shows it.
    fn current_id(&self) -> Option<i64> {
        match self.screen {
            Screen::Queue => self
                .tasks
                .get(self.queue_sel)
                .and_then(|t| t["id"].as_i64()),
            Screen::Requests => self
                .requests
                .get(self.req_sel)
                .and_then(|t| t["id"].as_i64()),
            Screen::Task => self.trace.as_ref().and_then(|t| t["task"]["id"].as_i64()),
        }
    }

    fn retry(&mut self, chain: bool) {
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

    fn down(&mut self) {
        match self.screen {
            Screen::Queue => {
                self.queue_sel = (self.queue_sel + 1).min(self.tasks.len().saturating_sub(1))
            }
            Screen::Requests => {
                self.req_sel = (self.req_sel + 1).min(self.requests.len().saturating_sub(1))
            }
            Screen::Task => self.scroll = self.scroll.saturating_add(1),
        }
    }

    fn up(&mut self) {
        match self.screen {
            Screen::Queue => self.queue_sel = self.queue_sel.saturating_sub(1),
            Screen::Requests => self.req_sel = self.req_sel.saturating_sub(1),
            Screen::Task => self.scroll = self.scroll.saturating_sub(1),
        }
    }
}

fn state_style(state: &str) -> Style {
    let color = match state {
        "succeeded" => Color::Green,
        "running" => Color::Cyan,
        "queued" => Color::Gray,
        "blocked" => Color::Yellow,
        "failed" => Color::Red,
        "unverified" => Color::Magenta,
        _ => Color::White,
    };
    Style::default().fg(color)
}

fn draw(frame: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let queued = app.tasks.iter().filter(|t| t["state"] == "queued").count();
    let running = app.tasks.iter().filter(|t| t["state"] == "running").count();
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
    let header = Line::from(vec![
        Span::styled("Forge 2", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {queued} queued, {running} running  ")),
        tab("queue", Screen::Queue),
        tab(
            &format!("requests ({})", app.requests.len()),
            Screen::Requests,
        ),
        tab("task", Screen::Task),
    ]);
    frame.render_widget(Paragraph::new(header), head);
    match app.screen {
        Screen::Queue => draw_queue(frame, app, body),
        Screen::Requests => draw_requests(frame, app, body),
        Screen::Task => draw_task(frame, app, body),
    }
    let keys = match app.screen {
        Screen::Task => "j/k scroll  Esc back  r retry  R retry chain  q quit",
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

fn short(v: &Value, key: &str, n: usize) -> String {
    let s = v[key].as_str().unwrap_or("").replace('\n', " ");
    s.chars().take(n).collect()
}

fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let rows = app.tasks.iter().map(|t| {
        let state = t["state"].as_str().unwrap_or("");
        Row::new(vec![
            Cell::from(t["id"].as_i64().unwrap_or(0).to_string()),
            Cell::from(Span::styled(state.to_string(), state_style(state))),
            Cell::from(t["workflow"].as_str().unwrap_or("").to_string()),
            Cell::from(t["attempts"].as_i64().unwrap_or(0).to_string()),
            Cell::from(format!("${:.2}", t["cost_usd"].as_f64().unwrap_or(0.0))),
            Cell::from(short(t, "task", 200)),
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
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["ID", "STATE", "WF", "ATT", "COST", "TASK"])
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
            Cell::from(r["id"].as_i64().unwrap_or(0).to_string()),
            Cell::from(r["kind"].as_str().unwrap_or("").to_string()),
            Cell::from(r["workflow"].as_str().unwrap_or("").to_string()),
            Cell::from(short(r, "text", 200)),
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
            format!("task {}  {}", r["id"], r["task"].as_str().unwrap_or("")),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            r["text"].as_str().unwrap_or("").to_string(),
            Style::default().fg(Color::Yellow),
        )));
        if let Some(p) = r["path"].as_str().filter(|p| !p.is_empty()) {
            lines.push(Line::raw(format!("path: {p}")));
        }
        if let Some(t) = r["tried"].as_str().filter(|t| !t.is_empty()) {
            lines.push(Line::raw(""));
            lines.push(Line::raw(format!("did: {t}")));
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

fn draw_task(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(tr) = &app.trace {
        let t = &tr["task"];
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
        for (k, v) in [("after", &t["after"]), ("retry of", &t["retry_of"])] {
            if !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()) {
                lines.push(Line::raw(format!("{k} {v}")));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(t["text"].as_str().unwrap_or("").to_string()));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "attempts",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for a in tr["attempts"].as_array().into_iter().flatten() {
            let st = a["state"].as_str().unwrap_or("");
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "  {:>2} {:<9} ",
                    a["attempt_no"],
                    a["step"].as_str().unwrap_or("")
                )),
                Span::styled(format!("{st:<13}"), state_style(st)),
                Span::raw(format!(
                    " {:>3} turns  ${:.2}  {}",
                    a["num_turns"],
                    a["cost_usd"].as_f64().unwrap_or(0.0),
                    a["reason"].as_str().unwrap_or("")
                )),
            ]));
            if let Some(checks) = a["verdict"].as_array() {
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
        for o in tr["ops"].as_array().into_iter().flatten() {
            let ok = o["ok"] == true;
            lines.push(Line::from(vec![
                Span::styled(
                    if ok { "  ✓ " } else { "  ✗ " }.to_string(),
                    Style::default().fg(if ok { Color::Green } else { Color::Red }),
                ),
                Span::raw(format!(
                    "{:<11} {}",
                    o["name"].as_str().unwrap_or(""),
                    o["detail"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                )),
            ]));
        }
        if let Some(d) = tr["diagnosis"].as_array().filter(|d| !d.is_empty()) {
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

fn run() -> Result<()> {
    let mut app = App::new(Forge::new());
    app.refresh();
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw(f, &app))?;
            if event::poll(Duration::from_millis(250))?
                && let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('j') | KeyCode::Down => app.down(),
                    KeyCode::Char('k') | KeyCode::Up => app.up(),
                    KeyCode::Char('g') => app.refresh(),
                    KeyCode::Char('r') => app.retry(false),
                    KeyCode::Char('R') => app.retry(true),
                    KeyCode::Tab => {
                        app.screen = match app.screen {
                            Screen::Queue => Screen::Requests,
                            _ => Screen::Queue,
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(id) = app.current_id()
                            && app.screen != Screen::Task
                        {
                            app.scroll = 0;
                            app.open_task(id);
                        }
                    }
                    KeyCode::Esc => {
                        if app.screen == Screen::Task {
                            app.screen = Screen::Queue;
                        }
                    }
                    _ => {}
                }
            }
            if app.refreshed.elapsed() > Duration::from_secs(3) {
                app.refresh();
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--dump") {
        // One frame, no terminal: for a script or a smoke test.
        let mut app = App::new(Forge::new());
        app.refresh();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|f| draw(f, &app))?;
        print!("{}", render_text(terminal.backend()));
        return Ok(());
    }
    run()
}

/// The backend's buffer as lines of text.
fn render_text(backend: &ratatui::backend::TestBackend) -> String {
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
        app.tasks = serde_json::from_str::<Value>(tasks)
            .unwrap()
            .as_array()
            .cloned()
            .unwrap();
        app.requests = serde_json::from_str::<Value>(requests)
            .unwrap()
            .as_array()
            .cloned()
            .unwrap();
        app
    }

    fn frame_of(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        render_text(terminal.backend())
    }

    #[test]
    fn the_queue_lists_tasks_with_their_state_and_counts_them() {
        let app = app_with(
            r#"[{"id":7,"state":"running","workflow":"tdd","attempts":1,"cost_usd":0.5,"task":"stars tier"},
                {"id":6,"state":"queued","workflow":"direct","attempts":0,"cost_usd":0,"task":"docs"}]"#,
            "[]",
        );
        let text = frame_of(&app);
        assert!(text.contains("1 queued, 1 running"), "{text}");
        assert!(text.contains("7     running"), "{text}");
        assert!(text.contains("stars tier"), "{text}");
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
    fn a_trace_renders_attempts_operations_and_the_diagnosis() {
        let mut app = app_with("[]", "[]");
        app.trace = Some(serde_json::json!({
            "task": {"id": 3, "state": "failed", "workflow": "tdd", "reason": "L1 failed: test (after 2 attempt(s))", "branch": "forge/3-x", "base_sha": "abcdef1234567890", "text": "do the thing", "after": [], "retry_of": null},
            "attempts": [{"attempt_no": 1, "step": "code", "state": "checks_failed", "num_turns": 12, "cost_usd": 0.4, "reason": "L1 failed: test", "verdict": [{"level": "L1", "name": "test", "ok": false, "tail": "FAIL x\nmore"}]}],
            "ops": [{"name": "clone", "ok": true, "detail": "abc"}, {"name": "verify", "ok": false, "detail": "L1 failed: test"}],
            "diagnosis": [{"what": "the repo's test check fails", "action": "read the failing tests"}]
        }));
        app.screen = Screen::Task;
        let text = frame_of(&app);
        assert!(text.contains("task 3  failed"), "{text}");
        assert!(text.contains("✗ L1 test: FAIL x"), "{text}");
        assert!(text.contains("✗ verify"), "{text}");
        assert!(text.contains("action read the failing tests"), "{text}");
    }
}
