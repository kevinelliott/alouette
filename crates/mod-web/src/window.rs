//! The Web window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alouette_core::event::ModulePayload;
use alouette_core::{AppEvent, Notification};
use alouette_html::layout::Options;
use alouette_html::{FieldKind, Fx, Line as HLine, Page, layout};
use alouette_net::ConnInfo;
use alouette_net::web::{Browser, WebRequest};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::fetch::{Body, Fetched, Loaded};
use crate::{MODULE, pages};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Something on the page you can move to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Link(usize),
    Field(usize),
    Image(usize),
}

#[derive(Debug, Clone)]
enum PromptKind {
    Address,
    Find,
    Field(usize),
}

#[derive(Debug, Clone)]
struct Prompt {
    kind: PromptKind,
    label: String,
    value: String,
    secret: bool,
}

/// A select box opened as a list.
#[derive(Debug, Clone)]
struct Choice {
    field: usize,
    sel: usize,
}

#[derive(Debug, Clone)]
struct Entry {
    url: String,
    scroll: usize,
    /// A form post can't be fetched again without asking; keep it out of history.
    post: bool,
}

pub struct WebWindow {
    id: u64,
    browser: Browser,
    downloads: PathBuf,
    url: String,
    title: String,
    html: Option<String>,
    full: Option<Page>,
    reader_page: Option<Page>,
    reader: bool,
    text: Option<String>,
    lines: Vec<HLine>,
    lines_width: usize,
    /// Each target with the first line it appears on.
    targets: Vec<(Target, usize)>,
    sel: Option<Target>,
    scroll: usize,
    view_h: usize,
    back: Vec<Entry>,
    forward: Vec<Entry>,
    req: u64,
    loading: Option<Instant>,
    loading_url: String,
    pending_scroll: Option<usize>,
    prompt: Option<Prompt>,
    choice: Option<Choice>,
    hints: Option<(Vec<(String, Target)>, String)>,
    find: Option<String>,
    status: Option<String>,
    info: Option<(ConnInfo, &'static str)>,
    error: Option<String>,
    body_area: Rect,
    current_post: bool,
}

fn is_web(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("web:") || l.starts_with("about:")
}

/// Turn what was typed into a URL: an address, or a search.
pub fn resolve_input(input: &str, search: &str) -> String {
    let s = input.trim();
    if s.contains("://") || s.starts_with("web:") || s.starts_with("about:") || s.starts_with("mailto:") {
        return s.to_string();
    }
    let looks_like_host = !s.contains(' ')
        && (s.contains('.') || s.starts_with("localhost") || s.contains(':'))
        && !s.ends_with('.')
        && url::Url::parse(&format!("https://{s}")).is_ok_and(|u| u.host_str().is_some());
    if looks_like_host {
        let local = s.starts_with("localhost") || s.starts_with("127.") || s.starts_with("[::1]");
        return format!("{}://{s}", if local { "http" } else { "https" });
    }
    let q: String = url::form_urlencoded::byte_serialize(s.as_bytes()).collect();
    if search.contains("{}") { search.replace("{}", &q) } else { format!("{search}{q}") }
}

impl WebWindow {
    pub fn new(url: &str, browser: Browser, downloads: PathBuf, cx: &mut Cx) -> Self {
        let mut w = Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            browser,
            downloads,
            url: url.to_string(),
            title: "Web".into(),
            html: None,
            full: None,
            reader_page: None,
            reader: false,
            text: None,
            lines: vec![],
            lines_width: 0,
            targets: vec![],
            sel: None,
            scroll: 0,
            view_h: 10,
            back: vec![],
            forward: vec![],
            req: 0,
            loading: None,
            loading_url: String::new(),
            pending_scroll: None,
            prompt: None,
            choice: None,
            hints: None,
            find: None,
            status: None,
            info: None,
            error: None,
            body_area: Rect::default(),
            current_post: false,
        };
        w.start(WebRequest::get(url), false, cx);
        w
    }

    fn page(&self) -> Option<&Page> {
        if self.reader { self.reader_page.as_ref().or(self.full.as_ref()) } else { self.full.as_ref() }
    }

    fn page_mut(&mut self) -> Option<&mut Page> {
        if self.reader && self.reader_page.is_some() { self.reader_page.as_mut() } else { self.full.as_mut() }
    }

    fn host(url: &str) -> String {
        url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
    }

    fn reader_for(&self, url: &str, cx: &Cx) -> bool {
        let key = format!("web/reader/{}", Self::host(url));
        match cx.store.and_then(|s| s.kv_get(&key).ok().flatten()) {
            Some(v) => v == "on",
            None => cx.config.web.reader,
        }
    }

