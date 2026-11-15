//! The Burrow browser window.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alouette_core::event::ModulePayload;
use alouette_core::{AppEvent, Notification};
use alouette_net::ConnInfo;
use alouette_ui::window::{Cx, DrawCx, Effect, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::doc::{Doc, Flavor, Kind, Row, layout};
use crate::fetch::{Loaded, Loader, Outcome, Pins, Request};

pub const MODULE: &str = "burrow";
static NEXT_REQ: AtomicU64 = AtomicU64::new(1);

/// An input prompt shown on the bottom row.
struct Prompt {
    label: String,
    value: String,
    sensitive: bool,
    /// The URL the answer is sent to.
    target: String,
}

pub struct Browser {
    url: String,
    doc: Doc,
    pins: Pins,
    downloads: PathBuf,
    req: u64,
    loading: Option<Instant>,
    back: Vec<String>,
    forward: Vec<String>,
    /// The selected document line (always a link).
    sel: Option<usize>,
    scroll: usize,
    view_h: usize,
    rows: Vec<Row>,
    rows_width: usize,
    prompt: Option<Prompt>,
    info: Option<ConnInfo>,
    /// A changed certificate waiting for the reader to accept it.
    cert_change: Option<String>,
}

impl Browser {
    pub fn new(url: &str, pins: Pins, downloads: PathBuf, cx: &mut Cx) -> Self {
        let mut b = Self {
            url: url.to_string(),
            doc: Doc::message("Loading", Kind::Text, &[url]),
            pins,
            downloads,
            req: 0,
            loading: None,
            back: vec![],
            forward: vec![],
            sel: None,
            scroll: 0,
            view_h: 10,
            rows: vec![],
            rows_width: 0,
            prompt: None,
            info: None,
            cert_change: None,
        };
        b.start(url, false, false, cx);
        b
    }

    /// Start loading `url` in the background. History is handled by the caller.
    fn start(&mut self, url: &str, trust_new: bool, download: bool, cx: &mut Cx) {
        let req = NEXT_REQ.fetch_add(1, Ordering::Relaxed);
        if !download {
            self.req = req;
            self.loading = Some(Instant::now());
            self.url = url.to_string();
            self.prompt = None;
        }
        let loader =
            Arc::new(Loader { net: cx.net.clone(), pins: self.pins.clone(), downloads: self.downloads.clone() });
        let url = url.to_string();
        let title = format!("{} {}", if download { "Download" } else { "Open" }, short(&url));
        cx.tasks.spawn(MODULE, title, move |ctx| {
            let loader = loader.clone();
            let request = Request { url: url.clone(), trust_new, download };
            async move {
                let mut loaded = loader.load(&request, &ctx).await;
                loaded.req = req;
                let result = match &loaded.outcome {
                    Outcome::Failed { error, .. } => Err(error.clone()),
                    Outcome::CertChanged { .. } => Err("the server's certificate changed".into()),
                    Outcome::Saved { path, bytes, .. } => {
                        ctx.detail(format!("{} saved to {}", human(*bytes), path.display()));
                        Ok(())
                    }
                    _ => Ok(()),
                };
                ctx.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(loaded) });
                result
            }
        });
    }

    fn navigate(&mut self, url: &str, cx: &mut Cx) {
        if !is_burrow(url) {
            cx.open_url(url);
            return;
        }
        if url != self.url && self.loading.is_none() {
            self.back.push(self.url.clone());
            self.forward.clear();
        }
        self.cert_change = None;
        self.start(url, false, false, cx);
    }

    /// Open a link in a new window (Shift+Enter or the middle button).
    fn open_new(&self, url: &str, cx: &mut Cx) {
        if !is_burrow(url) {
            cx.open_url(url);
            return;
        }
        let w = Browser::new(url, self.pins.clone(), self.downloads.clone(), cx);
        cx.effects.push(Effect::OpenWindow(Box::new(w)));
    }

    fn go_back(&mut self, cx: &mut Cx) -> bool {
        let Some(prev) = self.back.pop() else { return false };
        self.forward.push(self.url.clone());
        self.start(&prev, false, false, cx);
        true
    }

    fn go_forward(&mut self, cx: &mut Cx) -> bool {
        let Some(next) = self.forward.pop() else { return false };
        self.back.push(self.url.clone());
        self.start(&next, false, false, cx);
        true
    }

    fn up(&self) -> Option<String> {
        let u = url::Url::parse(&self.url).ok()?;
        if u.scheme().starts_with("gopher") {
            let g = proto_gopher::GopherUrl::parse(&self.url).ok()?;
            let sel = g.selector.trim_end_matches('/');
            let parent = sel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            if sel.is_empty() {
                return None;
            }
            let mut p = g.clone();
            p.kind = proto_gopher::ItemType::Menu;
            p.selector = format!("{parent}/");
            p.search = None;
            return Some(p.to_string());
        }
        let path = u.path().trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        u.join(&format!("{parent}/")).ok().map(|x| x.to_string())
    }

    fn set_doc(&mut self, doc: Doc) {
        self.doc = doc;
        self.rows_width = 0;
        self.scroll = 0;
        self.sel = if self.doc.flavor == Flavor::Menu { self.doc.links().next() } else { None };
    }

    fn row_of(&self, line: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.line == line)
    }

    fn ensure_visible(&mut self) {
        let Some(row) = self.sel.and_then(|l| self.row_of(l)) else { return };
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + self.view_h {
            self.scroll = row + 1 - self.view_h;
        }
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.view_h)
    }

    fn select_next(&mut self, forward: bool) -> bool {
        let links: Vec<usize> = self.doc.links().collect();
        if links.is_empty() {
            return false;
        }
        let first_visible = self.rows.get(self.scroll).map(|r| r.line).unwrap_or(0);
        let next = match (self.sel, forward) {
            (Some(s), true) => links.iter().copied().find(|&l| l > s),
            (Some(s), false) => links.iter().rev().copied().find(|&l| l < s),
            (None, true) => links.iter().copied().find(|&l| l >= first_visible),
            (None, false) => links.iter().rev().copied().find(|&l| l <= first_visible + self.view_h),
        };
        match next {
            Some(n) => {
                self.sel = Some(n);
                self.ensure_visible();
                true
            }
            None => false,
        }
    }

    fn selected_url(&self) -> Option<String> {
        self.sel.and_then(|i| self.doc.lines.get(i)).and_then(|l| l.link.clone())
    }

    fn scroll_by(&mut self, n: isize) {
        self.scroll = (self.scroll as isize + n).clamp(0, self.max_scroll() as isize) as usize;
    }

    fn on_loaded(&mut self, l: &Loaded, cx: &mut Cx) {
        if l.pins_changed
            && let Some(store) = cx.store
            && let Err(e) = store.kv_set(crate::PINS_KEY, &self.pins.dump())
        {
            tracing::warn!("saving certificate pins: {e}");
        }
        // Downloads report with a toast; they never replace the page.
        if let Outcome::Saved { path, bytes, .. } = &l.outcome {
            cx.notify(Notification::success(format!("Saved {} to {}", human(*bytes), path.display())));
            if l.req == self.req {
                self.loading = None;
            }
            return;
        }
        if l.req != self.req {
            return;
        }
        self.loading = None;
        self.info = l.info.clone();
        match &l.outcome {
            Outcome::Page { url, doc } => {
                self.url = url.clone();
                if let Some(store) = cx.store {
                    let _ = store.add_history(url, &doc.title, MODULE);
                }
                self.set_doc(doc.clone());
            }
            Outcome::Input { url, prompt, sensitive } => {
                self.url = url.clone();
                let label = if prompt.is_empty() { "Input".to_string() } else { prompt.clone() };
                self.prompt = Some(Prompt { label, value: String::new(), sensitive: *sensitive, target: url.clone() });
                if self.rows.is_empty() || self.doc.title == "Loading" {
                    self.set_doc(Doc::message(
                        "Input requested",
                        Kind::Text,
                        &[prompt, "", "Type your answer below and press Enter."],
                    ));
                }
            }
            Outcome::CertChanged { url, host, old, new } => {
                self.url = url.clone();
                self.cert_change = Some(url.clone());
                let lines = [
                    format!("The certificate for {host} is not the one seen before."),
                    String::new(),
                    format!("Pinned:  {}", short_fp(old)),
                    format!("Now:     {}", short_fp(new)),
                    String::new(),
                    "This happens when a server renews its certificate, and also when someone is".into(),
                    "intercepting the connection. Press t to trust the new certificate, or b to go back.".into(),
                ];
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                self.set_doc(Doc::message("Certificate changed", Kind::Error, &refs));
            }
            Outcome::Elsewhere(u) => {
                cx.open_url(u.clone());
                if self.doc.title == "Loading" {
                    self.set_doc(Doc::message(
                        "Opened elsewhere",
                        Kind::Text,
                        &[&format!("{u} opened in its own window.")],
                    ));
                }
            }
            Outcome::Failed { url, error } => {
                self.url = url.clone();
                self.set_doc(
                    Doc::message("Could not load the page", Kind::Error, &[url, "", error]).with_link("Try again", url),
                );
            }
            Outcome::Saved { .. } => unreachable!(),
        }
    }

    fn submit_prompt(&mut self, cx: &mut Cx) {
        let Some(p) = self.prompt.take() else { return };
        let target = if p.target.starts_with("gemini:") {
            match proto_gemini::parse_url(&p.target) {
                Ok(u) => proto_gemini::with_query(&u, &p.value).to_string(),
                Err(_) => return,
            }
        } else {
            match proto_gopher::GopherUrl::parse(&p.target) {
                Ok(g) => g.with_search(&p.value).to_string(),
                Err(_) => return,
            }
        };
        if !p.sensitive {
            self.back.push(self.url.clone());
        }
        self.start(&target, false, false, cx);
    }

    fn prompt_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let Some(p) = &mut self.prompt else { return Handled::No };
        match key.code {
            KeyCode::Enter => self.submit_prompt(cx),
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                p.value.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => p.value.clear(),
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => p.value.push(c),
            _ => return Handled::No,
        }
        Handled::Yes
    }
}

