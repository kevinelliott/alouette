//! Themes.
//!
//! A theme fills a fixed set of semantic color slots. Built-in themes cover the Minuet and
//! Turbo Vision looks, phosphor monitors, modern dark and light, high contrast and mono.
//! Custom themes are TOML files in the config `themes` folder and may inherit a built-in.
//! Colors degrade automatically for 256-color, 16-color and no-color terminals.

use std::collections::BTreeMap;
use std::path::Path;

use alouette_core::config::BorderStyle;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
use serde::Deserialize;

/// What the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    Mono,
}

impl ColorMode {
    /// Guess from the environment: `NO_COLOR`, `COLORTERM` and `TERM`.
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorMode::Mono;
        }
        let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return ColorMode::TrueColor;
        }
        let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        if matches!(term_program.as_str(), "iTerm.app" | "WezTerm" | "ghostty" | "vscode")
            || term.contains("kitty")
            || term.contains("alacritty")
            || term.contains("direct")
        {
            return ColorMode::TrueColor;
        }
        if term == "linux" || term == "vt100" || term == "vt220" || term == "ansi" {
            return ColorMode::Ansi16;
        }
        if term == "dumb" {
            return ColorMode::Mono;
        }
        if cfg!(windows) {
            // Windows Terminal and modern conhost both do truecolor.
            return ColorMode::TrueColor;
        }
        ColorMode::Ansi256
    }
}

/// Every color slot a theme defines.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub border: BorderType,
    /// Fill character for the desktop background when the pattern is on.
    pub pattern: char,
    pub desktop_fg: Color,
    pub desktop_bg: Color,
    pub menu_fg: Color,
    pub menu_bg: Color,
    pub menu_hotkey: Color,
    pub menu_sel_fg: Color,
    pub menu_sel_bg: Color,
    pub menu_disabled: Color,
    pub window_fg: Color,
    pub window_bg: Color,
    pub border_fg: Color,
    pub border_active_fg: Color,
    pub title_fg: Color,
    pub title_active_fg: Color,
    pub title_active_bg: Color,
    pub status_fg: Color,
    pub status_bg: Color,
    pub status_key: Color,
    pub accent: Color,
    pub dim: Color,
    pub link: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub shadow_fg: Color,
    pub shadow_bg: Color,
    pub dialog_fg: Color,
    pub dialog_bg: Color,
    pub input_fg: Color,
    pub input_bg: Color,
}

// DOS palette, as Minuet and Turbo Vision drew it.
const DOS_BLACK: Color = Color::Rgb(0, 0, 0);
const DOS_BLUE: Color = Color::Rgb(0, 0, 170);
const DOS_GREEN: Color = Color::Rgb(0, 170, 0);
const DOS_CYAN: Color = Color::Rgb(0, 170, 170);
const DOS_RED: Color = Color::Rgb(170, 0, 0);
#[allow(dead_code)]
const DOS_BROWN: Color = Color::Rgb(170, 85, 0);
const DOS_LIGHTGRAY: Color = Color::Rgb(170, 170, 170);
const DOS_DARKGRAY: Color = Color::Rgb(85, 85, 85);
const DOS_LIGHTGREEN: Color = Color::Rgb(85, 255, 85);
const DOS_LIGHTCYAN: Color = Color::Rgb(85, 255, 255);
const DOS_LIGHTRED: Color = Color::Rgb(255, 85, 85);
const DOS_YELLOW: Color = Color::Rgb(255, 255, 85);
const DOS_WHITE: Color = Color::Rgb(255, 255, 255);

