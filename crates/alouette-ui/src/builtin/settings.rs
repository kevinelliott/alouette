use alouette_core::Config;
use alouette_core::config::{BorderStyle, KeymapPreset, LayoutKind};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use crate::window::{Cx, DrawCx, Handled, Window};

/// One row in the settings list.
struct Setting {
    section: &'static str,
    label: &'static str,
    value: fn(&Config) -> String,
    /// Change the value: `forward` is false for Left / Shift.
    change: Option<fn(&mut Config, bool)>,
}

fn onoff(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

fn cycle<T: Copy + PartialEq>(all: &[T], cur: T, forward: bool) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0);
    let n = all.len();
    all[if forward { (i + 1) % n } else { (i + n - 1) % n }]
}

const THEMES: &[&str] = &["minuet", "turbo", "amber", "green", "modern-dark", "modern-light", "high-contrast", "mono"];
const BORDERS: &[BorderStyle] = &[
    BorderStyle::Theme,
    BorderStyle::Rounded,
    BorderStyle::Plain,
    BorderStyle::Double,
    BorderStyle::Thick,
    BorderStyle::Ascii,
];

fn settings() -> Vec<Setting> {
    vec![
        Setting {
            section: "Look and feel",
            label: "Layout",
            value: |c| c.general.layout.name().into(),
            change: Some(|c, f| c.general.layout = cycle(&LayoutKind::ALL, c.general.layout, f)),
        },
        Setting {
            section: "Look and feel",
            label: "Theme",
            value: |c| c.general.theme.clone(),
            change: Some(|c, f| {
                let i = THEMES.iter().position(|t| *t == c.general.theme).unwrap_or(0);
                let n = THEMES.len();
                c.general.theme = THEMES[if f { (i + 1) % n } else { (i + n - 1) % n }].into();
            }),
        },
        Setting {
            section: "Look and feel",
            label: "Keymap",
            value: |c| c.general.keymap.name().into(),
            change: Some(|c, f| c.general.keymap = cycle(&KeymapPreset::ALL, c.general.keymap, f)),
        },
        Setting {
            section: "Look and feel",
            label: "Borders",
            value: |c| format!("{:?}", c.general.border).to_lowercase(),
            change: Some(|c, f| c.general.border = cycle(BORDERS, c.general.border, f)),
        },
        Setting {
            section: "Look and feel",
            label: "Mouse",
            value: |c| onoff(c.general.mouse),
            change: Some(|c, _| c.general.mouse = !c.general.mouse),
        },
        Setting {
            section: "Look and feel",
            label: "Clock",
            value: |c| onoff(c.general.clock),
            change: Some(|c, _| c.general.clock = !c.general.clock),
        },
        Setting {
            section: "Nostalgia",
            label: "All nostalgia",
            value: |c| onoff(c.nostalgia.enabled),
            change: Some(|c, _| c.nostalgia.enabled = !c.nostalgia.enabled),
        },
        Setting {
            section: "Nostalgia",
            label: "Splash screen",
            value: |c| onoff(c.nostalgia.splash),
            change: Some(|c, _| c.nostalgia.splash = !c.nostalgia.splash),
        },
        Setting {
            section: "Nostalgia",
            label: "Minuet names",
            value: |c| onoff(c.nostalgia.minuet_names),
            change: Some(|c, _| c.nostalgia.minuet_names = !c.nostalgia.minuet_names),
        },
        Setting {
            section: "Nostalgia",
            label: "Drop shadows",
            value: |c| onoff(c.nostalgia.shadows),
            change: Some(|c, _| c.nostalgia.shadows = !c.nostalgia.shadows),
        },
        Setting {
            section: "Nostalgia",
            label: "Desktop pattern",
            value: |c| onoff(c.nostalgia.desktop_pattern),
            change: Some(|c, _| c.nostalgia.desktop_pattern = !c.nostalgia.desktop_pattern),
        },
        Setting {
            section: "Nostalgia",
            label: "Sounds",
            value: |c| onoff(c.nostalgia.sounds),
            change: Some(|c, _| c.nostalgia.sounds = !c.nostalgia.sounds),
        },
        Setting {
            section: "Nostalgia",
            label: "Retro commands",
            value: |c| onoff(c.nostalgia.easter_eggs),
            change: Some(|c, _| c.nostalgia.easter_eggs = !c.nostalgia.easter_eggs),
        },
        Setting {
            section: "Privacy",
            label: "Offline cache",
            value: |c| onoff(c.cache.enabled),
            change: Some(|c, _| c.cache.enabled = !c.cache.enabled),
        },
        Setting {
            section: "Privacy",
            label: "AI assistant",
            value: |c| if c.ai.enabled { "on".into() } else { "off (providers arrive in M8)".into() },
            change: Some(|c, _| c.ai.enabled = !c.ai.enabled),
        },
        Setting {
            section: "Network",
            label: "Prefer IPv6",
            value: |c| onoff(c.network.prefer_ipv6),
            change: Some(|c, _| c.network.prefer_ipv6 = !c.network.prefer_ipv6),
        },
        Setting { section: "Network", label: "DNS", value: |c| c.network.dns.clone(), change: None },
    ]
}

