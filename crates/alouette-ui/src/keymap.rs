//! Key bindings.
//!
//! Three presets ship: **CUA** (Minuet and Turbo Vision style F-keys and Alt menus),
//! **Vim** (`Ctrl-W` window commands, `:` for the palette) and **Emacs** (`C-x` prefixes,
//! `M-x` for the palette). Any binding can be overridden in the `[keys]` config table, and
//! bindings may be two-key sequences such as `ctrl+w h`.

use std::collections::BTreeMap;
use std::fmt;

use alouette_core::config::{KeySpec, KeymapPreset};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Everything a key can do at the shell level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Quit,
    Palette,
    OpenUrl,
    Menu,
    NextWindow,
    PrevWindow,
    CloseWindow,
    ZoomWindow,
    MoveWindow,
    CycleLayout,
    LayoutDesktop,
    LayoutTiled,
    LayoutTabs,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    GrowMaster,
    ShrinkMaster,
    Cascade,
    Tasks,
    Help,
    Keys,
    Settings,
    History,
    About,
    CycleTheme,
    Redraw,
    FocusWindow1,
    FocusWindow2,
    FocusWindow3,
    FocusWindow4,
    FocusWindow5,
    FocusWindow6,
    FocusWindow7,
    FocusWindow8,
    FocusWindow9,
}

impl Action {
    /// Every action, for the palette and the keys window.
    pub const ALL: &'static [Action] = &[
        Action::Palette,
        Action::OpenUrl,
        Action::Menu,
        Action::NextWindow,
        Action::PrevWindow,
        Action::CloseWindow,
        Action::ZoomWindow,
        Action::MoveWindow,
        Action::CycleLayout,
        Action::LayoutDesktop,
        Action::LayoutTiled,
        Action::LayoutTabs,
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::GrowMaster,
        Action::ShrinkMaster,
        Action::Cascade,
        Action::Tasks,
        Action::Help,
        Action::Keys,
        Action::Settings,
        Action::History,
        Action::About,
        Action::CycleTheme,
        Action::Redraw,
        Action::FocusWindow1,
        Action::FocusWindow2,
        Action::FocusWindow3,
        Action::FocusWindow4,
        Action::FocusWindow5,
        Action::FocusWindow6,
        Action::FocusWindow7,
        Action::FocusWindow8,
        Action::FocusWindow9,
        Action::Quit,
    ];

    /// The name used in the `[keys]` config table.
    pub fn id(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Palette => "palette",
            Action::OpenUrl => "open_url",
            Action::Menu => "menu",
            Action::NextWindow => "next_window",
            Action::PrevWindow => "prev_window",
            Action::CloseWindow => "close_window",
            Action::ZoomWindow => "zoom_window",
            Action::MoveWindow => "move_window",
            Action::CycleLayout => "cycle_layout",
            Action::LayoutDesktop => "layout_desktop",
            Action::LayoutTiled => "layout_tiled",
            Action::LayoutTabs => "layout_tabs",
            Action::FocusLeft => "focus_left",
            Action::FocusRight => "focus_right",
            Action::FocusUp => "focus_up",
            Action::FocusDown => "focus_down",
            Action::GrowMaster => "grow_master",
            Action::ShrinkMaster => "shrink_master",
            Action::Cascade => "cascade",
            Action::Tasks => "tasks",
            Action::Help => "help",
            Action::Keys => "keys",
            Action::Settings => "settings",
            Action::History => "history",
            Action::About => "about",
            Action::CycleTheme => "cycle_theme",
            Action::Redraw => "redraw",
            Action::FocusWindow1 => "window_1",
            Action::FocusWindow2 => "window_2",
            Action::FocusWindow3 => "window_3",
            Action::FocusWindow4 => "window_4",
            Action::FocusWindow5 => "window_5",
            Action::FocusWindow6 => "window_6",
            Action::FocusWindow7 => "window_7",
            Action::FocusWindow8 => "window_8",
            Action::FocusWindow9 => "window_9",
        }
    }

    /// A human label for menus and the palette.
    pub fn label(self) -> &'static str {
        match self {
            Action::Quit => "Exit Alouette",
            Action::Palette => "Command palette",
            Action::OpenUrl => "Open URL…",
            Action::Menu => "Menu bar",
            Action::NextWindow => "Next window",
            Action::PrevWindow => "Previous window",
            Action::CloseWindow => "Close window",
            Action::ZoomWindow => "Zoom window",
            Action::MoveWindow => "Move or resize window",
            Action::CycleLayout => "Switch layout",
            Action::LayoutDesktop => "Layout: Desktop",
            Action::LayoutTiled => "Layout: Tiled",
            Action::LayoutTabs => "Layout: Tabs",
            Action::FocusLeft => "Focus window to the left",
            Action::FocusRight => "Focus window to the right",
            Action::FocusUp => "Focus window above",
            Action::FocusDown => "Focus window below",
            Action::GrowMaster => "Widen main pane",
            Action::ShrinkMaster => "Narrow main pane",
            Action::Cascade => "Cascade windows",
            Action::Tasks => "Tasks",
            Action::Help => "Help",
            Action::Keys => "Key bindings",
            Action::Settings => "Settings",
            Action::History => "History",
            Action::About => "About Alouette",
            Action::CycleTheme => "Next theme",
            Action::Redraw => "Redraw screen",
            Action::FocusWindow1 => "Window 1",
            Action::FocusWindow2 => "Window 2",
            Action::FocusWindow3 => "Window 3",
            Action::FocusWindow4 => "Window 4",
            Action::FocusWindow5 => "Window 5",
            Action::FocusWindow6 => "Window 6",
            Action::FocusWindow7 => "Window 7",
            Action::FocusWindow8 => "Window 8",
            Action::FocusWindow9 => "Window 9",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == id)
    }

    /// For FocusWindowN, the 0-based window number.
    pub fn window_number(self) -> Option<usize> {
        Some(match self {
            Action::FocusWindow1 => 0,
            Action::FocusWindow2 => 1,
            Action::FocusWindow3 => 2,
            Action::FocusWindow4 => 3,
            Action::FocusWindow5 => 4,
            Action::FocusWindow6 => 5,
            Action::FocusWindow7 => 6,
            Action::FocusWindow8 => 7,
            Action::FocusWindow9 => 8,
            _ => return None,
        })
    }
}