impl Theme {
    /// Minuet's look: a blue desktop, grey menus, cyan highlights.
    pub fn minuet() -> Self {
        Self {
            name: "minuet".into(),
            border: BorderType::Double,
            pattern: '░',
            desktop_fg: DOS_LIGHTGRAY,
            desktop_bg: DOS_BLUE,
            menu_fg: DOS_BLACK,
            menu_bg: DOS_LIGHTGRAY,
            menu_hotkey: DOS_RED,
            menu_sel_fg: DOS_BLACK,
            menu_sel_bg: DOS_GREEN,
            menu_disabled: DOS_DARKGRAY,
            window_fg: DOS_WHITE,
            window_bg: DOS_BLUE,
            border_fg: DOS_LIGHTGRAY,
            border_active_fg: DOS_WHITE,
            title_fg: DOS_LIGHTGRAY,
            title_active_fg: DOS_BLACK,
            title_active_bg: DOS_CYAN,
            status_fg: DOS_BLACK,
            status_bg: DOS_LIGHTGRAY,
            status_key: DOS_RED,
            accent: DOS_LIGHTCYAN,
            dim: DOS_LIGHTGRAY,
            link: DOS_YELLOW,
            selection_fg: DOS_BLACK,
            selection_bg: DOS_CYAN,
            success: DOS_LIGHTGREEN,
            warning: DOS_YELLOW,
            error: DOS_LIGHTRED,
            shadow_fg: DOS_DARKGRAY,
            shadow_bg: DOS_BLACK,
            dialog_fg: DOS_BLACK,
            dialog_bg: DOS_LIGHTGRAY,
            input_fg: DOS_WHITE,
            input_bg: DOS_BLUE,
        }
    }

    /// Turbo Vision's IDE colors: cyan desktop pattern, blue editor windows.
    pub fn turbo() -> Self {
        Self {
            name: "turbo".into(),
            pattern: '▒',
            desktop_fg: DOS_BLUE,
            desktop_bg: DOS_LIGHTGRAY,
            window_fg: DOS_YELLOW,
            window_bg: DOS_BLUE,
            title_active_fg: DOS_WHITE,
            title_active_bg: DOS_BLUE,
            title_fg: DOS_LIGHTGRAY,
            border_active_fg: DOS_LIGHTGREEN,
            selection_bg: DOS_LIGHTGRAY,
            selection_fg: DOS_BLACK,
            ..Self::minuet()
        }
    }

    fn phosphor(name: &str, bright: Color, mid: Color, dark: Color) -> Self {
        let black = Color::Rgb(0, 0, 0);
        Self {
            name: name.into(),
            border: BorderType::Plain,
            pattern: ' ',
            desktop_fg: dark,
            desktop_bg: black,
            menu_fg: mid,
            menu_bg: black,
            menu_hotkey: bright,
            menu_sel_fg: black,
            menu_sel_bg: bright,
            menu_disabled: dark,
            window_fg: mid,
            window_bg: black,
            border_fg: dark,
            border_active_fg: bright,
            title_fg: mid,
            title_active_fg: black,
            title_active_bg: bright,
            status_fg: black,
            status_bg: mid,
            status_key: black,
            accent: bright,
            dim: dark,
            link: bright,
            selection_fg: black,
            selection_bg: mid,
            success: bright,
            warning: bright,
            error: bright,
            shadow_fg: dark,
            shadow_bg: black,
            dialog_fg: mid,
            dialog_bg: black,
            input_fg: bright,
            input_bg: black,
        }
    }

    pub fn amber() -> Self {
        Self::phosphor("amber", Color::Rgb(255, 191, 0), Color::Rgb(230, 150, 0), Color::Rgb(120, 70, 0))
    }

    pub fn green() -> Self {
        Self::phosphor("green", Color::Rgb(51, 255, 51), Color::Rgb(0, 200, 0), Color::Rgb(0, 100, 0))
    }

