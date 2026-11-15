//! The terminal window: draws the emulator grid and sends keys to the backend.

use std::sync::Arc;

use alouette_ui::window::{AiContext, Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::emu::{Input, Shared, Status};
use crate::keys;

/// A terminal session shown in a window.
pub struct TermWindow {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Input>,
    module: &'static str,
    base_title: String,
    title: String,
    url: String,
    size: (u16, u16),
    /// Ctrl+] was pressed: the next key goes to Alouette, not the remote side.
    escape: bool,
    answer: String,
    /// The inside of the window at the last draw, for mouse positions.
    area: Rect,
    bell_until: Option<std::time::Instant>,
}

/// The key that hands the next key to Alouette, as in telnet.
pub const ESCAPE_HINT: &str = "Ctrl+]";

impl TermWindow {
    pub fn new(
        module: &'static str,
        title: impl Into<String>,
        url: impl Into<String>,
        shared: Arc<Shared>,
        tx: mpsc::UnboundedSender<Input>,
    ) -> Self {
        let title = title.into();
        Self {
            shared,
            tx,
            module,
            base_title: title.clone(),
            title,
            url: url.into(),
            size: (0, 0),
            escape: false,
            answer: String::new(),
            area: Rect::default(),
            bell_until: None,
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    fn send(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.tx.send(Input::Bytes(bytes));
        }
    }

    fn snap_to_bottom(&self) {
        let mut p = self.shared.parser();
        if p.screen().scrollback() > 0 {
            p.screen_mut().set_scrollback(0);
        }
    }

    fn scroll(&self, delta: isize) {
        let mut p = self.shared.parser();
        let rows = p.screen().size().0.get() as isize;
        let cur = p.screen().scrollback() as isize;
        let step = if delta.abs() == 1 { delta * 3 } else { delta.signum() * (rows - 1).max(1) };
        p.screen_mut().set_scrollback((cur + step).max(0) as usize);
    }

    fn draw_grid(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let mut p = self.shared.parser();
        // Pick up a title set by the remote program.
        if let Some(t) = p.callbacks_mut().title.take() {
            self.title = if t.is_empty() { self.base_title.clone() } else { format!("{} · {t}", self.base_title) };
        }
        if std::mem::take(&mut p.callbacks_mut().bell) {
            self.bell_until = Some(cx.now + std::time::Duration::from_millis(150));
        }
        let screen = p.screen();
        let buf = frame.buffer_mut();
        let (rows, cols) = (area.height, area.width);
        for r in 0..rows {
            let mut c = 0;
            while c < cols {
                let Some(cell) = screen.cell(r, c) else { break };
                let pos = (area.x + c, area.y + r);
                if cell.is_wide_continuation() {
                    c += 1;
                    continue;
                }
                let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                let mut m = Modifier::empty();
                if cell.bold() {
                    m |= Modifier::BOLD;
                }
                if cell.dim() {
                    m |= Modifier::DIM;
                }
                if cell.italic() {
                    m |= Modifier::ITALIC;
                }
                if cell.underline() {
                    m |= Modifier::UNDERLINED;
                }
                if cell.inverse() {
                    m |= Modifier::REVERSED;
                }
                style = style.add_modifier(m);
                if let Some(b) = buf.cell_mut(pos) {
                    let s = cell.contents();
                    b.set_symbol(if s.is_empty() { " " } else { s });
                    b.set_style(style);
                }
                c += 1;
            }
        }
        let flash = self.bell_until.is_some_and(|t| cx.now < t);
        if flash {
            buf.set_style(Rect { height: 1, ..area }, Style::new().add_modifier(Modifier::REVERSED));
        }
        let scrolled = screen.scrollback();
        if scrolled > 0 {
            let tag = format!(" scrolled back {scrolled} lines · Shift+PgDn ");
            let w = (tag.chars().count() as u16).min(area.width);
            let r = Rect { x: area.right() - w, y: area.y, width: w, height: 1 };
            frame.render_widget(Paragraph::new(tag).style(cx.theme.selection()), r);
        } else if cx.focused && !screen.hide_cursor() && self.shared.question().is_none() {
            let (cr, cc) = screen.cursor_position();
            if cr < rows && cc < cols {
                frame.set_cursor_position((area.x + cc, area.y + cr));
            }
        }
    }

    fn draw_question(&self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if let Some(q) = self.shared.question() {
            alouette_ui::ask::draw(frame, area, cx.theme, &q, &self.answer);
        }
    }
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

impl Window for TermWindow {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn module(&self) -> &'static str {
        self.module
    }
    fn url(&self) -> Option<String> {
        Some(self.url.clone())
    }
    fn badge(&self) -> Option<String> {
        self.shared.badge()
    }
    fn ai_context(&self) -> Option<AiContext> {
        let text = self.shared.parser().screen().contents();
        let text = text.trim_end().to_string();
        if text.is_empty() {
            return None;
        }
        Some(AiContext {
            module: self.module.into(),
            kind: "terminal output".into(),
            title: self.title.clone(),
            text,
            list: false,
            reply_url: None,
        })
    }
    fn captures_keys(&self) -> bool {
        matches!(self.shared.status(), Status::Running | Status::Connecting(_))
    }
    fn wants_text(&self) -> bool {
        true
    }
    fn animates(&self) -> bool {
        self.bell_until.is_some() || matches!(self.shared.status(), Status::Connecting(_))
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.shared.status() {
            Status::Exited(_) => vec![("Enter", "Close"), ("r", "Reconnect")],
            _ if self.escape => vec![("Tab", "Next window"), ("]", "Send Ctrl+]"), ("any key", "Alouette shortcut")],
            _ => vec![(ESCAPE_HINT, "Alouette keys"), ("Shift+PgUp", "Scroll back")],
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let status = self.shared.status();
        let (grid, footer) = match &status {
            Status::Exited(_) | Status::Connecting(_) if area.height > 2 => {
                (Rect { height: area.height - 1, ..area }, Some(Rect { y: area.bottom() - 1, height: 1, ..area }))
            }
            _ => (area, None),
        };
        self.area = grid;
        if self.size != (grid.height, grid.width) {
            self.size = (grid.height, grid.width);
            crate::emu::resize(&self.shared, grid.height, grid.width);
            let _ = self.tx.send(Input::Resize { cols: grid.width, rows: grid.height });
        }
        self.draw_grid(frame, grid, cx);
        if let Some(f) = footer {
            let (text, style) = match &status {
                Status::Connecting(what) => (format!(" {what}…"), cx.theme.dim()),
                Status::Exited(why) => (
                    format!(" {why}. Enter closes, r reconnects."),
                    Style::new().fg(cx.theme.selection_fg).bg(cx.theme.selection_bg),
                ),
                Status::Running => (String::new(), Style::new()),
            };
            frame.render_widget(Paragraph::new(text).style(style), f);
        }
        self.draw_question(frame, area, cx);
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.shared.question() {
            alouette_ui::ask::on_key(self.shared.asker(), &q, &mut self.answer, key);
            return Handled::Yes;
        }
        if let Status::Exited(_) = self.shared.status() {
            return match key.code {
                KeyCode::Enter | KeyCode::Esc => {
                    cx.close();
                    Handled::Yes
                }
                KeyCode::Char('r') => {
                    cx.open_url(self.url.clone());
                    cx.close();
                    Handled::Yes
                }
                KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.scroll(1 << 8);
                    Handled::Yes
                }
                _ => Handled::No,
            };
        }
        if self.escape {
            self.escape = false;
            if key.code == KeyCode::Char(']') {
                self.send(vec![0x1d]);
                return Handled::Yes;
            }
            // Hand the key to Alouette. Plain keys map to their Ctrl form so that Ctrl+] then
            // Tab or p works like the shell's own shortcut.
            return Handled::No;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5')) {
            self.escape = true;
            return Handled::Yes;
        }
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => {
                    self.scroll(2);
                    return Handled::Yes;
                }
                KeyCode::PageDown => {
                    self.scroll(-2);
                    return Handled::Yes;
                }
                _ => {}
            }
        }
        self.snap_to_bottom();
        let app_cursor = self.shared.parser().screen().application_cursor();
        match keys::encode(&key, app_cursor) {
            Some(b) => {
                self.send(b);
                Handled::Yes
            }
            None => Handled::No,
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.shared.question().is_some() {
            self.answer.push_str(text.lines().next().unwrap_or(""));
            return Handled::Yes;
        }
        self.snap_to_bottom();
        let bracketed = self.shared.parser().screen().bracketed_paste();
        self.send(keys::paste(text, bracketed));
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        let a = self.area;
        let col = ev.column.saturating_sub(a.x).min(a.width.saturating_sub(1));
        let row = ev.row.saturating_sub(a.y).min(a.height.saturating_sub(1));
        let (mode, sgr, alt, app_cursor) = {
            let p = self.shared.parser();
            let s = p.screen();
            (
                s.mouse_protocol_mode(),
                s.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr,
                s.alternate_screen(),
                s.application_cursor(),
            )
        };
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        if !shift && let Some(b) = keys::mouse(&ev, col, row, mode, sgr) {
            self.send(b);
            return Handled::Yes;
        }
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = ev.kind == MouseEventKind::ScrollUp;
                if alt && !shift {
                    // Full-screen programs without mouse support: scroll with arrow keys.
                    let k = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
                    let b = keys::encode(&k, app_cursor).unwrap_or_default();
                    self.send(b.repeat(3));
                } else {
                    self.scroll(if up { 1 } else { -1 });
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}
