//! The deploys and doctor screens: `forge project deploy list`, `forge
//! deploy log` and `forge doctor` as text, with a deploy-now key (asking
//! first) and a gc key.

use crate::{App, Screen, short, time};
use crossterm::event::KeyCode;
use forge_client::{Deploy, DeployTargetRow, DoctorCheck};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

/// One declared target with its deploys, newest first.
pub struct TargetView {
    pub target: DeployTargetRow,
    pub log: Vec<Deploy>,
}

#[derive(Default)]
pub struct Ops {
    pub targets: Vec<TargetView>,
    pub sel: usize,
    /// The target index a "deploy now" is waiting on a `y` for.
    pub confirm: Option<usize>,
    pub checks: Vec<DoctorCheck>,
}

impl Ops {
    /// The footer line while a deploy waits for its confirmation.
    pub fn prompt(&self) -> Option<String> {
        let t = &self.targets.get(self.confirm?)?.target;
        Some(format!(
            "Deploy {}/{} now? y confirm  n cancel",
            t.project, t.name
        ))
    }
}

impl App {
    /// Every project's targets, each with its own log: what the web page reads.
    pub(crate) fn load_deploys(&mut self) {
        let read = || -> anyhow::Result<Vec<TargetView>> {
            let mut out = Vec::new();
            for p in self.forge.project_list()? {
                for target in self.forge.deploy_targets(&p.name)? {
                    let log = self.forge.deploy_log(&p.name, Some(&target.name))?;
                    out.push(TargetView { target, log });
                }
            }
            Ok(out)
        };
        match read() {
            Ok(targets) => self.ops.targets = targets,
            Err(e) => self.status = format!("{e:#}"),
        }
        self.ops.sel = self.ops.sel.min(self.ops.targets.len().saturating_sub(1));
    }

    /// `forge doctor --json`, read when the screen opens and on `g`.
    pub(crate) fn load_doctor(&mut self) {
        match self.forge.doctor() {
            Ok(checks) => self.ops.checks = checks,
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    pub(crate) fn load_ops(&mut self) {
        match self.screen {
            Screen::Deploys => self.load_deploys(),
            Screen::Doctor => {
                self.load_doctor();
                self.load_initiatives();
            }
            Screen::Messages => self.load_messages(),
            Screen::Workflows => self.load_workflows(),
            _ => {}
        }
    }

    /// Keys of the two screens; `true` when the key was theirs.
    pub(crate) fn ops_key(&mut self, code: KeyCode) -> bool {
        if let Some(i) = self.ops.confirm {
            self.ops.confirm = None;
            if code == KeyCode::Char('y') {
                self.deploy_now(i);
            } else {
                self.status = "deploy cancelled".into();
            }
            return true;
        }
        match (self.screen, code) {
            (Screen::Deploys, KeyCode::Char('j') | KeyCode::Down) => {
                let last = self.ops.targets.len().saturating_sub(1);
                self.ops.sel = (self.ops.sel + 1).min(last);
            }
            (Screen::Deploys, KeyCode::Char('k') | KeyCode::Up) => {
                self.ops.sel = self.ops.sel.saturating_sub(1);
            }
            (Screen::Deploys, KeyCode::Char('d')) => {
                if self.ops.sel < self.ops.targets.len() {
                    self.ops.confirm = Some(self.ops.sel);
                    self.status.clear();
                }
            }
            (Screen::Doctor, KeyCode::Char('x')) => self.gc(),
            _ => return false,
        }
        true
    }

    fn deploy_now(&mut self, i: usize) {
        let Some(t) = self.ops.targets.get(i).map(|t| t.target.clone()) else {
            return;
        };
        self.status = match self.forge.run(&["deploy", &t.project, &t.name]) {
            Ok(out) => out.trim().to_owned(),
            Err(e) => format!("{e:#}"),
        };
        self.load_deploys();
    }

    fn gc(&mut self) {
        self.status = match self.forge.run(&["gc"]) {
            Ok(out) => out.trim().to_owned(),
            Err(e) => format!("{e:#}"),
        };
        self.load_doctor();
    }
}

fn verdict(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "ok",
        Some(false) => "FAILED",
        None => "-",
    }
}

fn verdict_span(label: &str, v: Option<bool>) -> Span<'static> {
    let color = match v {
        Some(true) => Color::Green,
        Some(false) => Color::Red,
        None => Color::DarkGray,
    };
    Span::styled(
        format!("{label} {}  ", verdict(v)),
        Style::default().fg(color),
    )
}

fn check_span(d: &Deploy) -> Span<'static> {
    match (d.check_ok, &d.rolled_back_to) {
        (None, _) => Span::styled("check running  ", Style::default().fg(Color::Cyan)),
        (Some(false), Some(to)) => Span::styled(
            format!("check FAILED (rolled back to {})  ", short(to, 8)),
            Style::default().fg(Color::Red),
        ),
        (ok, _) => verdict_span("check", ok),
    }
}