    pub fn modern_dark() -> Self {
        let bg = Color::Rgb(24, 25, 30);
        let panel = Color::Rgb(32, 34, 40);
        let fg = Color::Rgb(220, 222, 228);
        let muted = Color::Rgb(130, 135, 150);
        let accent = Color::Rgb(122, 162, 247);
        Self {
            name: "modern-dark".into(),
            border: BorderType::Rounded,
            pattern: ' ',
            desktop_fg: muted,
            desktop_bg: bg,
            menu_fg: fg,
            menu_bg: panel,
            menu_hotkey: accent,
            menu_sel_fg: bg,
            menu_sel_bg: accent,
            menu_disabled: muted,
            window_fg: fg,
            window_bg: panel,
            border_fg: Color::Rgb(70, 74, 86),
            border_active_fg: accent,
            title_fg: muted,
            title_active_fg: accent,
            title_active_bg: panel,
            status_fg: fg,
            status_bg: Color::Rgb(40, 42, 50),
            status_key: accent,
            accent,
            dim: muted,
            link: Color::Rgb(125, 207, 255),
            selection_fg: fg,
            selection_bg: Color::Rgb(54, 60, 82),
            success: Color::Rgb(158, 206, 106),
            warning: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),
            shadow_fg: Color::Rgb(16, 16, 20),
            shadow_bg: Color::Rgb(12, 12, 16),
            dialog_fg: fg,
            dialog_bg: Color::Rgb(44, 47, 56),
            input_fg: fg,
            input_bg: bg,
        }
    }

    pub fn modern_light() -> Self {
        let bg = Color::Rgb(244, 244, 246);
        let panel = Color::Rgb(255, 255, 255);
        let fg = Color::Rgb(36, 41, 47);
        let muted = Color::Rgb(110, 119, 129);
        let accent = Color::Rgb(9, 105, 218);
        Self {
            name: "modern-light".into(),
            border: BorderType::Rounded,
            pattern: ' ',
            desktop_fg: muted,
            desktop_bg: bg,
            menu_fg: fg,
            menu_bg: panel,
            menu_hotkey: accent,
            menu_sel_fg: panel,
            menu_sel_bg: accent,
            menu_disabled: muted,
            window_fg: fg,
            window_bg: panel,
            border_fg: Color::Rgb(208, 215, 222),
            border_active_fg: accent,
            title_fg: muted,
            title_active_fg: accent,
            title_active_bg: panel,
            status_fg: fg,
            status_bg: Color::Rgb(230, 232, 236),
            status_key: accent,
            accent,
            dim: muted,
            link: accent,
            selection_fg: fg,
            selection_bg: Color::Rgb(221, 236, 255),
            success: Color::Rgb(26, 127, 55),
            warning: Color::Rgb(154, 103, 0),
            error: Color::Rgb(207, 34, 46),
            shadow_fg: Color::Rgb(200, 200, 205),
            shadow_bg: Color::Rgb(210, 210, 215),
            dialog_fg: fg,
            dialog_bg: panel,
            input_fg: fg,
            input_bg: bg,
        }
    }

    pub fn high_contrast() -> Self {
        Self {
            name: "high-contrast".into(),
            border: BorderType::Thick,
            pattern: ' ',
            desktop_fg: Color::White,
            desktop_bg: Color::Black,
            menu_fg: Color::White,
            menu_bg: Color::Black,
            menu_hotkey: Color::Yellow,
            menu_sel_fg: Color::Black,
            menu_sel_bg: Color::Yellow,
            menu_disabled: Color::Gray,
            window_fg: Color::White,
            window_bg: Color::Black,
            border_fg: Color::White,
            border_active_fg: Color::Yellow,
            title_fg: Color::White,
            title_active_fg: Color::Black,
            title_active_bg: Color::Yellow,
            status_fg: Color::Black,
            status_bg: Color::White,
            status_key: Color::Black,
            accent: Color::Yellow,
            dim: Color::Gray,
            link: Color::LightCyan,
            selection_fg: Color::Black,
            selection_bg: Color::Yellow,
            success: Color::LightGreen,
            warning: Color::Yellow,
            error: Color::LightRed,
            shadow_fg: Color::Black,
            shadow_bg: Color::Black,
            dialog_fg: Color::White,
            dialog_bg: Color::Black,
            input_fg: Color::Black,
            input_bg: Color::White,
        }
    }

    /// No colors at all: reverse video for focus and selection.
    pub fn mono() -> Self {
        let r = Color::Reset;
        Self {
            name: "mono".into(),
            border: BorderType::Plain,
            pattern: ' ',
            desktop_fg: r,
            desktop_bg: r,
            menu_fg: r,
            menu_bg: r,
            menu_hotkey: r,
            menu_sel_fg: r,
            menu_sel_bg: r,
            menu_disabled: r,
            window_fg: r,
            window_bg: r,
            border_fg: r,
            border_active_fg: r,
            title_fg: r,
            title_active_fg: r,
            title_active_bg: r,
            status_fg: r,
            status_bg: r,
            status_key: r,
            accent: r,
            dim: r,
            link: r,
            selection_fg: r,
            selection_bg: r,
            success: r,
            warning: r,
            error: r,
            shadow_fg: r,
            shadow_bg: r,
            dialog_fg: r,
            dialog_bg: r,
            input_fg: r,
            input_bg: r,
        }
    }

    /// True when this theme relies on reverse video instead of colors.
    pub fn is_mono(&self) -> bool {
        self.selection_bg == Color::Reset && self.selection_fg == Color::Reset
    }

    // ---- styles built from slots ----

    pub fn window(&self) -> Style {
        Style::new().fg(self.window_fg).bg(self.window_bg)
    }
    pub fn selection(&self) -> Style {
        if self.is_mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.selection_fg).bg(self.selection_bg)
        }
    }
    pub fn menu_selected(&self) -> Style {
        if self.is_mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.menu_sel_fg).bg(self.menu_sel_bg)
        }
    }
    pub fn dim(&self) -> Style {
        if self.is_mono() { Style::new().add_modifier(Modifier::DIM) } else { Style::new().fg(self.dim) }
    }
    pub fn accent(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }
    pub fn link(&self) -> Style {
        Style::new().fg(self.link).add_modifier(Modifier::UNDERLINED)
    }

    /// Border type after applying the user's `border` setting.
    pub fn border_type(&self, setting: BorderStyle) -> Option<BorderType> {
        match setting {
            BorderStyle::Theme => Some(self.border),
            BorderStyle::Rounded => Some(BorderType::Rounded),
            BorderStyle::Plain => Some(BorderType::Plain),
            BorderStyle::Double => Some(BorderType::Double),
            BorderStyle::Thick => Some(BorderType::Thick),
            BorderStyle::Ascii => None,
        }
    }

    /// Rewrite every color for what the terminal can show.
    pub fn degraded(mut self, mode: ColorMode) -> Self {
        if mode == ColorMode::TrueColor {
            return self;
        }
        if mode == ColorMode::Mono {
            let name = self.name.clone();
            let border = self.border;
            return Self { name, border, ..Self::mono() };
        }
        self.for_each_color(|c| degrade(c, mode));
        if mode == ColorMode::Ansi16 && !self.pattern.is_ascii() && self.pattern != '░' && self.pattern != '▒' {
            self.pattern = ' ';
        }
        self
    }

    fn for_each_color(&mut self, mut f: impl FnMut(Color) -> Color) {
        for slot in [
            &mut self.desktop_fg,
            &mut self.desktop_bg,
            &mut self.menu_fg,
            &mut self.menu_bg,
            &mut self.menu_hotkey,
            &mut self.menu_sel_fg,
            &mut self.menu_sel_bg,
            &mut self.menu_disabled,
            &mut self.window_fg,
            &mut self.window_bg,
            &mut self.border_fg,
            &mut self.border_active_fg,
            &mut self.title_fg,
            &mut self.title_active_fg,
            &mut self.title_active_bg,
            &mut self.status_fg,
            &mut self.status_bg,
            &mut self.status_key,
            &mut self.accent,
            &mut self.dim,
            &mut self.link,
            &mut self.selection_fg,
            &mut self.selection_bg,
            &mut self.success,
            &mut self.warning,
            &mut self.error,
            &mut self.shadow_fg,
            &mut self.shadow_bg,
            &mut self.dialog_fg,
            &mut self.dialog_bg,
            &mut self.input_fg,
            &mut self.input_bg,
        ] {
            *slot = f(*slot);
        }
    }

    fn slot_mut(&mut self, name: &str) -> Option<&mut Color> {
        Some(match name {
            "desktop_fg" => &mut self.desktop_fg,
            "desktop_bg" => &mut self.desktop_bg,
            "menu_fg" => &mut self.menu_fg,
            "menu_bg" => &mut self.menu_bg,
            "menu_hotkey" => &mut self.menu_hotkey,
            "menu_sel_fg" => &mut self.menu_sel_fg,
            "menu_sel_bg" => &mut self.menu_sel_bg,
            "menu_disabled" => &mut self.menu_disabled,
            "window_fg" => &mut self.window_fg,
            "window_bg" => &mut self.window_bg,
            "border_fg" => &mut self.border_fg,
            "border_active_fg" => &mut self.border_active_fg,
            "title_fg" => &mut self.title_fg,
            "title_active_fg" => &mut self.title_active_fg,
            "title_active_bg" => &mut self.title_active_bg,
            "status_fg" => &mut self.status_fg,
            "status_bg" => &mut self.status_bg,
            "status_key" => &mut self.status_key,
            "accent" => &mut self.accent,
            "dim" => &mut self.dim,
            "link" => &mut self.link,
            "selection_fg" => &mut self.selection_fg,
            "selection_bg" => &mut self.selection_bg,
            "success" => &mut self.success,
            "warning" => &mut self.warning,
            "error" => &mut self.error,
            "shadow_fg" => &mut self.shadow_fg,
            "shadow_bg" => &mut self.shadow_bg,
            "dialog_fg" => &mut self.dialog_fg,
            "dialog_bg" => &mut self.dialog_bg,
            "input_fg" => &mut self.input_fg,
            "input_bg" => &mut self.input_bg,
            _ => return None,
        })
    }
}

