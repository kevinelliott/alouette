//! The menu bar, Minuet and Turbo Vision style.
//!
//! Labels mark their hotkey with `~`, so `~F~ile` opens with Alt-F and shows a red F.

use alouette_core::config::LayoutKind;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear};
use unicode_width::UnicodeWidthStr;

use crate::chrome;
use crate::keymap::{Action, Keymap};
use crate::module::Launcher;
use crate::theme::Theme;

/// What a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuCmd {
    Action(Action),
    Open(String),
    /// Open the palette with this text typed, for choosers such as themes.
    Palette(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub hint: Option<String>,
    /// `None` draws a separator.
    pub cmd: Option<MenuCmd>,
    pub checked: Option<bool>,
}

impl MenuItem {
    fn action(label: &str, a: Action, km: &Keymap) -> Self {
        Self { label: label.into(), hint: km.key_for(a), cmd: Some(MenuCmd::Action(a)), checked: None }
    }
    fn open(label: &str, url: &str) -> Self {
        Self { label: label.into(), hint: None, cmd: Some(MenuCmd::Open(url.into())), checked: None }
    }
    fn palette(label: &str, query: &str) -> Self {
        Self { label: label.into(), hint: None, cmd: Some(MenuCmd::Palette(query.into())), checked: None }
    }
    fn sep() -> Self {
        Self { label: String::new(), hint: None, cmd: None, checked: None }
    }
    fn check(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
    pub fn is_separator(&self) -> bool {
        self.cmd.is_none()
    }
    /// The hotkey letter, lowercased.
    pub fn hotkey(&self) -> Option<char> {
        hotkey(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn hotkey(&self) -> Option<char> {
        hotkey(&self.title)
    }
}

/// The hotkey marked with `~` in a label.
pub fn hotkey(label: &str) -> Option<char> {
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            return chars.next().map(|c| c.to_ascii_lowercase());
        }
    }
    None
}

/// A label without its `~` markers.
pub fn plain(label: &str) -> String {
    label.replace('~', "")
}

/// Build the menu bar for the current state.
pub fn build(km: &Keymap, layout: LayoutKind, modules: &[(String, Vec<Launcher>)]) -> Vec<Menu> {
    let mut module_items = vec![];
    for (name, launchers) in modules {
        if launchers.is_empty() {
            continue;
        }
        if !module_items.is_empty() {
            module_items.push(MenuItem::sep());
        }
        for l in launchers {
            let label = if launchers.len() == 1 { name.clone() } else { format!("{name}: {}", l.label) };
            module_items.push(MenuItem::open(&label, &l.url));
        }
    }
    vec![
        Menu {
            title: "~≡~".into(),
            items: vec![
                MenuItem::action("~A~bout Alouette", Action::About, km),
                MenuItem::open("About ~M~inuet", "about:minuet"),
                MenuItem::sep(),
                MenuItem::action("E~x~it", Action::Quit, km),
            ],
        },
        Menu {
            title: "~F~ile".into(),
            items: vec![
                MenuItem::action("~O~pen URL…", Action::OpenUrl, km),
                MenuItem::action("~C~lose window", Action::CloseWindow, km),
                MenuItem::sep(),
                MenuItem::action("~H~istory", Action::History, km),
                MenuItem::sep(),
                MenuItem::action("E~x~it", Action::Quit, km),
            ],
        },
        Menu {
            title: "~W~indow".into(),
            items: vec![
                MenuItem::action("~N~ext", Action::NextWindow, km),
                MenuItem::action("~P~revious", Action::PrevWindow, km),
                MenuItem::action("~Z~oom", Action::ZoomWindow, km),
                MenuItem::action("~M~ove or resize", Action::MoveWindow, km),
                MenuItem::action("C~a~scade", Action::Cascade, km),
                MenuItem::sep(),
                MenuItem::action("~D~esktop layout", Action::LayoutDesktop, km).check(layout == LayoutKind::Desktop),
                MenuItem::action("~T~iled layout", Action::LayoutTiled, km).check(layout == LayoutKind::Tiled),
                MenuItem::action("Ta~b~s layout", Action::LayoutTabs, km).check(layout == LayoutKind::Tabs),
                MenuItem::sep(),
                MenuItem::action("Ta~s~ks", Action::Tasks, km),
                MenuItem::palette("~L~ist windows…", "window "),
            ],
        },
        Menu { title: "~M~odules".into(), items: module_items },
        Menu {
            title: "~O~ptions".into(),
            items: vec![
                MenuItem::action("~S~ettings", Action::Settings, km),
                MenuItem::palette("~T~heme…", "theme "),
                MenuItem::palette("~K~eymap…", "keymap "),
                MenuItem::action("~N~ext theme", Action::CycleTheme, km),
            ],
        },
        Menu {
            title: "~H~elp".into(),
            items: vec![
                MenuItem::action("~C~ontents", Action::Help, km),
                MenuItem::action("~K~ey bindings", Action::Keys, km),
                MenuItem::palette("Command ~p~alette", ""),
                MenuItem::sep(),
                MenuItem::action("~A~bout Alouette", Action::About, km),
            ],
        },
    ]
}

/// Open-menu state.
#[derive(Debug, Clone, Default)]
pub struct MenuState {
    /// Which menu is dropped down.
    pub open: Option<usize>,
    pub selected: usize,
}

impl MenuState {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open_menu(&mut self, menus: &[Menu], i: usize) {
        self.open = Some(i.min(menus.len().saturating_sub(1)));
        self.selected = first_selectable(&menus[self.open.unwrap_or(0)].items, 0, true);
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    pub fn move_menu(&mut self, menus: &[Menu], delta: isize) {
        if let Some(i) = self.open {
            let n = menus.len() as isize;
            let j = ((i as isize + delta) % n + n) % n;
            self.open_menu(menus, j as usize);
        }
    }