/// Common settings, changed in place and saved to alouette.toml.
pub struct SettingsWindow {
    state: ListState,
    rows: Vec<Setting>,
}

impl Default for SettingsWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsWindow {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { state, rows: settings() }
    }

    fn change(&mut self, forward: bool, cx: &mut Cx) {
        let Some(i) = self.state.selected() else { return };
        if let Some(f) = self.rows[i].change {
            cx.edit_config(move |c| f(c, forward));
        } else {
            cx.notify(alouette_core::Notification::info("Change this one in alouette.toml"));
        }
    }
}

impl Window for SettingsWindow {
    fn title(&self) -> String {
        "Settings".into()
    }
    fn module(&self) -> &'static str {
        "core"
    }
    fn url(&self) -> Option<String> {
        Some("about:settings".into())
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let mut items = vec![];
        let mut last = "";
        let mut index_map = vec![];
        for (i, s) in self.rows.iter().enumerate() {
            if s.section != last {
                if !last.is_empty() {
                    items.push(ListItem::new(Line::default()));
                    index_map.push(None);
                }
                items.push(ListItem::new(Line::from(Span::styled(s.section, t.accent()))));
                index_map.push(None);
                last = s.section;
            }
            let value = (s.value)(cx.config);
            let value_style =
                if s.change.is_some() { Style::new().fg(t.link).add_modifier(Modifier::BOLD) } else { t.dim() };
            items.push(ListItem::new(Line::from(vec![
                Span::raw(format!("  {:<20}", s.label)),
                Span::styled(format!("‹ {value} ›"), value_style),
            ])));
            index_map.push(Some(i));
        }
        let footer_h = 2u16.min(area.height);
        let list_area = Rect { height: area.height.saturating_sub(footer_h), ..area };
        let sel = self.state.selected().unwrap_or(0);
        let visual = index_map.iter().position(|m| *m == Some(sel)).unwrap_or(0);
        let mut vis_state = ListState::default().with_selected(Some(visual));
        *vis_state.offset_mut() = visual.saturating_sub(list_area.height.saturating_sub(1) as usize);
        let list =
            List::new(items).style(t.window()).highlight_style(if cx.focused { t.selection() } else { Style::new() });
        frame.render_stateful_widget(list, list_area, &mut vis_state);
        let footer = Rect { y: area.bottom().saturating_sub(footer_h), height: footer_h, ..area };
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                "Enter or ←→ changes a setting. Changes save to alouette.toml at once.",
                t.dim(),
            ))])
            .style(t.window()),
            footer,
        );
    }
    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let n = self.rows.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.select(Some((self.state.selected().unwrap_or(0) + 1).min(n - 1)))
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => self.change(true, cx),
            KeyCode::Left | KeyCode::Char('h') => self.change(false, cx),
            _ => return Handled::No,
        }
        Handled::Yes
    }
    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollUp => self.state.select(Some(self.state.selected().unwrap_or(0).saturating_sub(1))),
            MouseEventKind::ScrollDown => {
                self.state.select(Some((self.state.selected().unwrap_or(0) + 1).min(self.rows.len() - 1)))
            }
            MouseEventKind::Down(_) => self.change(true, cx),
            _ => return Handled::No,
        }
        Handled::Yes
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Enter", "Change"), ("←→", "Cycle")]
    }
}
