use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::keymap::{Action, Keymap};
use crate::text::ScrollText;
use crate::window::{Cx, DrawCx, Handled, Window};

/// A read-only page of styled text.
pub struct TextPage {
    title: String,
    url: String,
    body: ScrollText,
    /// Lines whose text is a heading, styled at draw time with the theme.
    raw: Vec<Row>,
}

#[derive(Clone)]
enum Row {
    Title(String),
    Heading(String),
    Text(String),
    Key(String, String),
    Blank,
}

impl TextPage {
    fn new(title: &str, url: &str, rows: Vec<Row>) -> Self {
        Self { title: title.into(), url: url.into(), body: ScrollText::default(), raw: rows }
    }

    pub fn welcome(cx: &Cx) -> Self {
        let km = cx.keymap;
        let k = |a: Action| km.key_for(a).unwrap_or_else(|| "(unbound)".into());
        let mut rows = vec![];
        for l in alouette_nostalgia::LOGO {
            rows.push(Row::Title((*l).to_string()));
        }
        rows.extend([
            Row::Blank,
            Row::Text(alouette_nostalgia::TAGLINE.into()),
            Row::Blank,
            Row::Text(
                "Alouette brings mail, news, Gopher and Gemini, file transfer, terminals, the web \
                 and more into one terminal app, in the spirit of Minuet (1994). This is milestone \
                 M0, the foundation: the desktop, layouts, palette, themes and background tasks."
                    .into(),
            ),
            Row::Blank,
            Row::Heading("Getting around".into()),
            Row::Key(k(Action::Palette), "Command palette: every command, window, theme and URL".into()),
            Row::Key(k(Action::Menu), "Menu bar (or Alt plus the red letter)".into()),
            Row::Key(k(Action::OpenUrl), "Open a URL, such as about:tasks or demo:jobs".into()),
            Row::Key(k(Action::NextWindow), "Next window".into()),
            Row::Key(k(Action::CycleLayout), "Switch layout: Desktop, Tiled, Tabs".into()),
            Row::Key(k(Action::ZoomWindow), "Zoom the window".into()),
            Row::Key(k(Action::MoveWindow), "Move or resize with the arrow keys (Desktop)".into()),
            Row::Key(k(Action::CloseWindow), "Close the window".into()),
            Row::Key(k(Action::Tasks), "Tasks: everything running in the background".into()),
            Row::Key(k(Action::Help), "Help".into()),
            Row::Key(k(Action::Quit), "Exit".into()),
            Row::Blank,
            Row::Heading("Try these".into()),
            Row::Text("• Open Modules > Demo and start a few background jobs, then watch Tasks.".into()),
            Row::Text("• Type \"theme\" in the palette and try amber, green or modern-dark.".into()),
            Row::Text("• Open Settings to switch layouts, keymaps and nostalgia touches.".into()),
            Row::Text("• Click and drag window titles. The mouse works everywhere.".into()),
            Row::Blank,
            Row::Heading("Coming next".into()),
            Row::Text(
                "M1 Gopher, Gemini, Finger, search and net tools · M2 SSH, Mosh and files · \
                       M3 Mail · M4 News and social · M5 Web · M6 Contacts and calendar · \
                       M7 Chat · M8 AI assistant · M9 SSH serve mode · M10 polish and 1.0"
                    .into(),
            ),
        ]);
        Self::new("Welcome", "about:welcome", rows)
    }

    pub fn about() -> Self {
        Self::new(
            "About Alouette",
            "about:alouette",
            vec![
                Row::Title(format!("Alouette {}", alouette_core::VERSION)),
                Row::Blank,
                Row::Text(alouette_nostalgia::TAGLINE.into()),
                Row::Blank,
                Row::Key("License".into(), "MIT".into()),
                Row::Key("Source".into(), "https://github.com/kevinelliott/alouette".into()),
                Row::Key("Built with".into(), "Rust, Ratatui, crossterm, tokio, SQLite".into()),
                Row::Blank,
                Row::Text(
                    "Inspired by Minuet, the Minnesota Internet Users Essential Tool, from the \
                           University of Minnesota. See about:minuet."
                        .into(),
                ),
            ],
        )
    }