/// Map one color to the nearest the terminal can show.
pub fn degrade(c: Color, mode: ColorMode) -> Color {
    match (c, mode) {
        (Color::Rgb(r, g, b), ColorMode::Ansi256) => Color::Indexed(rgb_to_256(r, g, b)),
        (Color::Rgb(r, g, b), ColorMode::Ansi16) => rgb_to_16(r, g, b),
        (Color::Indexed(_), ColorMode::Ansi16) => Color::Gray,
        (_, ColorMode::Mono) => Color::Reset,
        (c, _) => c,
    }
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Grey ramp when the color is close to grey.
    if r.abs_diff(g) < 8 && g.abs_diff(b) < 8 {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((r as u16 - 8) * 24 / 247) as u8;
    }
    let q = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v - 35) / 40).min(5)
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

fn rgb_to_16(r: u8, g: u8, b: u8) -> Color {
    const TABLE: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (170, 0, 0)),
        (Color::Green, (0, 170, 0)),
        (Color::Yellow, (170, 85, 0)),
        (Color::Blue, (0, 0, 170)),
        (Color::Magenta, (170, 0, 170)),
        (Color::Cyan, (0, 170, 170)),
        (Color::Gray, (170, 170, 170)),
        (Color::DarkGray, (85, 85, 85)),
        (Color::LightRed, (255, 85, 85)),
        (Color::LightGreen, (85, 255, 85)),
        (Color::LightYellow, (255, 255, 85)),
        (Color::LightBlue, (85, 85, 255)),
        (Color::LightMagenta, (255, 85, 255)),
        (Color::LightCyan, (85, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];
    let dist = |(cr, cg, cb): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(r, cr) + d(g, cg) + d(b, cb)
    };
    TABLE.iter().min_by_key(|(_, rgb)| dist(*rgb)).map(|(c, _)| *c).unwrap_or(Color::Reset)
}