    /// Load a request. History is the caller's business.
    fn start(&mut self, req: WebRequest, save: bool, cx: &mut Cx) {
        let url = req.url.clone();
        if url.starts_with("web:") || url.starts_with("about:") {
            self.show_internal(&url, cx);
            return;
        }
        let reqid = NEXT.fetch_add(1, Ordering::Relaxed);
        if !save {
            self.req = reqid;
            self.loading = Some(Instant::now());
            self.loading_url = url.clone();
            self.hints = None;
            self.prompt = None;
        }
        let reader = self.reader_for(&url, cx);
        let browser = self.browser.clone();
        let downloads = self.downloads.clone();
        let window = self.id;
        let title = format!("{} {}", if save { "Download" } else { "Open" }, short(&url));
        cx.tasks.spawn(MODULE, title, move |ctx| {
            let browser = browser.clone();
            let req = req.clone();
            let downloads = downloads.clone();
            async move {
                let result = crate::fetch::load(&browser, req, reader, save, &downloads).await;
                let out = match &result {
                    Ok(f) => match &f.body {
                        Body::Saved(p, n) => {
                            ctx.detail(format!("{} saved to {}", human(*n), p.display()));
                            Ok(())
                        }
                        _ => Ok(()),
                    },
                    Err(e) => Err(e.clone()),
                };
                ctx.send(AppEvent::Module {
                    module: MODULE,
                    payload: ModulePayload::new(Loaded { window, req: reqid, result }),
                });
                out
            }
        });
    }

    fn show_internal(&mut self, url: &str, cx: &mut Cx) {
        let html = pages::internal(url, cx);
        self.set_html(url, html, false);
        self.info = None;
        self.loading = None;
    }

    fn set_html(&mut self, url: &str, html: String, reader: bool) {
        let page = Page::parse(&html, url);
        let rp = reader.then(|| Page::parse_reader(&html, url));
        self.set_page(url, html, page, rp, reader);
    }

    fn set_page(&mut self, url: &str, html: String, page: Page, reader_page: Option<Page>, reader: bool) {
        self.url = url.to_string();
        self.title = if page.title.is_empty() { short(url) } else { page.title.clone() };
        self.html = Some(html);
        self.full = Some(page);
        self.reader_page = reader_page;
        self.reader = reader;
        self.text = None;
        self.relayout_soon();
        self.sel = None;
        self.scroll = self.pending_scroll.take().unwrap_or(0);
        self.find = None;
        self.error = None;
    }

    fn relayout_soon(&mut self) {
        self.lines_width = 0;
    }

    fn relayout(&mut self, width: usize) {
        let lines = if let Some(t) = &self.text {
            t.lines()
                .map(|l| HLine {
                    segs: vec![alouette_html::Seg {
                        text: l.replace('\t', "    "),
                        fx: Fx::CODE,
                        ..Default::default()
                    }],
                    block: 0,
                })
                .collect()
        } else if let Some(p) = self.page() {
            layout(p, width, &Options::default())
        } else {
            vec![]
        };
        let mut targets: Vec<(Target, usize)> = vec![];
        for (i, l) in lines.iter().enumerate() {
            for s in &l.segs {
                let t = if let Some(f) = s.field {
                    Some(Target::Field(f))
                } else if let Some(k) = s.link {
                    Some(Target::Link(k))
                } else {
                    s.image.map(Target::Image)
                };
                if let Some(t) = t
                    && !targets.iter().any(|(o, _)| *o == t)
                {
                    targets.push((t, i));
                }
            }
        }
        self.lines = lines;
        self.targets = targets;
        self.lines_width = width;
    }

    fn navigate(&mut self, url: &str, cx: &mut Cx) {
        if !is_web(url) {
            cx.open_url(url);
            return;
        }
        // Same page, different fragment: stay put.
        if let (Ok(a), Ok(b)) = (url::Url::parse(url), url::Url::parse(&self.url))
            && a.fragment().is_some()
            && {
                let mut a2 = a.clone();
                a2.set_fragment(None);
                let mut b2 = b.clone();
                b2.set_fragment(None);
                a2 == b2
            }
        {
            self.jump_to_fragment(a.fragment().unwrap_or(""));
            return;
        }
        self.push_history();
        self.forward.clear();
        self.start(
            WebRequest {
                referer: Some(self.url.clone()).filter(|u| is_web(u) && !u.starts_with("web:")),
                ..WebRequest::get(url)
            },
            false,
            cx,
        );
    }

    fn push_history(&mut self) {
        if self.full.is_some() || self.text.is_some() {
            self.back.push(Entry { url: self.url.clone(), scroll: self.scroll, post: self.current_post });
        }
    }

    fn jump_to_fragment(&mut self, frag: &str) {
        // Headings stand in for anchors: find one whose words match.
        let want = frag.replace(['-', '_'], " ").to_lowercase();
        if let Some(i) = self
            .lines
            .iter()
            .position(|l| l.segs.iter().any(|s| s.fx.has(Fx::HEADING)) && l.text().to_lowercase().contains(want.trim()))
        {
            self.scroll = i;
        }
    }

    fn go_back(&mut self, cx: &mut Cx) -> bool {
        while let Some(e) = self.back.pop() {
            if e.post {
                continue;
            }
            self.forward.push(Entry { url: self.url.clone(), scroll: self.scroll, post: self.current_post });
            self.pending_scroll = Some(e.scroll);
            self.current_post = false;
            self.start(WebRequest::get(&e.url), false, cx);
            return true;
        }
        false
    }