fn target_line(t: &TargetView, selected: bool) -> Line<'static> {
    let host = t.target.args.get("host").map_or("", String::as_str);
    let mut spans = vec![Span::raw(format!(
        "{} {:<20} {:<16} {:<10} ",
        if selected { ">" } else { " " },
        format!("{}/{}", t.target.project, t.target.name),
        t.target.method,
        host,
    ))];
    match t.log.first() {
        None => spans.push(Span::styled(
            "never deployed",
            Style::default().fg(Color::DarkGray),
        )),
        Some(d) => {
            spans.push(Span::raw(format!("{} ", short(&d.sha, 8))));
            spans.push(check_span(d));
            spans.push(verdict_span("smoke", d.smoke_ok));
            spans.push(verdict_span("look", d.look_ok));
            spans.push(Span::raw(time::fmt_time(d.started_at)));
        }
    }
    let mut line = Line::from(spans);
    if selected {
        line = line.style(Style::default().add_modifier(Modifier::REVERSED));
    }
    line
}

fn log_lines(t: &TargetView) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for d in &t.log {
        lines.push(Line::from(vec![
            Span::raw(format!(
                "#{:<4} {} {}  ",
                d.id,
                short(&d.sha, 8),
                time::fmt_time(d.started_at)
            )),
            check_span(d),
            verdict_span("smoke", d.smoke_ok),
            verdict_span("look", d.look_ok),
        ]));
        if !d.reason.is_empty() {
            lines.push(Line::raw(format!("      {}", d.reason)));
        }
        let findings: Vec<serde_json::Value> = d
            .look_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        for f in findings {
            lines.push(Line::raw(format!(
                "      {} {}",
                f["severity"].as_str().unwrap_or("?"),
                f["finding"].as_str().unwrap_or("")
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw("no deploys yet"));
    }
    lines
}

pub fn draw_deploys(frame: &mut Frame, app: &App, area: Rect) {
    let ops = &app.ops;
    if ops.targets.is_empty() {
        frame.render_widget(
            Paragraph::new("no deploy targets declared").block(Block::bordered().title("deploys")),
            area,
        );
        return;
    }
    let top = (ops.targets.len() as u16 + 2).min(area.height / 2);
    let [list, log] = Layout::vertical([Constraint::Length(top), Constraint::Min(1)]).areas(area);
    let lines: Vec<Line> = ops
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| target_line(t, i == ops.sel))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("deploy targets")),
        list,
    );
    let sel = &ops.targets[ops.sel.min(ops.targets.len() - 1)];
    frame.render_widget(
        Paragraph::new(log_lines(sel)).block(Block::bordered().title(format!(
            "log: {}/{}, newest first",
            sel.target.project, sel.target.name
        ))),
        log,
    );
}

fn status_style(status: &str) -> Style {
    Style::default().fg(match status {
        "ok" => Color::Green,
        "warn" => Color::Yellow,
        "fail" => Color::Red,
        _ => Color::White,
    })
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    )
}

fn window(label: &str, pct: Option<f64>, resets: Option<i64>) -> Option<String> {
    let pct = pct?;
    let resets = resets.map_or(String::new(), |t| {
        format!(" (resets {})", time::fmt_time(t))
    });
    Some(format!("{label} {:.0}%{resets}", pct * 100.0))
}

pub fn draw_doctor(frame: &mut Frame, app: &App, area: Rect) {
    let checks = &app.ops.checks;
    let mut lines = vec![heading("Checks")];
    for c in checks {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<5}", c.status), status_style(&c.status)),
            Span::raw(format!(" {:<14} {}", c.name, c.detail)),
        ]));
        if !c.hint.is_empty() && c.status != "ok" {
            lines.push(Line::raw(format!("{:<21}fix: {}", "", c.hint)));
        }
    }
    lines.push(heading("Held initiatives"));
    let held: Vec<_> = app
        .initiatives
        .iter()
        .filter(|i| i.state == "held")
        .collect();
    if held.is_empty() {
        lines.push(Line::raw("  none"));
    }
    for i in held {
        lines.push(Line::raw(format!(
            "  #{} {} held by {}: {}",
            i.id,
            i.project,
            i.held_rule.as_deref().unwrap_or("?"),
            i.outcome
        )));
    }
    lines.push(heading("Worktrees"));
    match checks.iter().find(|c| c.name == "worktrees") {
        Some(c) => {
            lines.push(Line::raw(format!("  {}", c.detail)));
            if c.worktree_ids.as_ref().is_some_and(|ids| !ids.is_empty()) {
                lines.push(Line::raw("  x runs forge gc on them"));
            }
        }
        None => lines.push(Line::raw("  no worktrees check")),
    }
    lines.push(heading("Rate windows"));
    let mut any = false;
    for c in checks.iter().filter(|c| c.name == "rate_limit") {
        any = true;
        let parts: Vec<String> = [
            window("5h", c.five_hour_pct, c.five_hour_resets_at),
            window("7d", c.seven_day_pct, c.seven_day_resets_at),
        ]
        .into_iter()
        .flatten()
        .collect();
        let text = if parts.is_empty() {
            c.detail.clone()
        } else {
            parts.join(" · ")
        };
        lines.push(Line::raw(format!(
            "  {}: {text}",
            c.provider.as_deref().unwrap_or("provider")
        )));
    }
    if !any {
        lines.push(Line::raw("  none"));
    }
    lines.push(heading("Spend"));
    lines.push(Line::raw(match checks.iter().find(|c| c.name == "spend") {
        Some(c) => match (c.spend_usd, c.spend_cap_usd) {
            (Some(s), Some(cap)) => format!("  ${s:.2} of ${cap:.2}"),
            (Some(s), None) => format!("  ${s:.2}"),
            _ => format!("  {}", c.detail),
        },
        None => "  no spend check".into(),
    }));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.scroll, 0))
            .block(Block::bordered().title("doctor")),
        area,
    );
}
