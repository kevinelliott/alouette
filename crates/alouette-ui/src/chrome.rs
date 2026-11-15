//! Drawing the shell's own parts: desktop, window frames, shadows, tab bar, status line
//! and toasts. Windows only draw their insides.

use std::time::Instant;

use alouette_core::config::BorderStyle;
use alouette_core::{Level, Notification};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Plain ASCII borders for terminals without box drawing.
pub const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// A block with the configured border style.
pub fn block<'a>(theme: &Theme, setting: BorderStyle, active: bool) -> Block<'a> {
    let b = Block::default().borders(Borders::ALL);
    match theme.border_type(setting) {
        Some(t) => {
            // Minuet and Turbo Vision draw inactive windows with single lines.
            let t = if !active && t == ratatui::widgets::BorderType::Double {
                ratatui::widgets::BorderType::Plain
            } else {
                t
            };
            b.border_type(t)
        }
        None => b.border_set(ASCII_BORDER),
    }
}

/// Fill the desktop background.
pub fn desktop(buf: &mut Buffer, area: Rect, theme: &Theme, pattern: bool) {
    let ch = if pattern && theme.pattern != ' ' { theme.pattern } else { ' ' };
    let style = Style::new().fg(theme.desktop_fg).bg(theme.desktop_bg);
    let mut s = [0u8; 4];
    let sym: &str = ch.encode_utf8(&mut s);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(sym).set_style(style);
            }
        }
    }
}

/// Darken the cells to the right of and below `rect`, like a DOS drop shadow.
pub fn shadow(buf: &mut Buffer, rect: Rect, clip: Rect, theme: &Theme) {
    let style = if theme.is_mono() {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new().fg(theme.shadow_fg).bg(theme.shadow_bg)
    };
    let right = Rect { x: rect.right(), y: rect.y + 1, width: 2, height: rect.height };
    let below = Rect { x: rect.x + 2, y: rect.bottom(), width: rect.width, height: 1 };
    for r in [right, below] {
        let r = r.intersection(clip);
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(style);
                }
            }
        }
    }
}

/// Where the clickable boxes sit on a window frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameHits {
    pub close: Option<Rect>,
    pub zoom: Option<Rect>,
    pub title_bar: Rect,
    pub resize: Option<Rect>,
}

pub struct FrameSpec<'a> {
    pub title: &'a str,
    pub number: Option<usize>,
    pub focused: bool,
    pub zoomed: bool,
    /// Draw the close and zoom boxes (Desktop layout only).
    pub boxes: bool,
    pub border: BorderStyle,
}

/// Draw a window frame and return the inside area plus clickable spots.
pub fn window_frame(frame: &mut Frame, rect: Rect, spec: &FrameSpec, theme: &Theme) -> (Rect, FrameHits) {
    frame.render_widget(Clear, rect);
    let border_style = if spec.focused {
        Style::new().fg(theme.border_active_fg).bg(theme.window_bg)
    } else {
        Style::new().fg(theme.border_fg).bg(theme.window_bg)
    };
    let title_style = if spec.focused {
        if theme.is_mono() {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new().fg(theme.title_active_fg).bg(theme.title_active_bg).add_modifier(Modifier::BOLD)
        }
    } else {
        Style::new().fg(theme.title_fg).bg(theme.window_bg)
    };
    let blk = block(theme, spec.border, spec.focused).border_style(border_style).style(theme.window());
    let inner = blk.inner(rect);
    frame.render_widget(blk, rect);

    let mut hits = FrameHits { title_bar: Rect { height: 1.min(rect.height), ..rect }, ..Default::default() };
    if rect.width < 8 || rect.height < 2 {
        return (inner, hits);
    }
    let buf = frame.buffer_mut();
    let y = rect.y;
    let mut left_used = rect.x + 1;
    let mut right_used = rect.right() - 1;
    if spec.boxes && rect.width >= 20 {
        let close = Rect { x: rect.x + 2, y, width: 3, height: 1 };
        buf.set_string(close.x, y, "[■]", border_style);
        hits.close = Some(close);
        left_used = close.right() + 1;
        let zoom = Rect { x: rect.right() - 5, y, width: 3, height: 1 };
        buf.set_string(zoom.x, y, if spec.zoomed { "[↕]" } else { "[↑]" }, border_style);
        hits.zoom = Some(zoom);
        right_used = zoom.x - 1;
        hits.resize = Some(Rect { x: rect.right() - 2, y: rect.bottom() - 1, width: 2, height: 1 });
    }
    if let Some(n) = spec.number {
        let label = format!(" {n} ");
        let x = right_used.saturating_sub(label.len() as u16);
        if x > left_used {
            buf.set_string(x, y, &label, border_style);
            right_used = x;
        }
    }
    let room = right_used.saturating_sub(left_used).saturating_sub(2) as usize;
    if room > 2 {
        let title = truncate(spec.title, room.saturating_sub(2));
        let label = format!(" {title} ");
        let w = label.width() as u16;
        let x = left_used + (right_used - left_used).saturating_sub(w) / 2;
        buf.set_string(x, y, &label, title_style);
    }
    (inner, hits)
}