fn is_burrow(url: &str) -> bool {
    matches!(
        url.split(':').next().map(str::to_ascii_lowercase).as_deref(),
        Some("gemini" | "gopher" | "gophers" | "finger")
    )
}

fn short(url: &str) -> String {
    let s = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    if s.chars().count() > 48 { format!("{}…", s.chars().take(47).collect::<String>()) } else { s.to_string() }
}

fn short_fp(fp: &str) -> String {
    let groups: Vec<String> =
        fp.as_bytes().chunks(4).take(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    format!("sha256 {}…", groups.join(" "))
}

pub fn human(bytes: usize) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

impl Window for Browser {
    fn title(&self) -> String {
        if self.loading.is_some() && self.doc.title == "Loading" { short(&self.url) } else { self.doc.title.clone() }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(self.url.clone())
    }
    fn animates(&self) -> bool {
        self.loading.is_some()
    }
    fn wants_text(&self) -> bool {
        self.prompt.is_some()
    }
    fn badge(&self) -> Option<String> {
        self.info.as_ref().map(ConnInfo::badge)
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.prompt.is_some() {
            return vec![("Enter", "Send"), ("Esc", "Cancel")];
        }
        if self.cert_change.is_some() {
            return vec![("t", "Trust new"), ("b", "Back")];
        }
        vec![("Enter", "Open"), ("Tab", "Link"), ("b", "Back"), ("u", "Up"), ("a", "Bookmark"), ("d", "Save")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 2 {
            return;
        }
        // Address bar.
        let addr_style = Style::new().fg(t.input_fg).bg(t.input_bg);
        let spinner = self.loading.map(|s| {
            const F: [&str; 4] = ["◐", "◓", "◑", "◒"];
            format!(" {} loading", F[(s.elapsed().as_millis() / 200) as usize % 4])
        });
        let addr = Line::from(vec![
            Span::styled(format!(" {} ", self.url), addr_style),
            Span::styled(spinner.unwrap_or_default(), t.accent()),
        ]);
        frame.render_widget(Paragraph::new(addr).style(addr_style), Rect { height: 1, ..area });

        let prompt_h = u16::from(self.prompt.is_some());
        let body = Rect { y: area.y + 1, height: area.height - 1 - prompt_h.min(area.height - 1), ..area };
        let width = body.width.saturating_sub(1) as usize;
        if width != self.rows_width {
            self.rows = layout(&self.doc, width);
            self.rows_width = width;
            self.ensure_visible();
        }
        self.view_h = body.height as usize;
        self.scroll = self.scroll.min(self.max_scroll());

        let mut lines = Vec::with_capacity(self.view_h);
        for row in self.rows.iter().skip(self.scroll).take(self.view_h) {
            let dl = &self.doc.lines[row.line];
            let selected = self.sel == Some(row.line);
            let style = match &dl.kind {
                _ if selected => t.selection(),
                Kind::Heading(1) => t.accent().add_modifier(Modifier::UNDERLINED),
                Kind::Heading(_) => t.accent(),
                Kind::Link(_) => t.link(),
                Kind::Quote => t.dim().add_modifier(Modifier::ITALIC),
                Kind::Error => Style::new().fg(t.error),
                Kind::Info if self.doc.flavor == Flavor::Menu => t.window(),
                _ => t.window(),
            };
            let prefix_style = match &dl.kind {
                Kind::Link(_) if self.doc.flavor == Flavor::Menu => {
                    if selected {
                        t.selection()
                    } else {
                        t.dim()
                    }
                }
                Kind::Link(_) => t.accent(),
                _ => t.dim(),
            };
            let mut text = row.text.clone();
            let avail = width.saturating_sub(row.prefix.width());
            if text.width() > avail {
                text = alouette_ui::chrome::truncate(&text, avail);
            }
            lines.push(Line::from(vec![Span::styled(row.prefix.clone(), prefix_style), Span::styled(text, style)]));
        }
        frame.render_widget(Paragraph::new(lines).style(t.window()), body);
        if self.rows.len() > self.view_h {
            use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
            let mut st = ScrollbarState::new(self.max_scroll()).position(self.scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).style(t.dim()),
                body,
                &mut st,
            );
        }

        if let Some(p) = &self.prompt {
            let shown = if p.sensitive { "•".repeat(p.value.chars().count()) } else { p.value.clone() };
            let y = area.y + area.height - 1;
            let label = format!(" {}: ", p.label);
            let line =
                Line::from(vec![Span::styled(label.clone(), t.accent()), Span::styled(shown.clone(), addr_style)]);
            frame.render_widget(Paragraph::new(line).style(addr_style), Rect { y, height: 1, ..area });
            if cx.focused {
                let x = (area.x as usize + label.width() + shown.width()).min(area.right().saturating_sub(1) as usize);
                frame.set_cursor_position((x as u16, y));
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if self.prompt.is_some() {
            return self.prompt_key(key, cx);
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.view_h.saturating_sub(1).max(1) as isize;
        let menu = self.doc.flavor == Flavor::Menu;
        match key.code {
            KeyCode::Enter => match self.selected_url() {
                Some(u) if shift => self.open_new(&u, cx),
                Some(u) => self.navigate(&u, cx),
                None => return Handled::No,
            },
            KeyCode::Down | KeyCode::Char('j') if menu => {
                if !self.select_next(true) {
                    self.scroll_by(1)
                }
            }
            KeyCode::Up | KeyCode::Char('k') if menu => {
                if !self.select_next(false) {
                    self.scroll_by(-1)
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Tab => {
                self.select_next(true);
            }
            KeyCode::BackTab => {
                self.select_next(false);
            }
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                if menu {
                    self.sel = self.doc.links().next();
                }
            }
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Backspace | KeyCode::Char('b') | KeyCode::Left
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if !self.go_back(cx) {
                    return Handled::No;
                }
            }
            KeyCode::Char('f') | KeyCode::Right => {
                if !self.go_forward(cx) {
                    return Handled::No;
                }
            }
            KeyCode::Char('r') => {
                let u = self.url.clone();
                self.start(&u, false, false, cx);
            }
            KeyCode::Char('u') => match self.up() {
                Some(u) => self.navigate(&u, cx),
                None => cx.notify(Notification::info("Already at the top")),
            },
            KeyCode::Char('t') if self.cert_change.is_some() => {
                let u = self.cert_change.take().unwrap_or_default();
                self.start(&u, true, false, cx);
            }
            KeyCode::Char('a') => {
                if let Some(store) = cx.store {
                    match store.set_bookmark(&self.url, &self.doc.title, &[MODULE]) {
                        Ok(()) => cx.notify(Notification::success(format!("Bookmarked {}", self.doc.title))),
                        Err(e) => cx.notify(Notification::error(format!("Bookmark failed: {e}"))),
                    }
                }
            }
            KeyCode::Char('d') => {
                let u = self.selected_url().filter(|u| is_burrow(u)).unwrap_or_else(|| self.url.clone());
                self.start(&u, false, true, cx);
                cx.notify(Notification::info(format!("Downloading {}", short(&u))));
            }
            KeyCode::Char('i') => {
                let text = match &self.info {
                    Some(i) => {
                        let mut s = format!("{}  via {}", i.badge(), i.via);
                        if let Some(t) = &i.tls {
                            s.push_str(&format!(
                                "  {} {}",
                                t.key_exchange,
                                t.cert_sha256.as_deref().map(short_fp).unwrap_or_default()
                            ));
                        }
                        s
                    }
                    None => "No connection yet".into(),
                };
                cx.notify(Notification::info(text));
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Middle) => {
                let row = (ev.row as usize).checked_sub(area.y as usize + 1).map(|r| r + self.scroll);
                let Some(line) = row.and_then(|r| self.rows.get(r)).map(|r| r.line) else { return Handled::No };
                if let Some(u) = self.doc.lines[line].link.clone() {
                    self.sel = Some(line);
                    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Middle)) {
                        self.open_new(&u, cx);
                    } else {
                        self.navigate(&u, cx);
                    }
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        if let AppEvent::Module { module: MODULE, payload } = ev
            && let Some(l) = payload.downcast_ref::<Loaded>()
        {
            self.on_loaded(l, cx);
        }
    }
}
