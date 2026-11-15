//! The admin console, `serve:`: who is connected, from where, for how long, how idle, and
//! a key to end a session. Only users listed in `serve.admins` get it.

use std::sync::Arc;

use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use alouette_ui::{Launcher, Module};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use url::Url;

use crate::registry::Registry;

pub struct AdminModule {
    reg: Arc<Registry>,
    me: u64,
}

impl AdminModule {
    pub fn new(reg: Arc<Registry>, me: u64) -> Self {
        Self { reg, me }
    }
}

impl Module for AdminModule {
    fn id(&self) -> &'static str {
        "serve"
    }
    fn name(&self) -> &'static str {
        "Server"
    }
    fn description(&self) -> &'static str {
        "Who is connected to this Alouette server"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["serve"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![Launcher::new("Server console", "serve:", "Sessions, limits and users")]
    }
    fn open(&mut self, _url: &Url, _cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        Ok(Box::new(AdminWindow { reg: self.reg.clone(), me: self.me, sel: 0, note: None }))
    }
}

struct AdminWindow {
    reg: Arc<Registry>,
    me: u64,
    sel: usize,
    note: Option<String>,
}

fn ago(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

impl Window for AdminWindow {
    fn title(&self) -> String {
        "Server console".into()
    }
    fn module(&self) -> &'static str {
        "serve"
    }
    fn url(&self) -> Option<String> {
        Some("serve:".into())
    }
    fn animates(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑↓", "Choose"), ("k", "End session")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let sessions = self.reg.snapshot();
        self.sel = self.sel.min(sessions.len().saturating_sub(1));
        let lim = &self.reg.limits;
        let mut lines = vec![
            Line::styled(
                format!(
                    " {} sessions of {} · {} a user · {} guests · {} windows each · idle {} min",
                    sessions.len(),
                    lim.max_sessions,
                    lim.sessions_per_user,
                    lim.max_guests,
                    lim.max_windows,
                    lim.idle_minutes
                ),
                t.dim(),
            ),
            Line::raw(""),
            Line::styled(
                format!(" {:<16} {:<22} {:>8} {:>6} {:>7}  {}", "User", "From", "On for", "Idle", "Windows", "Screen"),
                t.accent(),
            ),
        ];
        for (i, s) in sessions.iter().enumerate() {
            let me = if s.id == self.me { " (you)" } else { "" };
            let text = format!(
                " {:<16} {:<22} {:>8} {:>6} {:>7}  {}x{}{me}",
                if s.guest { format!("{} (guest)", s.user) } else { s.user.clone() },
                s.addr.map(|a| a.to_string()).unwrap_or_default(),
                ago(s.on_for()),
                ago(s.idle()),
                s.windows(),
                s.size().0,
                s.size().1
            );
            lines.push(Line::from(Span::styled(text, if i == self.sel { t.selection() } else { t.window() })));
        }
        if let Some(n) = &self.note {
            lines.push(Line::raw(""));
            lines.push(Line::styled(format!(" {n}"), t.dim()));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn on_key(&mut self, key: KeyEvent, _cx: &mut Cx) -> Handled {
        let sessions = self.reg.snapshot();
        match key.code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(sessions.len().saturating_sub(1)),
            KeyCode::Char('k') => match sessions.get(self.sel) {
                Some(s) if s.id == self.me => self.note = Some("That is this session; quit instead.".into()),
                Some(s) => {
                    s.kill("An administrator ended this session.");
                    self.note = Some(format!("Ended {}'s session.", s.user));
                }
                None => {}
            },
            _ => return Handled::No,
        }
        Handled::Yes
    }
}