/// Parse a color: a name (`blue`, `lightcyan`), `#rrggbb`, or a 0-255 palette index.
pub fn parse_color(s: &str) -> Result<Color, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).map_err(|_| format!("bad hex color `{s}`"))?;
            return Ok(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return Err(format!("hex colors need 6 digits: `{s}`"));
    }
    if let Ok(n) = s.parse::<u8>() {
        return Ok(Color::Indexed(n));
    }
    s.parse::<Color>().map_err(|_| format!("unknown color `{s}`"))
}

/// A custom theme file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    name: Option<String>,
    inherits: Option<String>,
    border: Option<String>,
    pattern: Option<char>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

/// The built-in themes plus any found in the user's themes folder.
#[derive(Debug, Clone)]
pub struct ThemeSet {
    themes: Vec<Theme>,
    /// Problems loading custom themes, shown once as notifications.
    pub errors: Vec<String>,
}

impl Default for ThemeSet {
    fn default() -> Self {
        Self::builtin()
    }
}

impl ThemeSet {
    pub fn builtin() -> Self {
        Self {
            themes: vec![
                Theme::minuet(),
                Theme::turbo(),
                Theme::amber(),
                Theme::green(),
                Theme::modern_dark(),
                Theme::modern_light(),
                Theme::high_contrast(),
                Theme::mono(),
            ],
            errors: vec![],
        }
    }