/// Cut text to `max` display columns, adding an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// The tab bar for the Tabs layout. Returns each tab's clickable area.
pub fn tab_bar(buf: &mut Buffer, area: Rect, tabs: &[(String, bool)], theme: &Theme) -> Vec<Rect> {
    let base = Style::new().fg(theme.status_fg).bg(theme.status_bg);
    buf.set_style(area, base);
    let mut x = area.x;
    let mut hits = vec![];
    for (i, (title, active)) in tabs.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, truncate(title, 24));
        let w = label.width() as u16;
        if x + w > area.right() {
            break;
        }
        let style = if *active { theme.menu_selected().add_modifier(Modifier::BOLD) } else { base };
        buf.set_string(x, area.y, &label, style);
        hits.push(Rect { x, y: area.y, width: w, height: 1 });
        x += w + 1;
    }
    hits
}

/// The status line: key hints on the left, state on the right.
pub fn status_line(buf: &mut Buffer, area: Rect, hints: &[(String, String)], right: &str, theme: &Theme) {
    let base = Style::new().fg(theme.status_fg).bg(theme.status_bg);
    let key = Style::new().fg(theme.status_key).bg(theme.status_bg).add_modifier(Modifier::BOLD);
    buf.set_style(area, base);
    let right_w = right.width() as u16;
    let limit = area.right().saturating_sub(right_w + 1);
    let mut x = area.x + 1;
    for (k, label) in hints {
        let need = (k.width() + label.width() + 3) as u16;
        if x + need > limit {
            break;
        }
        buf.set_string(x, area.y, k, key);
        x += k.width() as u16 + 1;
        buf.set_string(x, area.y, label, base);
        x += label.width() as u16 + 2;
    }
    if right_w < area.width {
        buf.set_string(area.right() - right_w - 1, area.y, right, base);
    }
}

/// Toasts stacked in the bottom-right corner, newest at the bottom.
pub fn toasts(frame: &mut Frame, area: Rect, items: &[Notification], theme: &Theme, now: Instant) {
    let width = 44.min(area.width.saturating_sub(2));
    if width < 12 {
        return;
    }
    let mut bottom = area.bottom();
    for n in items.iter().rev().filter(|n| !n.expired(now)).take(4) {
        let text_w = width.saturating_sub(4) as usize;
        let lines = textwrap_lines(&n.text, text_w);
        let h = (lines as u16 + 2).min(6);
        if bottom < area.y + h {
            break;
        }
        let rect = Rect { x: area.right().saturating_sub(width + 1), y: bottom - h, width, height: h };
        let color = match n.level {
            Level::Info => theme.accent,
            Level::Success => theme.success,
            Level::Warning => theme.warning,
            Level::Error => theme.error,
        };
        let icon = match n.level {
            Level::Info => "i",
            Level::Success => "✓",
            Level::Warning => "!",
            Level::Error => "✗",
        };
        frame.render_widget(Clear, rect);
        let blk = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(color).bg(theme.dialog_bg))
            .style(Style::new().fg(theme.dialog_fg).bg(theme.dialog_bg));
        let para = Paragraph::new(Line::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw(n.text.clone()),
        ]))
        .wrap(Wrap { trim: true })
        .block(blk);
        frame.render_widget(para, rect);
        bottom = rect.y;
    }
}

fn textwrap_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut lines = 1;
    let mut col = 0;
    for word in text.split_whitespace() {
        let w = word.width() + 1;
        if col + w > width && col > 0 {
            lines += 1;
            col = 0;
        }
        col += w;
    }
    lines
}

/// Center a box of the given size inside `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("Welcome", 20), "Welcome");
        assert_eq!(truncate("A very long window title", 10), "A very lo…");
    }

    #[test]
    fn centered_box_fits() {
        let r = centered(Rect::new(0, 0, 80, 24), 40, 10);
        assert_eq!(r, Rect::new(20, 7, 40, 10));
        let r = centered(Rect::new(0, 0, 30, 5), 40, 10);
        assert_eq!(r, Rect::new(0, 0, 30, 5));
    }
}
