//! The startup splash: logo, tagline and a fake boot sequence.

use std::time::{Duration, Instant};

use alouette_nostalgia::{BOOT_LINES, LOGO, TAGLINE};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear};

use crate::chrome;
use crate::theme::Theme;

pub struct Splash {
    pub started: Instant,
    pub duration: Duration,
}

impl Splash {
    pub fn new(duration: Duration) -> Self {
        Self { started: Instant::now(), duration }
    }

    pub fn done(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= self.duration
    }

    pub fn draw(&self, frame: &mut Frame, screen: Rect, theme: &Theme, now: Instant) {
        let logo_w = LOGO.iter().map(|l| l.len()).max().unwrap_or(40) as u16;
        let w = (logo_w + 6).max(TAGLINE.len() as u16 + 6).min(screen.width);
        let h = (LOGO.len() + BOOT_LINES.len() + 7) as u16;
        let rect = chrome::centered(screen, w, h.min(screen.height));
        let style = Style::new().fg(theme.dialog_fg).bg(theme.dialog_bg);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Double)
                .border_style(style)
                .style(style),
            rect,
        );
        chrome::shadow(frame.buffer_mut(), rect, screen, theme);
        let buf = frame.buffer_mut();
        let mut y = rect.y + 1;
        let accent = Style::new().fg(theme.accent).bg(theme.dialog_bg).add_modifier(Modifier::BOLD);
        for line in LOGO {
            if y >= rect.bottom() - 1 {
                return;
            }
            let x = rect.x + rect.width.saturating_sub(line.len() as u16) / 2;
            buf.set_string(x, y, line, if theme.is_mono() { style } else { accent });
            y += 1;
        }
        y += 1;
        let x = rect.x + rect.width.saturating_sub(TAGLINE.len() as u16) / 2;
        buf.set_string(x, y, TAGLINE, style);
        y += 2;
        let elapsed = now.duration_since(self.started).as_secs_f32();
        let per = self.duration.as_secs_f32().max(0.1) * 0.8 / BOOT_LINES.len() as f32;
        let shown = ((elapsed / per) as usize + 1).min(BOOT_LINES.len());
        for line in &BOOT_LINES[..shown] {
            if y >= rect.bottom() - 1 {
                break;
            }
            buf.set_string(rect.x + 3, y, line, Style::new().fg(theme.dim).bg(theme.dialog_bg));
            y += 1;
        }
        let version = format!(" v{} · press any key ", alouette_core::VERSION);
        let vx = rect.right().saturating_sub(version.len() as u16 + 2);
        buf.set_string(vx, rect.bottom() - 1, version, style);
    }
}