    fn go_forward(&mut self, cx: &mut Cx) -> bool {
        let Some(e) = self.forward.pop() else { return false };
        self.back.push(Entry { url: self.url.clone(), scroll: self.scroll, post: self.current_post });
        self.pending_scroll = Some(e.scroll);
        self.start(WebRequest::get(&e.url), false, cx);
        true
    }

    fn on_loaded(&mut self, l: &Loaded, cx: &mut Cx) {
        if l.req != self.req {
            return;
        }
        self.loading = None;
        match &l.result {
            Err(e) => {
                self.error = Some(e.clone());
                self.status = None;
                if self.full.is_none() && self.text.is_none() {
                    self.title = short(&self.loading_url);
                    self.url = self.loading_url.clone();
                }
            }
            Ok(f) => self.on_fetched(f.clone(), cx),
        }
    }

    fn on_fetched(&mut self, f: Fetched, cx: &mut Cx) {
        self.info = Some((f.info.clone(), f.protocol));
        match f.body {
            Body::Html { html, page, reader } => {
                let want_reader = reader.is_some();
                if let Some((secs, target)) = page.refresh.clone()
                    && secs <= 5
                    && target != f.url
                {
                    self.status = Some(format!("Moving on to {}", short(&target)));
                    self.start(WebRequest::get(target), false, cx);
                    return;
                }
                let status = (!(200..300).contains(&f.status)).then(|| format!("The server said {}", f.status));
                self.set_page(&f.url, html, *page, reader.map(|r| *r), want_reader);
                self.status = status;
                if let Some(frag) = url::Url::parse(&f.url).ok().and_then(|u| u.fragment().map(str::to_string)) {
                    self.relayout(self.body_area.width.saturating_sub(1).max(20) as usize);
                    self.jump_to_fragment(&frag);
                }
                if let Some(s) = cx.store {
                    let _ = s.add_history(&f.url, &self.title, MODULE);
                }
            }
            Body::Text(t) => {
                self.url = f.url.clone();
                self.title = short(&f.url);
                self.full = None;
                self.reader_page = None;
                self.html = None;
                self.text = Some(t);
                self.relayout_soon();
                self.scroll = self.pending_scroll.take().unwrap_or(0);
                self.error = None;
            }
            Body::Saved(p, n) => {
                cx.notify(Notification::success(format!("Saved {} to {}", human(n), p.display())));
                // Nothing new to show: undo the history step this load made.
                if self.back.last().is_some_and(|e| e.url == self.url) {
                    self.back.pop();
                }
            }
            Body::Elsewhere(u) => cx.open_url(u),
        }
    }