    pub fn move_item(&mut self, menus: &[Menu], forward: bool) {
        let Some(i) = self.open else { return };
        let items = &menus[i].items;
        if items.is_empty() {
            return;
        }
        let n = items.len();
        let start = if forward { (self.selected + 1) % n } else { (self.selected + n - 1) % n };
        self.selected = first_selectable(items, start, forward);
    }

    pub fn current<'a>(&self, menus: &'a [Menu]) -> Option<&'a MenuItem> {
        menus.get(self.open?)?.items.get(self.selected).filter(|i| !i.is_separator())
    }
}

fn first_selectable(items: &[MenuItem], start: usize, forward: bool) -> usize {
    let n = items.len();
    for k in 0..n {
        let i = if forward { (start + k) % n } else { (start + n - k) % n };
        if !items[i].is_separator() {
            return i;
        }
    }
    0
}

/// Where each menu title sits on the bar.
pub fn title_positions(menus: &[Menu], bar: Rect) -> Vec<Rect> {
    let mut x = bar.x + 1;
    menus
        .iter()
        .map(|m| {
            let w = plain(&m.title).width() as u16 + 2;
            let r = Rect { x, y: bar.y, width: w, height: 1 };
            x += w;
            r
        })
        .collect()
}

/// Draw text with its `~` hotkey highlighted.
fn draw_label(buf: &mut Buffer, x: u16, y: u16, label: &str, base: Style, hot: Style) -> u16 {
    let mut cx = x;
    let mut in_marker = false;
    for ch in label.chars() {
        if ch == '~' {
            in_marker = !in_marker;
            continue;
        }
        let mut tmp = [0u8; 4];
        let s: &str = ch.encode_utf8(&mut tmp);
        buf.set_string(cx, y, s, if in_marker { hot } else { base });
        cx += s.width() as u16;
    }
    cx
}

pub fn draw_bar(buf: &mut Buffer, bar: Rect, menus: &[Menu], state: &MenuState, theme: &Theme) {
    let base = Style::new().fg(theme.menu_fg).bg(theme.menu_bg);
    buf.set_style(bar, base);
    for (i, (m, r)) in menus.iter().zip(title_positions(menus, bar)).enumerate() {
        let open = state.open == Some(i);
        let style = if open { theme.menu_selected() } else { base };
        let hot = if open { style } else { Style::new().fg(theme.menu_hotkey).bg(theme.menu_bg) };
        buf.set_string(r.x, r.y, " ".repeat(r.width as usize), style);
        draw_label(buf, r.x + 1, r.y, &m.title, style, hot);
    }
}