/// One key with modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    pub const fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self { code, mods }
    }

    /// Normalize a terminal key event so it compares equal to a parsed chord.
    pub fn from_event(ev: &KeyEvent) -> Self {
        let mut mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match ev.code {
            // Terminals report Shift+Tab as BackTab, sometimes with SHIFT set.
            KeyCode::BackTab => {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            // Shift is already in the character for printable keys.
            KeyCode::Char(c) => {
                if c.is_ascii_alphabetic() && mods.contains(KeyModifiers::CONTROL) {
                    mods.remove(KeyModifiers::SHIFT);
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    mods.remove(KeyModifiers::SHIFT);
                    KeyCode::Char(c)
                }
            }
            other => other,
        };
        Self { code, mods }
    }

    /// Parse `ctrl+k`, `alt+x`, `f10`, `shift+f6`, `ctrl+space`, `:`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        // A lone `+` or a trailing `+` key, such as `ctrl++`.
        let (mod_part, key_part) = match s.strip_suffix("++") {
            Some(m) => (format!("{m}+"), "+".to_string()),
            None if s == "+" => (String::new(), "+".to_string()),
            None => match s.rsplit_once('+') {
                Some((m, k)) => (format!("{m}+"), k.to_string()),
                None => (String::new(), s.to_string()),
            },
        };
        let mut mods = KeyModifiers::NONE;
        for m in mod_part.split('+').filter(|m| !m.is_empty()) {
            mods |= match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
                "alt" | "meta" | "m" | "option" => KeyModifiers::ALT,
                "shift" | "s" => KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier `{other}` in `{s}`")),
            };
        }
        let lower = key_part.to_ascii_lowercase();
        let code = match lower.as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n: u8 = f[1..].parse().map_err(|_| format!("bad function key `{s}`"))?;
                if !(1..=24).contains(&n) {
                    return Err(format!("function keys go from f1 to f24: `{s}`"));
                }
                KeyCode::F(n)
            }
            _ => {
                let mut chars = key_part.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => {
                        if c.is_ascii_uppercase() && !mods.contains(KeyModifiers::CONTROL) {
                            KeyCode::Char(c)
                        } else {
                            KeyCode::Char(if mods.contains(KeyModifiers::CONTROL) { c.to_ascii_lowercase() } else { c })
                        }
                    }
                    _ => return Err(format!("unknown key `{key_part}` in `{s}`")),
                }
            }
        };
        if let KeyCode::Char(_) = code {
            // Shift is folded into the character.
            mods.remove(KeyModifiers::SHIFT);
        }
        Ok(Self { code, mods })
    }
}

