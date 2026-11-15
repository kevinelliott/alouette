use alouette_core::{AppEvent, TaskId, TaskState};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};

use crate::window::{Cx, DrawCx, Handled, Window};

/// Every background task, with progress, cancel and retry.
pub struct TasksWindow {
    state: TableState,
    ids: Vec<TaskId>,
}

impl Default for TasksWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl TasksWindow {
    pub fn new() -> Self {
        Self { state: TableState::default(), ids: vec![] }
    }

    fn selected(&self) -> Option<TaskId> {
        self.state.selected().and_then(|i| self.ids.get(i).copied())
    }
}

fn bar(progress: Option<f32>, width: usize, tick: u64) -> String {
    match progress {
        Some(p) => {
            let filled = ((p * width as f32).round() as usize).min(width);
            format!("{}{} {:>3}%", "█".repeat(filled), "░".repeat(width - filled), (p * 100.0).round() as u32)
        }
        None => {
            // An indeterminate bouncing block.
            let pos = (tick as usize) % (width.max(1) * 2);
            let pos = if pos >= width { width * 2 - pos - 1 } else { pos };
            let mut s: Vec<char> = "░".repeat(width).chars().collect();
            if let Some(c) = s.get_mut(pos) {
                *c = '█';
            }
            format!("{}     ", s.into_iter().collect::<String>())
        }
    }
}

fn elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s / 60) % 60)
    }
}

impl Window for TasksWindow {
    fn title(&self) -> String {
        "Tasks".into()
    }
    fn module(&self) -> &'static str {
        "core"
    }
    fn url(&self) -> Option<String> {
        Some("about:tasks".into())
    }
    fn animates(&self) -> bool {
        true
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let tasks = cx.tasks.list();
        self.ids = tasks.iter().rev().map(|t| t.id).collect();
        if tasks.is_empty() {
            let lines = vec![
                Line::default(),
                Line::from(Span::styled("  Nothing is running.", t.accent())),
                Line::default(),
                Line::from("  Network jobs appear here as they run: fetches, syncs, transfers."),
                Line::from("  Try Modules > Demo to start a few pretend jobs."),
            ];
            frame.render_widget(ratatui::widgets::Paragraph::new(lines).style(t.window()), area);
            return;
        }
        if self.state.selected().is_none() {
            self.state.select(Some(0));
        }
        if let Some(i) = self.state.selected() {
            self.state.select(Some(i.min(self.ids.len() - 1)));
        }
        let tick = (cx.now.elapsed().as_millis() as u64 / 120)
            .wrapping_add(std::time::Instant::now().duration_since(cx.now).as_millis() as u64);
        let tick = tick.wrapping_add(cx.tasks.list().len() as u64);
        let bar_w = 14usize;
        let rows = tasks.iter().rev().map(|task| {
            let (state_style, state) = match &task.state {
                TaskState::Running => (Style::new().fg(t.accent), "running".to_string()),
                TaskState::Done => (Style::new().fg(t.success), "done".to_string()),
                TaskState::Failed(_) => (Style::new().fg(t.error).add_modifier(Modifier::BOLD), "failed".to_string()),
                TaskState::Cancelled => (t.dim(), "cancelled".to_string()),
            };
            let detail = match &task.state {
                TaskState::Failed(e) => e.clone(),
                _ => task.detail.clone(),
            };
            let progress = if task.state == TaskState::Running {
                bar(task.progress, bar_w, tick + task.started.elapsed().as_millis() as u64 / 120)
            } else if task.state == TaskState::Done {
                bar(Some(1.0), bar_w, 0)
            } else {
                bar(task.progress.or(Some(0.0)), bar_w, 0)
            };
            Row::new(vec![
                Cell::from(task.id.to_string()),
                Cell::from(task.module.clone()).style(t.dim()),
                Cell::from(Line::from(vec![
                    Span::raw(task.title.clone()),
                    Span::styled(format!("  {detail}"), t.dim()),
                ])),
                Cell::from(progress),
                Cell::from(state).style(state_style),
                Cell::from(elapsed(task.elapsed())).style(t.dim()),
            ])
        });
        let header = Row::new(vec!["#", "Module", "Task", "Progress", "State", "Time"])
            .style(Style::new().fg(t.accent).add_modifier(Modifier::BOLD));
        let table = Table::new(
            rows,
            [
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Min(16),
                Constraint::Length(bar_w as u16 + 5),
                Constraint::Length(9),
                Constraint::Length(7),
            ],
        )
        .header(header)
        .style(t.window())
        .row_highlight_style(if cx.focused { t.selection() } else { Style::new() });
        frame.render_stateful_widget(table, area, &mut self.state);
    }
    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let n = self.ids.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if n > 0 {
                    self.state.select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.state.select(Some((self.state.selected().unwrap_or(0) + 1).min(n - 1)));
                }
            }
            KeyCode::Char('c') | KeyCode::Delete => {
                if let Some(id) = self.selected()
                    && cx.tasks.cancel(id)
                {
                    cx.notify(alouette_core::Notification::info(format!("Cancelled task {id}")));
                }
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.selected() {
                    cx.tasks.retry(id);
                }
            }
            KeyCode::Char('x') => {
                cx.tasks.clear_finished();
                self.state.select(Some(0));
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }
    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, _cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::Down(_) if ev.row > area.y => {
                let i = (ev.row - area.y - 1) as usize + self.state.offset();
                if i < self.ids.len() {
                    self.state.select(Some(i));
                }
                Handled::Yes
            }
            MouseEventKind::ScrollUp => {
                self.state.select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)));
                Handled::Yes
            }
            MouseEventKind::ScrollDown => {
                let n = self.ids.len();
                if n > 0 {
                    self.state.select(Some((self.state.selected().unwrap_or(0) + 1).min(n - 1)));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn on_event(&mut self, _ev: &AppEvent, _cx: &mut Cx) {}
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("c", "Cancel"), ("r", "Retry"), ("x", "Clear done")]
    }
}