/// The dropdown box's area for menu `i`.
pub fn dropdown_rect(menus: &[Menu], i: usize, bar: Rect, screen: Rect) -> Rect {
    let menu = &menus[i];
    let label_w = menu.items.iter().map(|it| plain(&it.label).width() + 2).max().unwrap_or(10);
    let hint_w = menu.items.iter().filter_map(|it| it.hint.as_ref()).map(|h| h.width() + 2).max().unwrap_or(0);
    let w = (label_w + hint_w + 4).max(16) as u16;
    let h = menu.items.len() as u16 + 2;
    let pos = title_positions(menus, bar)[i];
    let x = pos.x.min(screen.right().saturating_sub(w + 2));
    Rect { x, y: bar.bottom(), width: w.min(screen.width), height: h.min(screen.height.saturating_sub(1)) }
}

pub fn draw_dropdown(frame: &mut Frame, menus: &[Menu], state: &MenuState, bar: Rect, theme: &Theme, shadows: bool) {
    let Some(i) = state.open else { return };
    let screen = frame.area();
    let rect = dropdown_rect(menus, i, bar, screen);
    let base = Style::new().fg(theme.menu_fg).bg(theme.menu_bg);
    frame.render_widget(Clear, rect);
    frame.render_widget(Block::default().borders(Borders::ALL).border_style(base).style(base), rect);
    if shadows {
        chrome::shadow(frame.buffer_mut(), rect, screen, theme);
    }
    let buf = frame.buffer_mut();
    let menu = &menus[i];
    if menu.items.is_empty() {
        buf.set_string(rect.x + 2, rect.y + 1, "(nothing yet)", Style::new().fg(theme.menu_disabled).bg(theme.menu_bg));
        return;
    }
    for (row, item) in menu.items.iter().enumerate() {
        let y = rect.y + 1 + row as u16;
        if y >= rect.bottom() - 1 {
            break;
        }
        if item.is_separator() {
            let line = format!("├{}┤", "─".repeat(rect.width.saturating_sub(2) as usize));
            buf.set_string(rect.x, y, line, base);
            continue;
        }
        let selected = row == state.selected;
        let style = if selected { theme.menu_selected() } else { base };
        let hot = if selected {
            style.add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.menu_hotkey).bg(theme.menu_bg)
        };
        buf.set_string(rect.x + 1, y, " ".repeat(rect.width.saturating_sub(2) as usize), style);
        let mark = match item.checked {
            Some(true) => "•",
            _ => " ",
        };
        buf.set_string(rect.x + 1, y, mark, style);
        draw_label(buf, rect.x + 2, y, &item.label, style, hot);
        if let Some(h) = &item.hint {
            let hx = rect.right().saturating_sub(h.width() as u16 + 2);
            buf.set_string(hx, y, h, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn hotkeys_and_plain_labels() {
        assert_eq!(hotkey("~F~ile"), Some('f'));
        assert_eq!(hotkey("E~x~it"), Some('x'));
        assert_eq!(plain("E~x~it"), "Exit");
        assert_eq!(hotkey("Nothing"), None);
    }

    #[test]
    fn menu_navigation_skips_separators() {
        let km = Keymap::new(Default::default(), &BTreeMap::new());
        let menus = build(&km, LayoutKind::Desktop, &[]);
        let mut st = MenuState::default();
        st.open_menu(&menus, 1); // File
        assert_eq!(st.current(&menus).unwrap().label, "~O~pen URL…");
        st.move_item(&menus, true);
        st.move_item(&menus, true); // skips the separator
        assert_eq!(st.current(&menus).unwrap().label, "~H~istory");
        st.move_menu(&menus, -2);
        assert_eq!(st.open, Some(menus.len() - 1));
    }
}