impl fmt::Display for KeyChord {
    /// Compact form for the status line and menus: `^K`, `Alt-X`, `F10`, `Shift-F6`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = match self.code {
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Shift-Tab".into(),
            KeyCode::Backspace => "Bksp".into(),
            KeyCode::Delete => "Del".into(),
            KeyCode::Insert => "Ins".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            other => format!("{other:?}"),
        };
        let ctrl = self.mods.contains(KeyModifiers::CONTROL);
        let alt = self.mods.contains(KeyModifiers::ALT);
        let shift = self.mods.contains(KeyModifiers::SHIFT);
        let is_char = matches!(self.code, KeyCode::Char(c) if c != ' ');
        let mut out = String::new();
        if ctrl && is_char && !alt && !shift {
            return write!(f, "^{key}");
        }
        if ctrl {
            out.push_str("Ctrl-");
        }
        if alt {
            out.push_str("Alt-");
        }
        if shift {
            out.push_str("Shift-");
        }
        out.push_str(&key);
        f.write_str(&out)
    }
}

/// A key or a short sequence of keys.
pub type KeySeq = Vec<KeyChord>;

/// Parse `ctrl+w h` into a two-chord sequence.
pub fn parse_seq(s: &str) -> Result<KeySeq, String> {
    let seq: Result<Vec<_>, _> = s.split_whitespace().map(KeyChord::parse).collect();
    let seq = seq?;
    if seq.is_empty() {
        return Err("empty key binding".into());
    }
    if seq.len() > 3 {
        return Err(format!("key sequences can be at most 3 keys: `{s}`"));
    }
    Ok(seq)
}

pub fn seq_to_string(seq: &[KeyChord]) -> String {
    seq.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

/// What a key press means right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Action(Action),
    /// The keys so far start a longer binding; wait for the next key.
    Pending,
    None,
}

/// The active bindings.
#[derive(Debug, Clone)]
pub struct Keymap {
    pub preset: KeymapPreset,
    bindings: std::collections::HashMap<KeySeq, Action>,
    /// Problems in the `[keys]` table, reported once.
    pub errors: Vec<String>,
}

impl Keymap {
    pub fn new(preset: KeymapPreset, overrides: &BTreeMap<String, KeySpec>) -> Self {
        let mut km = Self { preset, bindings: Default::default(), errors: vec![] };
        for (keys, action) in preset_bindings(preset) {
            for k in keys.split('|') {
                let seq = parse_seq(k).unwrap_or_else(|e| panic!("bad built-in binding `{k}`: {e}"));
                km.bindings.insert(seq, *action);
            }
        }
        for (name, spec) in overrides {
            let Some(action) = Action::from_id(name) else {
                km.errors.push(format!("[keys] unknown action `{name}`"));
                continue;
            };
            // An override replaces every default binding for that action.
            km.bindings.retain(|_, a| *a != action);
            for k in spec.keys() {
                match parse_seq(k) {
                    Ok(seq) => {
                        km.bindings.insert(seq, action);
                    }
                    Err(e) => km.errors.push(format!("[keys] {name}: {e}")),
                }
            }
        }
        km
    }

    /// Look up the keys pressed so far.
    pub fn lookup(&self, pressed: &[KeyChord]) -> Lookup {
        if let Some(a) = self.bindings.get(pressed) {
            return Lookup::Action(*a);
        }
        let is_prefix = self.bindings.keys().any(|seq| seq.len() > pressed.len() && seq.starts_with(pressed));
        if is_prefix { Lookup::Pending } else { Lookup::None }
    }

    /// The first binding for an action, for hints.
    pub fn key_for(&self, action: Action) -> Option<String> {
        self.bindings
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(k, _)| k)
            .map(|k| seq_to_string(k))
            .min_by_key(|s| (s.chars().filter(|c| *c == ' ').count(), s.len(), s.clone()))
    }

    /// Every binding for an action.
    pub fn keys_for(&self, action: Action) -> Vec<String> {
        let mut v: Vec<String> =
            self.bindings.iter().filter(|(_, a)| **a == action).map(|(k, _)| seq_to_string(k)).collect();
        v.sort_by_key(|s| (s.contains(' '), s.len(), s.clone()));
        v
    }

    /// All bindings sorted by action, for the keys window.
    pub fn all(&self) -> Vec<(Action, Vec<String>)> {
        Action::ALL.iter().map(|a| (*a, self.keys_for(*a))).collect()
    }
}