    pub fn minuet() -> Self {
        let mut rows = vec![Row::Title("Minuet, 1993-1996".into()), Row::Blank];
        for para in alouette_nostalgia::MINUET_HISTORY.split("\n\n") {
            rows.push(Row::Text(para.replace('\n', " ")));
            rows.push(Row::Blank);
        }
        Self::new("About Minuet", "about:minuet", rows)
    }

    pub fn help() -> Self {
        let mut rows = vec![];
        for line in include_str!("../help/manual.md").lines() {
            rows.push(if let Some(h) = line.strip_prefix("# ") {
                Row::Title(h.into())
            } else if let Some(h) = line.strip_prefix("## ") {
                Row::Heading(h.into())
            } else if line.trim().is_empty() {
                Row::Blank
            } else {
                Row::Text(line.into())
            });
        }
        Self::new("Help", "about:help", rows)
    }

    fn render(&self, cx: &DrawCx) -> Vec<Line<'static>> {
        let t = cx.theme;
        self.raw
            .iter()
            .map(|r| match r {
                Row::Title(s) => Line::from(Span::styled(s.clone(), t.accent())),
                Row::Heading(s) => Line::from(Span::styled(
                    s.clone(),
                    Style::new().fg(t.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                )),
                Row::Text(s) => Line::from(s.clone()),
                Row::Key(k, v) => Line::from(vec![
                    Span::styled(format!("  {k:<14}"), Style::new().fg(t.link).add_modifier(Modifier::BOLD)),
                    Span::raw(v.clone()),
                ]),
                Row::Blank => Line::default(),
            })
            .collect()
    }
}

impl Window for TextPage {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn module(&self) -> &'static str {
        "core"
    }
    fn url(&self) -> Option<String> {
        Some(self.url.clone())
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let lines = self.render(cx);
        self.body.set_lines(lines);
        let inner = Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area };
        self.body.draw(frame, inner, cx.theme);
    }
    fn on_key(&mut self, key: KeyEvent, _cx: &mut Cx) -> Handled {
        self.body.on_key(key)
    }
    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        self.body.on_mouse(ev)
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑↓", "Scroll")]
    }
}

/// Lists every key binding for the active keymap.
pub struct KeysWindow {
    body: ScrollText,
    preset: String,
}

impl KeysWindow {
    pub fn new(cx: &Cx) -> Self {
        Self { body: ScrollText::default(), preset: cx.keymap.preset.name().into() }
    }

    fn lines(km: &Keymap, cx: &DrawCx) -> Vec<Line<'static>> {
        let mut out = vec![
            Line::from(Span::styled(format!("Key bindings: {} preset", km.preset.name()), cx.theme.accent())),
            Line::from(Span::styled("Change bindings in the [keys] table of alouette.toml.", cx.theme.dim())),
            Line::default(),
        ];
        for (a, keys) in km.all() {
            if keys.is_empty() {
                continue;
            }
            out.push(Line::from(vec![
                Span::styled(format!("  {:<26}", a.label()), Style::new()),
                Span::styled(keys.join(", "), Style::new().fg(cx.theme.link).add_modifier(Modifier::BOLD)),
                Span::styled(format!("   {}", a.id()), cx.theme.dim()),
            ]));
        }
        out
    }
}

impl Window for KeysWindow {
    fn title(&self) -> String {
        format!("Keys ({})", self.preset)
    }
    fn module(&self) -> &'static str {
        "core"
    }
    fn url(&self) -> Option<String> {
        Some("about:keys".into())
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        self.preset = cx.keymap.preset.name().into();
        self.body.set_lines(Self::lines(cx.keymap, cx));
        self.body.draw(frame, area, cx.theme);
    }
    fn on_key(&mut self, key: KeyEvent, _cx: &mut Cx) -> Handled {
        self.body.on_key(key)
    }
    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        self.body.on_mouse(ev)
    }
}