    fn ensure_visible(&mut self) {
        let Some(t) = self.sel else { return };
        let Some(&(_, line)) = self.targets.iter().find(|(x, _)| *x == t) else { return };
        if line < self.scroll {
            self.scroll = line.saturating_sub(1);
        } else if line >= self.scroll + self.view_h {
            self.scroll = (line + 2).saturating_sub(self.view_h);
        }
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.view_h.max(1))
    }

    fn scroll_by(&mut self, n: isize) {
        self.scroll = (self.scroll as isize + n).clamp(0, self.max_scroll() as isize) as usize;
        // Keep the selection on screen, or let it go.
        if let Some(t) = self.sel
            && let Some(&(_, line)) = self.targets.iter().find(|(x, _)| *x == t)
            && (line < self.scroll || line >= self.scroll + self.view_h)
        {
            self.sel = None;
        }
    }

    fn select_next(&mut self, forward: bool) {
        if self.targets.is_empty() {
            return;
        }
        let pos = self.sel.and_then(|t| self.targets.iter().position(|(x, _)| *x == t));
        let visible = |i: usize, s: &Self| {
            let l = s.targets[i].1;
            l >= s.scroll && l < s.scroll + s.view_h
        };
        let next = match pos {
            Some(p) if forward => (p + 1 < self.targets.len()).then_some(p + 1),
            Some(p) => p.checked_sub(1),
            None if forward => (0..self.targets.len()).find(|&i| visible(i, self)).or(Some(0)),
            None => (0..self.targets.len()).rev().find(|&i| visible(i, self)),
        };
        if let Some(n) = next {
            self.sel = Some(self.targets[n].0);
            self.ensure_visible();
        }
    }

    fn target_url(&self, t: Target) -> Option<String> {
        let p = self.page()?;
        match t {
            Target::Link(i) => p.links.get(i).map(|l| l.url.clone()),
            Target::Image(i) => p.images.get(i).map(|im| {
                im.link.and_then(|l| p.links.get(l)).map(|l| l.url.clone()).unwrap_or_else(|| im.src.clone())
            }),
            Target::Field(_) => None,
        }
    }

    fn activate(&mut self, t: Target, new_window: bool, cx: &mut Cx) {
        match t {
            Target::Link(_) | Target::Image(_) => {
                let Some(u) = self.target_url(t) else { return };
                let is_image = matches!(t, Target::Image(i) if self.page().and_then(|p| p.images.get(i)).is_some_and(|im| im.link.is_none()));
                if is_image {
                    open_external(&u, &cx.config.web.external_browser);
                    cx.notify(Notification::info(format!("Opened {} in your browser", short(&u))));
                } else if new_window && is_web(&u) {
                    cx.open_url(format!("web:{u}"));
                } else {
                    self.navigate(&u, cx);
                }
            }
            Target::Field(f) => self.activate_field(f, cx),
        }
    }

    fn activate_field(&mut self, f: usize, cx: &mut Cx) {
        let Some(field) = self.page().and_then(|p| p.fields.get(f)).cloned() else { return };
        match &field.kind {
            FieldKind::Text | FieldKind::TextArea | FieldKind::Password => {
                let label = if !field.placeholder.is_empty() {
                    field.placeholder.clone()
                } else if !field.name.is_empty() {
                    field.name.clone()
                } else {
                    "Text".into()
                };
                self.prompt = Some(Prompt {
                    kind: PromptKind::Field(f),
                    label,
                    value: field.value.clone(),
                    secret: field.kind == FieldKind::Password,
                });
            }
            FieldKind::Checkbox => {
                if let Some(p) = self.page_mut() {
                    p.fields[f].checked = !p.fields[f].checked;
                }
                self.relayout_soon();
            }
            FieldKind::Radio => {
                if let Some(p) = self.page_mut() {
                    p.check_radio(f);
                }
                self.relayout_soon();
            }
            FieldKind::Select(opts) => {
                let sel = opts.iter().position(|(v, _)| *v == field.value).unwrap_or(0);
                self.choice = Some(Choice { field: f, sel });
            }
            FieldKind::Submit | FieldKind::Button => {
                if let Some(form) = field.form {
                    self.submit(form, Some(f), cx);
                }
            }
            FieldKind::Reset => {
                if let Some(html) = self.html.clone() {
                    let url = self.url.clone();
                    let reader = self.reader;
                    let scroll = self.scroll;
                    self.set_html(&url, html, reader);
                    self.scroll = scroll;
                }
            }
            FieldKind::File => cx.notify(Notification::warning("Uploading files from a form is not supported yet")),
            FieldKind::Hidden => {}
        }
    }

    fn submit(&mut self, form: usize, pressed: Option<usize>, cx: &mut Cx) {
        let Some((method, url, body, ct)) = self.page().and_then(|p| p.submit(form, pressed)) else { return };
        if self.page().and_then(|p| p.forms.get(form)).is_some_and(|f| f.enctype.contains("multipart")) {
            cx.notify(Notification::warning("This form sends files; it is sent without them"));
        }
        if url.starts_with("http:") && self.url.starts_with("https:") && method == "POST" {
            cx.notify(Notification::warning("This form is sent without encryption"));
        }
        self.push_history();
        self.forward.clear();
        self.current_post = method == "POST";
        let req = WebRequest {
            url,
            method,
            body: body.map(String::into_bytes),
            content_type: Some(ct).filter(|c| !c.is_empty()),
            referer: Some(self.url.clone()),
            headers: vec![],
        };
        self.start(req, false, cx);
    }

    fn toggle_reader(&mut self, cx: &mut Cx) {
        let Some(html) = self.html.clone() else { return };
        let on = !self.reader;
        if on && self.reader_page.is_none() {
            self.reader_page = Some(Page::parse_reader(&html, &self.url));
        }
        self.reader = on;
        self.relayout_soon();
        self.scroll = 0;
        self.sel = None;
        if let Some(s) = cx.store {
            let _ = s.kv_set(&format!("web/reader/{}", Self::host(&self.url)), if on { "on" } else { "off" });
        }
        self.status = Some(if on { "Reader mode: on (R for the whole page)" } else { "Reader mode: off" }.into());
    }

    fn save_markdown(&mut self, cx: &mut Cx) {
        let Some(p) = self.page() else { return };
        let md = alouette_html::markdown::to_markdown(p);
        let slug: String = self
            .title
            .chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .take(8)
            .collect::<Vec<_>>()
            .join("-");
        let name = format!("{}.md", if slug.is_empty() { "page" } else { &slug });
        let _ = std::fs::create_dir_all(&self.downloads);
        let path = crate::fetch::unique_path(&self.downloads, &name);
        match std::fs::write(&path, md) {
            Ok(()) => cx.notify(Notification::success(format!("Saved {}", path.display()))),
            Err(e) => cx.notify(Notification::error(format!("Could not save: {e}"))),
        }
    }

    fn find_next(&mut self, forward: bool) {
        let Some(q) = self.find.clone().filter(|q| !q.is_empty()) else { return };
        let q = q.to_lowercase();
        let n = self.lines.len();
        if n == 0 {
            return;
        }
        let start = self.scroll;
        for k in 1..=n {
            let i = if forward { (start + k) % n } else { (start + n - k) % n };
            if self.lines[i].text().to_lowercase().contains(&q) {
                self.scroll = i.min(self.max_scroll()).max(if i < self.scroll { i } else { 0 });
                self.scroll = i.saturating_sub(self.view_h / 3).min(self.max_scroll());
                self.status = Some(format!("Found on line {}", i + 1));
                return;
            }
        }
        self.status = Some(format!("“{q}” is not on this page"));
    }

    fn start_hints(&mut self) {
        let visible: Vec<Target> = self
            .targets
            .iter()
            .filter(|(t, l)| *l >= self.scroll && *l < self.scroll + self.view_h && !matches!(t, Target::Field(f) if self.page().and_then(|p| p.fields.get(*f)).is_some_and(|f| !f.interactive())))
            .map(|(t, _)| *t)
            .collect();
        if visible.is_empty() {
            self.status = Some("No links on screen".into());
            return;
        }
        let labels = hint_labels(visible.len());
        self.hints = Some((labels.into_iter().zip(visible).collect(), String::new()));
    }

    fn prompt_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let Some(p) = self.prompt.as_mut() else { return Handled::No };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let p = self.prompt.take().unwrap();
                match p.kind {
                    PromptKind::Address => {
                        let url = resolve_input(&p.value, &cx.config.web.search);
                        if !url.is_empty() {
                            self.navigate(&url, cx);
                        }
                    }
                    PromptKind::Find => {
                        self.find = Some(p.value);
                        self.scroll = self.scroll.saturating_sub(1);
                        self.find_next(true);
                    }
                    PromptKind::Field(f) => {
                        let form = {
                            let Some(page) = self.page_mut() else { return Handled::Yes };
                            page.fields[f].value = p.value;
                            page.fields[f].form
                        };
                        self.relayout_soon();
                        // A lone text box, such as a search, is sent at once.
                        if let Some(form) = form {
                            let texts = self
                                .page()
                                .map(|pg| {
                                    pg.fields
                                        .iter()
                                        .filter(|x| {
                                            x.form == Some(form)
                                                && matches!(
                                                    x.kind,
                                                    FieldKind::Text | FieldKind::TextArea | FieldKind::Password
                                                )
                                        })
                                        .count()
                                })
                                .unwrap_or(0);
                            if texts == 1 {
                                self.submit(form, None, cx);
                            } else {
                                self.select_next(true);
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                p.value.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => p.value.clear(),
            KeyCode::Char(c) => p.value.push(c),
            _ => {}
        }
        Handled::Yes
    }

    fn choice_key(&mut self, key: KeyEvent) -> Handled {
        let Some(field) = self.choice.as_ref().map(|c| c.field) else { return Handled::No };
        let n = match self.page().and_then(|p| p.fields.get(field)).map(|f| f.kind.clone()) {
            Some(FieldKind::Select(o)) => o.len(),
            _ => 0,
        };
        let c = self.choice.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => self.choice = None,
            KeyCode::Up | KeyCode::Char('k') => c.sel = c.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => c.sel = (c.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let (f, sel) = (c.field, c.sel);
                self.choice = None;
                if let Some(p) = self.page_mut()
                    && let FieldKind::Select(opts) = p.fields[f].kind.clone()
                    && let Some((v, _)) = opts.get(sel)
                {
                    p.fields[f].value = v.clone();
                }
                self.relayout_soon();
            }
            _ => {}
        }
        Handled::Yes
    }

    fn hint_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let Some((labels, typed)) = self.hints.as_mut() else { return Handled::No };
        match key.code {
            KeyCode::Esc => self.hints = None,
            KeyCode::Backspace => {
                typed.pop();
            }
            KeyCode::Char(c) => {
                typed.push(c.to_ascii_lowercase());
                let matching: Vec<&(String, Target)> =
                    labels.iter().filter(|(l, _)| l.starts_with(typed.as_str())).collect();
                match matching.as_slice() {
                    [] => self.hints = None,
                    [(l, t)] if *l == *typed => {
                        let t = *t;
                        let shift = key.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase();
                        self.hints = None;
                        self.sel = Some(t);
                        self.activate(t, shift, cx);
                    }
                    _ => {}
                }
            }
            _ => self.hints = None,
        }
        Handled::Yes
    }

    fn style_for(&self, s: &alouette_html::Seg, cx: &DrawCx) -> Style {
        let t = cx.theme;
        let selected = match self.sel {
            Some(Target::Link(i)) => s.link == Some(i) && s.field.is_none(),
            Some(Target::Field(i)) => s.field == Some(i),
            Some(Target::Image(i)) => s.image == Some(i) && s.link.is_none(),
            None => false,
        };
        if selected {
            return t.selection();
        }
        if s.field.is_some() {
            return Style::new().fg(t.input_fg).bg(t.input_bg);
        }
        let mut st = if s.link.is_some() || s.image.is_some() && s.link.is_some() {
            t.link()
        } else if s.fx.has(Fx::HEADING) {
            t.accent()
        } else if s.fx.has(Fx::DIM) {
            t.dim()
        } else if s.fx.has(Fx::CODE) {
            Style::new().fg(t.success).bg(t.window_bg)
        } else {
            t.window()
        };
        if s.fx.has(Fx::BOLD) {
            st = st.add_modifier(Modifier::BOLD);
        }
        if s.fx.has(Fx::ITALIC) {
            st = st.add_modifier(Modifier::ITALIC);
        }
        if s.fx.has(Fx::UNDERLINE) {
            st = st.add_modifier(Modifier::UNDERLINED);
        }
        if s.fx.has(Fx::STRIKE) {
            st = st.add_modifier(Modifier::CROSSED_OUT);
        }
        if s.fx.has(Fx::MARK) {
            st = st.add_modifier(Modifier::REVERSED);
        }
        st
    }

    fn line_at(&self, row: u16) -> Option<usize> {
        let r = row.checked_sub(self.body_area.y)? as usize;
        (r < self.body_area.height as usize).then_some(r + self.scroll)
    }

    fn target_at(&self, line: usize, col: u16) -> Option<Target> {
        let l = self.lines.get(line)?;
        let col = col.checked_sub(self.body_area.x)? as usize;
        let mut x = 0;
        for s in &l.segs {
            let w = s.text.width();
            if col >= x && col < x + w {
                return s.field.map(Target::Field).or(s.link.map(Target::Link)).or(s.image.map(Target::Image));
            }
            x += w;
        }
        None
    }
}