/// Built-in bindings: `|`-separated key sequences per action.
fn preset_bindings(preset: KeymapPreset) -> &'static [(&'static str, Action)] {
    use Action::*;
    const COMMON: &[(&str, Action)] = &[
        ("f10", Menu),
        ("f1", Help),
        ("ctrl+k|ctrl+space", Palette),
        ("ctrl+l", Redraw),
        ("alt+1", FocusWindow1),
        ("alt+2", FocusWindow2),
        ("alt+3", FocusWindow3),
        ("alt+4", FocusWindow4),
        ("alt+5", FocusWindow5),
        ("alt+6", FocusWindow6),
        ("alt+7", FocusWindow7),
        ("alt+8", FocusWindow8),
        ("alt+9", FocusWindow9),
    ];
    const CUA: &[(&str, Action)] = &[
        ("alt+x|ctrl+q", Quit),
        ("ctrl+o|f3", OpenUrl),
        ("f6|ctrl+tab", NextWindow),
        ("shift+f6|ctrl+backtab", PrevWindow),
        ("ctrl+w|alt+f3", CloseWindow),
        ("f5", ZoomWindow),
        ("ctrl+f5", MoveWindow),
        ("ctrl+f6", CycleLayout),
        ("ctrl+left", FocusLeft),
        ("ctrl+right", FocusRight),
        ("ctrl+up", FocusUp),
        ("ctrl+down", FocusDown),
        ("alt+right", GrowMaster),
        ("alt+left", ShrinkMaster),
        ("ctrl+t", Tasks),
        ("alt+f1", Keys),
        ("ctrl+,|f9", Settings),
        ("ctrl+h", History),
        ("f12", CycleTheme),
    ];
    const VIM: &[(&str, Action)] = &[
        ("ctrl+q|ctrl+w ctrl+q", Quit),
        (":", Palette),
        ("ctrl+o", OpenUrl),
        ("ctrl+w w|ctrl+w ctrl+w|f6", NextWindow),
        ("ctrl+w W|ctrl+w p|shift+f6", PrevWindow),
        ("ctrl+w q|ctrl+w c", CloseWindow),
        ("ctrl+w z|ctrl+w o|f5", ZoomWindow),
        ("ctrl+w m", MoveWindow),
        ("ctrl+w space|ctrl+f6", CycleLayout),
        ("ctrl+w d", LayoutDesktop),
        ("ctrl+w s|ctrl+w v", LayoutTiled),
        ("ctrl+w t", LayoutTabs),
        ("ctrl+w h|ctrl+w left", FocusLeft),
        ("ctrl+w l|ctrl+w right", FocusRight),
        ("ctrl+w k|ctrl+w up", FocusUp),
        ("ctrl+w j|ctrl+w down", FocusDown),
        ("ctrl+w >|ctrl+w +", GrowMaster),
        ("ctrl+w <|ctrl+w -", ShrinkMaster),
        ("ctrl+w =", Cascade),
        ("ctrl+t", Tasks),
        ("ctrl+w ?", Keys),
        ("ctrl+w ,", Settings),
        ("ctrl+h", History),
        ("f12", CycleTheme),
    ];
    const EMACS: &[(&str, Action)] = &[
        ("ctrl+x ctrl+c|ctrl+q", Quit),
        ("alt+x", Palette),
        ("ctrl+x ctrl+f|ctrl+o", OpenUrl),
        ("ctrl+x o|f6", NextWindow),
        ("ctrl+x O|shift+f6", PrevWindow),
        ("ctrl+x 0|ctrl+x k", CloseWindow),
        ("ctrl+x 1|f5", ZoomWindow),
        ("ctrl+x m", MoveWindow),
        ("ctrl+x l|ctrl+f6", CycleLayout),
        ("ctrl+x d", LayoutDesktop),
        ("ctrl+x 2|ctrl+x 3", LayoutTiled),
        ("ctrl+x t", LayoutTabs),
        ("ctrl+x left", FocusLeft),
        ("ctrl+x right", FocusRight),
        ("ctrl+x up", FocusUp),
        ("ctrl+x down", FocusDown),
        ("ctrl+x }", GrowMaster),
        ("ctrl+x {", ShrinkMaster),
        ("ctrl+x =", Cascade),
        ("ctrl+x ctrl+t", Tasks),
        ("ctrl+h k", Keys),
        ("ctrl+x ,", Settings),
        ("ctrl+x ctrl+h", History),
        ("f12", CycleTheme),
    ];
    // Leak-free: return a static slice by concatenating at first use.
    use std::sync::OnceLock;
    static CUA_ALL: OnceLock<Vec<(&'static str, Action)>> = OnceLock::new();
    static VIM_ALL: OnceLock<Vec<(&'static str, Action)>> = OnceLock::new();
    static EMACS_ALL: OnceLock<Vec<(&'static str, Action)>> = OnceLock::new();
    let (cell, specific) = match preset {
        KeymapPreset::Cua => (&CUA_ALL, CUA),
        KeymapPreset::Vim => (&VIM_ALL, VIM),
        KeymapPreset::Emacs => (&EMACS_ALL, EMACS),
    };
    cell.get_or_init(|| COMMON.iter().chain(specific.iter()).copied().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn parses_keys() {
        assert_eq!(k("ctrl+k"), KeyChord::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(k("Ctrl+K"), KeyChord::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(k("alt+x"), KeyChord::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(k("f10"), KeyChord::new(KeyCode::F(10), KeyModifiers::NONE));
        assert_eq!(k("shift+f6"), KeyChord::new(KeyCode::F(6), KeyModifiers::SHIFT));
        assert_eq!(k(":"), KeyChord::new(KeyCode::Char(':'), KeyModifiers::NONE));
        assert_eq!(k("ctrl++"), KeyChord::new(KeyCode::Char('+'), KeyModifiers::CONTROL));
        assert_eq!(k("W"), KeyChord::new(KeyCode::Char('W'), KeyModifiers::NONE));
        assert!(KeyChord::parse("hyper+x").is_err());
        assert!(KeyChord::parse("f99").is_err());
        assert!(KeyChord::parse("banana").is_err());
    }

    #[test]
    fn displays_compactly() {
        assert_eq!(k("ctrl+k").to_string(), "^K");
        assert_eq!(k("alt+x").to_string(), "Alt-X");
        assert_eq!(k("shift+f6").to_string(), "Shift-F6");
        assert_eq!(k("ctrl+f5").to_string(), "Ctrl-F5");
    }

    #[test]
    fn events_normalize_to_chords() {
        let ev = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(KeyChord::from_event(&ev), k("ctrl+k"));
        let ev = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT);
        assert_eq!(KeyChord::from_event(&ev), k(":"));
    }

    #[test]
    fn every_preset_builds_and_has_essentials() {
        for preset in KeymapPreset::ALL {
            let km = Keymap::new(preset, &BTreeMap::new());
            for a in [Action::Quit, Action::Palette, Action::Menu, Action::NextWindow, Action::CloseWindow] {
                assert!(km.key_for(a).is_some(), "{preset:?} has no key for {a:?}");
            }
        }
    }

    #[test]
    fn sequences_report_pending() {
        let km = Keymap::new(KeymapPreset::Vim, &BTreeMap::new());
        assert_eq!(km.lookup(&[k("ctrl+w")]), Lookup::Pending);
        assert_eq!(km.lookup(&[k("ctrl+w"), k("h")]), Lookup::Action(Action::FocusLeft));
        assert_eq!(km.lookup(&[k("ctrl+w"), k("y")]), Lookup::None);
    }

    #[test]
    fn overrides_replace_defaults_and_report_errors() {
        let mut o = BTreeMap::new();
        o.insert("palette".to_string(), KeySpec::One("ctrl+p".into()));
        o.insert("not_an_action".to_string(), KeySpec::One("ctrl+z".into()));
        o.insert("quit".to_string(), KeySpec::Many(vec!["alt+q".into(), "hyper+q".into()]));
        let km = Keymap::new(KeymapPreset::Cua, &o);
        assert_eq!(km.lookup(&[k("ctrl+p")]), Lookup::Action(Action::Palette));
        assert_eq!(km.lookup(&[k("ctrl+k")]), Lookup::None);
        assert_eq!(km.lookup(&[k("alt+q")]), Lookup::Action(Action::Quit));
        assert_eq!(km.errors.len(), 2);
    }
}