    /// Built-ins plus every `*.toml` in `dir`. A broken file is skipped and reported.
    pub fn load(dir: &Path) -> Self {
        let mut set = Self::builtin();
        let Ok(read) = std::fs::read_dir(dir) else { return set };
        let mut files: Vec<_> = read
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for path in files {
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| set.parse_custom(&t, &path)) {
                Ok(theme) => set.insert(theme),
                Err(e) => set.errors.push(format!("theme {}: {e}", path.display())),
            }
        }
        set
    }

    fn parse_custom(&self, text: &str, path: &Path) -> Result<Theme, String> {
        let file: ThemeFile = toml::from_str(text).map_err(|e| e.to_string().trim().to_string())?;
        let base = file.inherits.as_deref().unwrap_or("minuet");
        let mut theme = self.get(base).cloned().ok_or_else(|| format!("inherits unknown theme `{base}`"))?;
        theme.name = file
            .name
            .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "custom".into());
        if let Some(b) = file.border {
            theme.border = match b.as_str() {
                "plain" => BorderType::Plain,
                "rounded" => BorderType::Rounded,
                "double" => BorderType::Double,
                "thick" => BorderType::Thick,
                other => return Err(format!("unknown border `{other}`")),
            };
        }
        if let Some(p) = file.pattern {
            theme.pattern = p;
        }
        for (slot, value) in &file.colors {
            let color = parse_color(value)?;
            *theme.slot_mut(slot).ok_or_else(|| format!("unknown color slot `{slot}`"))? = color;
        }
        Ok(theme)
    }

    fn insert(&mut self, theme: Theme) {
        match self.themes.iter_mut().find(|t| t.name == theme.name) {
            Some(existing) => *existing = theme,
            None => self.themes.push(theme),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.iter().map(|t| t.name.as_str())
    }

    /// The named theme, or Minuet if it does not exist.
    pub fn resolve(&self, name: &str) -> Theme {
        self.get(name).cloned().unwrap_or_else(Theme::minuet)
    }

    /// The theme after `name`, wrapping around.
    pub fn next_after(&self, name: &str) -> &Theme {
        let i = self.themes.iter().position(|t| t.name == name).map(|i| i + 1).unwrap_or(0);
        &self.themes[i % self.themes.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#0000aa").unwrap(), Color::Rgb(0, 0, 170));
        assert_eq!(parse_color("lightcyan").unwrap(), Color::LightCyan);
        assert_eq!(parse_color("17").unwrap(), Color::Indexed(17));
        assert!(parse_color("#abc").is_err());
        assert!(parse_color("chartreuse-ish").is_err());
    }

    #[test]
    fn degrades_to_palettes() {
        assert_eq!(degrade(DOS_BLUE, ColorMode::Ansi16), Color::Blue);
        assert_eq!(degrade(DOS_LIGHTGRAY, ColorMode::Ansi16), Color::Gray);
        assert!(matches!(degrade(DOS_BLUE, ColorMode::Ansi256), Color::Indexed(_)));
        assert_eq!(degrade(DOS_BLUE, ColorMode::Mono), Color::Reset);
        let t = Theme::minuet().degraded(ColorMode::Mono);
        assert!(t.is_mono());
    }

    #[test]
    fn custom_theme_inherits_and_overrides() {
        let set = ThemeSet::builtin();
        let t = set
            .parse_custom(
                "name = \"midnight\"\ninherits = \"modern-dark\"\nborder = \"double\"\n[colors]\naccent = \"#ff00ff\"\n",
                Path::new("midnight.toml"),
            )
            .unwrap();
        assert_eq!(t.name, "midnight");
        assert_eq!(t.border, BorderType::Double);
        assert_eq!(t.accent, Color::Rgb(255, 0, 255));
        assert_eq!(t.window_bg, Theme::modern_dark().window_bg);
        assert!(set.parse_custom("[colors]\nnope = \"red\"\n", Path::new("x.toml")).is_err());
    }

    #[test]
    fn theme_cycle_wraps() {
        let set = ThemeSet::builtin();
        assert_eq!(set.next_after("minuet").name, "turbo");
        assert_eq!(set.next_after("mono").name, "minuet");
    }
}