/// Labels for link hints: one letter while there are few, two otherwise.
pub fn hint_labels(n: usize) -> Vec<String> {
    const KEYS: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";
    if n <= KEYS.len() {
        return KEYS[..n].iter().map(|&c| (c as char).to_string()).collect();
    }
    let mut out = vec![];
    'outer: for &a in KEYS {
        for &b in KEYS {
            out.push(format!("{}{}", a as char, b as char));
            if out.len() == n {
                break 'outer;
            }
        }
    }
    out
}

pub fn short(url: &str) -> String {
    let s = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    alouette_ui::chrome::truncate(s.trim_end_matches('/'), 48)
}

pub fn human(bytes: usize) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

/// Open a URL in the person's own browser.
pub fn open_external(url: &str, command: &str) {
    if !command.trim().is_empty() {
        let mut parts = command.split_whitespace();
        if let Some(prog) = parts.next() {
            let _ = std::process::Command::new(prog)
                .args(parts)
                .arg(url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        return;
    }
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    if let Err(e) = r {
        tracing::warn!("could not open {url}: {e}");
    }
}

impl Window for WebWindow {
    fn title(&self) -> String {
        if self.loading.is_some() && self.full.is_none() && self.text.is_none() {
            short(&self.loading_url)
        } else {
            self.title.clone()
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        if self.url.starts_with("web:") || self.url.starts_with("about:") {
            Some(self.url.clone())
        } else {
            Some(format!("web:{}", self.url))
        }
    }
    fn animates(&self) -> bool {
        self.loading.is_some()
    }
    fn wants_text(&self) -> bool {
        self.prompt.is_some() || self.hints.is_some()
    }
    fn badge(&self) -> Option<String> {
        self.info.as_ref().map(|(i, proto)| {
            let p = match *proto {
                "h2" => "HTTP/2",
                _ => "HTTP/1.1",
            };
            format!("{} {p}", i.badge())
        })
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.prompt.is_some() {
            return vec![("Enter", "OK"), ("Esc", "Cancel")];
        }
        if self.hints.is_some() {
            return vec![("a-z", "Follow"), ("Esc", "Cancel")];
        }
        vec![
            ("Tab", "Next"),
            ("f", "Hints"),
            ("o", "Go to"),
            ("b", "Back"),
            ("R", "Reader"),
            ("/", "Find"),
            ("a", "Bookmark"),
            ("O", "Browser"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 3 {
            return;
        }
        let input = Style::new().fg(t.input_fg).bg(t.input_bg);
        let spinner = self.loading.map(|s| {
            const F: [&str; 4] = ["◐", "◓", "◑", "◒"];
            format!(" {} ", F[(s.elapsed().as_millis() / 200) as usize % 4])
        });
        let shown_url = if self.loading.is_some() { &self.loading_url } else { &self.url };
        let lock = if shown_url.starts_with("https:") {
            "🔒 "
        } else if shown_url.starts_with("http:") {
            "⚠ "
        } else {
            ""
        };
        let mut top = vec![Span::styled(format!(" {lock}{shown_url} "), input)];
        if self.reader {
            top.push(Span::styled(" Reader ", t.accent().add_modifier(Modifier::REVERSED)));
        }
        if let Some(s) = spinner {
            top.push(Span::styled(s, t.accent()));
        }
        frame.render_widget(Paragraph::new(Line::from(top)).style(input), Rect { height: 1, ..area });

        let body = Rect { y: area.y + 1, height: area.height - 2, ..area };
        self.body_area = body;
        let width = body.width.saturating_sub(1).max(10) as usize;
        if width != self.lines_width {
            self.relayout(width);
        }
        self.view_h = body.height as usize;
        self.scroll = self.scroll.min(self.max_scroll());

        let mut out: Vec<Line> = vec![];
        if let Some(e) = &self.error {
            out.push(Line::from(Span::styled(
                format!("Could not load {}", short(&self.loading_url)),
                Style::new().fg(t.error).add_modifier(Modifier::BOLD),
            )));
            out.push(Line::from(Span::styled(e.clone(), Style::new().fg(t.error))));
            out.push(Line::default());
            out.push(Line::from(Span::styled(
                "Press r to try again, b to go back or O to open it in your browser.",
                t.dim(),
            )));
            out.push(Line::default());
        }
        if self.page().is_some_and(|p| p.needs_script) && self.scroll == 0 {
            out.push(Line::from(Span::styled(
                "This page needs JavaScript to show much. Press O to open it in your browser.",
                Style::new().fg(t.warning),
            )));
            out.push(Line::default());
        }
        let hint_map: Vec<(String, Target)> = self
            .hints
            .as_ref()
            .map(|(l, typed)| l.iter().filter(|(x, _)| x.starts_with(typed.as_str())).cloned().collect())
            .unwrap_or_default();
        let room = (self.view_h).saturating_sub(out.len());
        let find = self.find.as_ref().map(|f| f.to_lowercase()).filter(|f| !f.is_empty());
        for l in self.lines.iter().skip(self.scroll).take(room) {
            let mut spans = vec![];
            let mut hinted: Vec<Target> = vec![];
            for s in &l.segs {
                let tgt = s.field.map(Target::Field).or(s.link.map(Target::Link)).or(s.image.map(Target::Image));
                let mut style = self.style_for(s, cx);
                if let Some(q) = &find
                    && s.text.to_lowercase().contains(q.as_str())
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some(tg) = tgt
                    && !hinted.contains(&tg)
                    && let Some((label, _)) = hint_map.iter().find(|(_, x)| *x == tg)
                {
                    hinted.push(tg);
                    spans.push(Span::styled(
                        label.clone(),
                        Style::new().fg(t.selection_fg).bg(t.warning).add_modifier(Modifier::BOLD),
                    ));
                }
                spans.push(Span::styled(s.text.clone(), style));
            }
            out.push(Line::from(spans));
        }
        if self.lines.is_empty() && self.error.is_none() && self.loading.is_some() {
            out.push(Line::from(Span::styled("Loading…", t.dim())));
        }
        frame
            .render_widget(Paragraph::new(out).style(t.window()), Rect { width: body.width.saturating_sub(1), ..body });
        if self.lines.len() > self.view_h {
            use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
            let mut st = ScrollbarState::new(self.max_scroll()).position(self.scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).style(t.dim()),
                body,
                &mut st,
            );
        }

        // The bottom line: a prompt, the selected link, or a message.
        let y = area.bottom() - 1;
        let foot = Rect { y, height: 1, ..area };
        if let Some(p) = &self.prompt {
            let shown = if p.secret { "•".repeat(p.value.chars().count()) } else { p.value.clone() };
            let label = format!(" {}: ", p.label);
            let line = Line::from(vec![Span::styled(label.clone(), t.accent()), Span::styled(shown.clone(), input)]);
            frame.render_widget(Paragraph::new(line).style(input), foot);
            if cx.focused {
                let x = (area.x as usize + label.width() + shown.width()).min(area.right().saturating_sub(1) as usize);
                frame.set_cursor_position((x as u16, y));
            }
        } else {
            let text = if self.hints.is_some() {
                "Type the letters by a link to follow it (Shift opens a new window)".to_string()
            } else if let Some(u) = self.sel.and_then(|s| self.target_url(s)) {
                u
            } else if let Some(s) = &self.status {
                s.clone()
            } else {
                let pct = if self.lines.len() <= self.view_h {
                    100
                } else {
                    (self.scroll + self.view_h).min(self.lines.len()) * 100 / self.lines.len().max(1)
                };
                format!("{} links · {pct}%", self.page().map(|p| p.links.len()).unwrap_or(0))
            };
            frame.render_widget(Paragraph::new(Span::styled(format!(" {text}"), t.dim())).style(t.window()), foot);
        }

        // A select box's options.
        if let Some(c) = &self.choice
            && let Some(FieldKind::Select(opts)) =
                self.page().and_then(|p| p.fields.get(c.field)).map(|f| f.kind.clone())
        {
            let w = opts.iter().map(|(_, l)| l.width()).max().unwrap_or(4).clamp(10, 50) as u16 + 4;
            let h = (opts.len() as u16 + 2).min(body.height);
            let r = alouette_ui::chrome::centered(body, w, h);
            frame.render_widget(Clear, r);
            let inner_h = h.saturating_sub(2) as usize;
            let first = c.sel.saturating_sub(inner_h.saturating_sub(1));
            let items: Vec<Line> = opts
                .iter()
                .enumerate()
                .skip(first)
                .take(inner_h)
                .map(|(i, (_, l))| {
                    Line::from(Span::styled(
                        format!(" {l} "),
                        if i == c.sel { t.selection() } else { Style::new().fg(t.dialog_fg).bg(t.dialog_bg) },
                    ))
                })
                .collect();
            let blk = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .style(Style::new().fg(t.dialog_fg).bg(t.dialog_bg));
            frame.render_widget(Paragraph::new(items).block(blk), r);
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if self.prompt.is_some() {
            return self.prompt_key(key, cx);
        }
        if self.choice.is_some() {
            return self.choice_key(key);
        }
        if self.hints.is_some() {
            return self.hint_key(key, cx);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.view_h.saturating_sub(2).max(1) as isize;
        self.status = None;
        match key.code {
            KeyCode::Char('l') if ctrl => self.open_prompt(Some(self.url.clone())),
            KeyCode::Char('o') => self.open_prompt(None),
            KeyCode::Char(':') => self.open_prompt(Some(self.url.clone())),
            KeyCode::Left if alt => {
                self.go_back(cx);
            }
            KeyCode::Right if alt => {
                self.go_forward(cx);
            }
            KeyCode::Enter => match self.sel {
                Some(t) => self.activate(t, shift, cx),
                None => return Handled::No,
            },
            KeyCode::Tab => self.select_next(true),
            KeyCode::BackTab => self.select_next(false),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                self.sel = None;
            }
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Backspace | KeyCode::Char('b') => {
                if !self.go_back(cx) {
                    self.status = Some("Nothing to go back to".into());
                }
            }
            KeyCode::Char('F') => {
                if !self.go_forward(cx) {
                    self.status = Some("Nothing to go forward to".into());
                }
            }
            KeyCode::Char('f') => self.start_hints(),
            KeyCode::Char('r') => {
                let url = if self.error.is_some() { self.loading_url.clone() } else { self.url.clone() };
                self.pending_scroll = Some(self.scroll);
                self.start(WebRequest::get(url), false, cx);
            }
            KeyCode::Char('R') => self.toggle_reader(cx),
            KeyCode::Char('/') => {
                self.prompt =
                    Some(Prompt { kind: PromptKind::Find, label: "Find".into(), value: String::new(), secret: false })
            }
            KeyCode::Char('n') => self.find_next(true),
            KeyCode::Char('N') => self.find_next(false),
            KeyCode::Char('a') => {
                if let Some(store) = cx.store {
                    match store.set_bookmark(&self.url, &self.title, &[MODULE]) {
                        Ok(()) => cx.notify(Notification::success(format!("Bookmarked {}", self.title))),
                        Err(e) => cx.notify(Notification::error(format!("Bookmark failed: {e}"))),
                    }
                }
            }
            KeyCode::Char('B') => self.navigate("web:bookmarks", cx),
            KeyCode::Char('H') => self.navigate("web:history", cx),
            KeyCode::Char('s') => self.save_markdown(cx),
            KeyCode::Char('d') => {
                let u = self.sel.and_then(|t| self.target_url(t)).unwrap_or_else(|| self.url.clone());
                if u.starts_with("http") {
                    self.start(WebRequest::get(&u), true, cx);
                    cx.notify(Notification::info(format!("Downloading {}", short(&u))));
                }
            }
            KeyCode::Char('O') => {
                let u = self
                    .sel
                    .and_then(|t| self.target_url(t))
                    .unwrap_or_else(|| if self.error.is_some() { self.loading_url.clone() } else { self.url.clone() });
                if u.starts_with("http") {
                    open_external(&u, &cx.config.web.external_browser);
                    cx.notify(Notification::info(format!("Opened {} in your browser", short(&u))));
                }
            }
            KeyCode::Char('i') => {
                let text = match &self.info {
                    Some((i, proto)) => {
                        let mut s = format!("{} {proto} via {}", i.badge(), i.via);
                        if let Some(tls) = &i.tls {
                            s.push_str(&format!(", {} {}", tls.version, tls.key_exchange));
                        }
                        let cookies = self.browser.jar.lock().map(|j| j.len()).unwrap_or(0);
                        s.push_str(&format!(", {cookies} cookies this session"));
                        s
                    }
                    None => "No connection yet".into(),
                };
                cx.notify(Notification::info(text));
            }
            KeyCode::Esc if self.sel.is_some() => self.sel = None,
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if let Some(p) = self.prompt.as_mut() {
            p.value.push_str(text.trim_end_matches(['\n', '\r']));
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::Down(b @ (MouseButton::Left | MouseButton::Middle)) => {
                let Some(line) = self.line_at(ev.row) else { return Handled::No };
                let Some(t) = self.target_at(line, ev.column) else { return Handled::No };
                self.sel = Some(t);
                self.activate(t, b == MouseButton::Middle, cx);
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        if let AppEvent::Module { module: MODULE, payload } = ev
            && let Some(l) = payload.downcast_ref::<Loaded>()
            && l.window == self.id
        {
            self.on_loaded(l, cx);
        }
    }
}

impl WebWindow {
    fn open_prompt(&mut self, prefill: Option<String>) {
        let value = prefill.filter(|u| !u.starts_with("web:")).unwrap_or_default();
        self.prompt = Some(Prompt { kind: PromptKind::Address, label: "Go to".into(), value, secret: false });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses() {
        let s = "https://search.example/?q={}";
        assert_eq!(resolve_input("example.org", s), "https://example.org");
        assert_eq!(resolve_input("localhost:8080/x", s), "http://localhost:8080/x");
        assert_eq!(resolve_input("http://a.b/", s), "http://a.b/");
        assert_eq!(resolve_input("rust tui", s), "https://search.example/?q=rust+tui");
        assert_eq!(resolve_input("hello", s), "https://search.example/?q=hello");
        assert_eq!(resolve_input("web:bookmarks", s), "web:bookmarks");
    }

    #[test]
    fn hint_labels_grow() {
        assert_eq!(hint_labels(3), ["a", "s", "d"]);
        let many = hint_labels(30);
        assert_eq!(many.len(), 30);
        assert_eq!(many[0], "aa");
        assert!(many.iter().all(|l| l.len() == 2));
    }
}
