//! The two-pane Files window, with Minuet's README viewer.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alouette_core::AppEvent;
use alouette_core::event::ModulePayload;
use alouette_ui::ask::Asker;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use alouette_vfs::{Entry, Fs, human_size};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::Semaphore;

use crate::MODULE;
use crate::open::{self, Target};

static NEXT_WIN: AtomicU64 = AtomicU64::new(1);

/// Messages from background work to one Files window.
#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Connected { pane: usize, fs: Arc<dyn Fs>, path: String },
    Failed { pane: usize, error: String },
    Listed { pane: usize, path: String, entries: Vec<Entry>, select: Option<String> },
    ListFailed { pane: usize, error: String },
    Text { title: String, text: String, readme: bool },
    Done { note: String },
    OpFailed { error: String },
}

enum State {
    Connecting(String),
    Ready,
    Failed(String),
}

struct Pane {
    target: Target,
    fs: Option<Arc<dyn Fs>>,
    state: State,
    path: String,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    marked: BTreeSet<String>,
    loading: bool,
    area: Rect,
}

impl Pane {
    fn new(target: Target) -> Self {
        Self {
            state: State::Connecting(format!("Connecting to {}", target.url)),
            target,
            fs: None,
            path: String::new(),
            entries: vec![],
            selected: 0,
            scroll: 0,
            marked: BTreeSet::new(),
            loading: false,
            area: Rect::default(),
        }
    }

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// The marked entries, or the selected one.
    fn chosen(&self) -> Vec<Entry> {
        if self.marked.is_empty() {
            self.current().cloned().into_iter().collect()
        } else {
            self.entries.iter().filter(|e| self.marked.contains(&e.name)).cloned().collect()
        }
    }
}

enum Prompt {
    Mkdir,
    Rename(String),
    Goto,
}

pub struct FilesWindow {
    id: u64,
    panes: [Pane; 2],
    active: usize,
    asker: Asker,
    typed: String,
    viewer: Option<(String, Vec<String>, usize, bool)>,
    confirm: Option<(String, Vec<Entry>)>,
    prompt: Option<(Prompt, LineInput)>,
    show_hidden: bool,
    readme_viewer: bool,
    transfers: Arc<Semaphore>,
    note: Option<String>,
    /// The name to select in a pane after the next reload (a new or renamed entry).
    want: Option<(usize, String)>,
}

fn send(events: &alouette_core::event::EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Msg { win, kind }) });
}

impl FilesWindow {
    pub fn new(left: Target, right: Target, cx: &mut Cx) -> Self {
        let events = cx.tasks.events();
        let wake_events = events.clone();
        let asker = Asker::new(move || {
            let _ = wake_events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(()) });
        });
        let mut w = Self {
            id: NEXT_WIN.fetch_add(1, Ordering::Relaxed),
            panes: [Pane::new(left), Pane::new(right)],
            active: 1,
            asker,
            typed: String::new(),
            viewer: None,
            confirm: None,
            prompt: None,
            show_hidden: cx.config.files.show_hidden,
            readme_viewer: cx.config.files.readme_viewer,
            transfers: Arc::new(Semaphore::new(cx.config.files.parallel_transfers.clamp(1, 16))),
            note: None,
            want: None,
        };
        w.connect(0, cx);
        w.connect(1, cx);
        w
    }

    fn connect(&mut self, pane: usize, cx: &mut Cx) {
        let target = self.panes[pane].target.clone();
        self.panes[pane].state = State::Connecting(format!("Connecting to {}", target.url));
        let (id, asker) = (self.id, self.asker.clone());
        let events = cx.tasks.events();
        let title = format!("Connect {}", target.url);
        cx.tasks.spawn(MODULE, title, move |_ctx| {
            let (target, asker, events) = (target.clone(), asker.clone(), events.clone());
            async move {
                match open::connect(target, asker).await {
                    Ok((fs, path)) => {
                        send(&events, id, Kind::Connected { pane, fs, path });
                        Ok(())
                    }
                    Err(e) => {
                        send(&events, id, Kind::Failed { pane, error: e.clone() });
                        Err(e)
                    }
                }
            }
        });
    }

    fn list(&mut self, pane: usize, path: String, select: Option<String>, cx: &mut Cx) {
        let Some(fs) = self.panes[pane].fs.clone() else { return };
        self.panes[pane].loading = true;
        let (id, events) = (self.id, cx.tasks.events());
        let readme = self.readme_viewer && pane == self.active;
        tokio::spawn(async move {
            match fs.list(&path).await {
                Ok(mut entries) => {
                    entries.sort_by(|a, b| {
                        b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    });
                    // Minuet showed a folder's README as you arrived; so do we.
                    let readme_file = entries
                        .iter()
                        .find(|e| !e.is_dir && is_readme(&e.name))
                        .map(|e| (e.name.clone(), fs.join(&path, &e.name)));
                    send(&events, id, Kind::Listed { pane, path: path.clone(), entries, select });
                    if readme
                        && let Some((name, p)) = readme_file
                        && let Ok(b) = alouette_vfs::read_head(fs.as_ref(), &p, 64 * 1024).await
                    {
                        send(&events, id, Kind::Text { title: name, text: to_text(&b), readme: true });
                    }
                }
                Err(e) => send(&events, id, Kind::ListFailed { pane, error: e }),
            }
        });
    }

    fn refresh(&mut self, pane: usize, cx: &mut Cx) {
        let path = self.panes[pane].path.clone();
        let sel = match self.want.take() {
            Some((p, name)) if p == pane => Some(name),
            other => {
                self.want = other;
                self.panes[pane].current().map(|e| e.name.clone())
            }
        };
        if !path.is_empty() {
            self.list(pane, path, sel, cx);
        }
    }

    fn visible(&self, pane: usize) -> Vec<usize> {
        self.panes[pane]
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.name.starts_with('.'))
            .map(|(i, _)| i)
            .collect()
    }

    fn enter(&mut self, cx: &mut Cx) {
        let p = &self.panes[self.active];
        let (Some(fs), Some(e)) = (p.fs.clone(), p.current().cloned()) else { return };
        let path = fs.join(&p.path, &e.name);
        if e.is_dir {
            self.viewer = None;
            self.panes[self.active].marked.clear();
            self.list(self.active, path, None, cx);
        } else {
            self.view(fs, path, e.name, cx);
        }
    }

    fn view(&mut self, fs: Arc<dyn Fs>, path: String, name: String, cx: &mut Cx) {
        let (id, events) = (self.id, cx.tasks.events());
        tokio::spawn(async move {
            match alouette_vfs::read_head(fs.as_ref(), &path, 256 * 1024).await {
                Ok(b) => send(&events, id, Kind::Text { title: name, text: to_text(&b), readme: false }),
                Err(e) => send(&events, id, Kind::OpFailed { error: e }),
            }
        });
    }

    fn up(&mut self, cx: &mut Cx) {
        let p = &self.panes[self.active];
        let Some(fs) = p.fs.clone() else { return };
        if let Some(parent) = fs.parent(&p.path) {
            let came_from = alouette_vfs::file_name(&p.path).to_string();
            self.viewer = None;
            self.panes[self.active].marked.clear();
            self.list(self.active, parent, Some(came_from), cx);
        }
    }

    fn copy(&mut self, cx: &mut Cx) {
        let (from, to) = (self.active, 1 - self.active);
        let (Some(src), Some(dst)) = (self.panes[from].fs.clone(), self.panes[to].fs.clone()) else { return };
        let items = self.panes[from].chosen();
        if items.is_empty() {
            return;
        }
        let src_dir = self.panes[from].path.clone();
        let dst_dir = self.panes[to].path.clone();
        let (id, events) = (self.id, cx.tasks.events());
        for e in items {
            let (src, dst, events) = (src.clone(), dst.clone(), events.clone());
            let src_path = src.join(&src_dir, &e.name);
            let dst_dir = dst_dir.clone();
            let sem = self.transfers.clone();
            let title = format!("Copy {} to {}", e.name, dst.label());
            cx.tasks.spawn(MODULE, title, move |ctx| {
                let (src, dst, events, e, src_path, dst_dir, sem) = (
                    src.clone(),
                    dst.clone(),
                    events.clone(),
                    e.clone(),
                    src_path.clone(),
                    dst_dir.clone(),
                    sem.clone(),
                );
                async move {
                    ctx.detail("Waiting for a free transfer slot");
                    let _permit = sem.acquire_owned().await.map_err(|e| e.to_string())?;
                    let name = e.name.clone();
                    let status = |s: alouette_vfs::CopyStatus| {
                        if s.total_bytes > 0 {
                            ctx.progress(s.done_bytes as f32 / s.total_bytes as f32);
                        }
                        let files = if s.files_total > 1 {
                            format!(" · file {} of {}", s.files_done + 1, s.files_total)
                        } else {
                            String::new()
                        };
                        ctx.detail(format!("{} of {}{files}", human_size(s.done_bytes), human_size(s.total_bytes)));
                    };
                    let cancelled = || ctx.is_cancelled();
                    match alouette_vfs::copy(src, src_path, e, dst, dst_dir, &status, &cancelled).await {
                        Ok(()) => {
                            send(&events, id, Kind::Done { note: format!("Copied {name}") });
                            Ok(())
                        }
                        Err(err) => {
                            send(&events, id, Kind::OpFailed { error: err.clone() });
                            Err(err)
                        }
                    }
                }
            });
        }
        self.panes[from].marked.clear();
        self.note = Some("Copying; see Tasks for progress".into());
    }

    fn run_op(
        &mut self,
        cx: &mut Cx,
        title: String,
        op: impl FnOnce(Arc<dyn Fs>) -> alouette_vfs::BoxFut<'static, String> + Send + 'static,
    ) {
        let Some(fs) = self.panes[self.active].fs.clone() else { return };
        let (id, events) = (self.id, cx.tasks.events());
        let once = std::sync::Mutex::new(Some(op));
        cx.tasks.spawn(MODULE, title, move |_ctx| {
            let op = once.lock().unwrap_or_else(|e| e.into_inner()).take();
            let (fs, events) = (fs.clone(), events.clone());
            async move {
                let Some(op) = op else { return Err("run it again from Files".to_string()) };
                match op(fs).await {
                    Ok(note) => {
                        send(&events, id, Kind::Done { note });
                        Ok(())
                    }
                    Err(e) => {
                        send(&events, id, Kind::OpFailed { error: e.clone() });
                        Err(e)
                    }
                }
            }
        });
    }

    fn submit_prompt(&mut self, cx: &mut Cx) {
        let Some((kind, input)) = self.prompt.take() else { return };
        let value = input.value.trim().to_string();
        if value.is_empty() {
            return;
        }
        let dir = self.panes[self.active].path.clone();
        if matches!(kind, Prompt::Mkdir | Prompt::Rename(_)) {
            self.want = Some((self.active, value.clone()));
        }
        match kind {
            Prompt::Mkdir => self.run_op(cx, format!("New folder {value}"), move |fs| {
                Box::pin(async move {
                    let p = fs.join(&dir, &value);
                    fs.mkdir(&p).await?;
                    Ok(format!("Made {value}"))
                })
            }),
            Prompt::Rename(old) => self.run_op(cx, format!("Rename {old}"), move |fs| {
                Box::pin(async move {
                    fs.rename(&fs.join(&dir, &old), &fs.join(&dir, &value)).await?;
                    Ok(format!("Renamed {old} to {value}"))
                })
            }),
            Prompt::Goto => {
                let pane = self.active;
                if value.contains("://") || value.starts_with("files:") {
                    match url::Url::parse(&value) {
                        Ok(u) => {
                            self.panes[pane].target.url = u;
                            self.panes[pane].fs = None;
                            self.connect(pane, cx);
                        }
                        Err(e) => self.note = Some(format!("{value}: {e}")),
                    }
                } else {
                    self.list(pane, value, None, cx);
                }
            }
        }
    }

    fn delete_confirmed(&mut self, cx: &mut Cx) {
        let Some((_, items)) = self.confirm.take() else { return };
        let dir = self.panes[self.active].path.clone();
        let n = items.len();
        self.run_op(cx, format!("Delete {n} item{}", if n == 1 { "" } else { "s" }), move |fs| {
            Box::pin(async move {
                for e in &items {
                    alouette_vfs::delete(fs.as_ref(), &fs.join(&dir, &e.name), e.is_dir && !e.is_link).await?;
                }
                Ok(format!("Deleted {n} item{}", if n == 1 { "" } else { "s" }))
            })
        });
        self.panes[self.active].marked.clear();
    }

    fn move_sel(&mut self, delta: isize) {
        let vis = self.visible(self.active);
        let p = &mut self.panes[self.active];
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == p.selected).unwrap_or(0) as isize;
        let n = (pos + delta).clamp(0, vis.len() as isize - 1) as usize;
        p.selected = vis[n];
    }

    fn draw_pane(&mut self, frame: &mut Frame, area: Rect, i: usize, cx: &DrawCx) {
        let t = cx.theme;
        let active = i == self.active;
        let vis = self.visible(i);
        let p = &mut self.panes[i];
        p.area = area;
        let label = p.fs.as_ref().map(|f| f.label()).unwrap_or_else(|| p.target.url.to_string());
        let title = format!(" {label} ");
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, if active { t.accent() } else { t.dim() }))
            .border_style(Style::new().fg(if active { t.border_active_fg } else { t.border_fg }));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 2 {
            return;
        }
        let path_line = format!("{}{}", p.path, if p.loading { "  …" } else { "" });
        frame.render_widget(Paragraph::new(Span::styled(path_line, t.dim())), Rect { height: 1, ..inner });
        let body = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
        match &p.state {
            State::Connecting(s) => {
                frame.render_widget(Paragraph::new(Span::styled(format!("{s}…"), t.dim())), body);
                return;
            }
            State::Failed(e) => {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled("Could not connect", Style::new().fg(t.error))),
                        Line::from(e.clone()),
                        Line::default(),
                        Line::from(Span::styled("Press g to try again, or o to open another place.", t.dim())),
                    ])
                    .wrap(Wrap { trim: false }),
                    body,
                );
                return;
            }
            State::Ready => {}
        }
        if vis.is_empty() {
            frame.render_widget(Paragraph::new(Span::styled("(empty)", t.dim())), body);
            return;
        }
        let rows = body.height as usize;
        let pos = vis.iter().position(|&x| x == p.selected).unwrap_or(0);
        if pos < p.scroll {
            p.scroll = pos;
        } else if pos >= p.scroll + rows {
            p.scroll = pos + 1 - rows;
        }
        let w = body.width as usize;
        let size_w = 9;
        let date_w = if w > 50 { 17 } else { 0 };
        let name_w = w.saturating_sub(size_w + date_w + 3);
        let lines: Vec<Line> = vis
            .iter()
            .enumerate()
            .skip(p.scroll)
            .take(rows)
            .map(|(row, &idx)| {
                let e = &p.entries[idx];
                let mark = if p.marked.contains(&e.name) { "•" } else { " " };
                let mut name = e.name.clone();
                if e.is_dir {
                    name.push('/');
                }
                if e.is_link {
                    name.push('@');
                }
                let name: String = name.chars().take(name_w.saturating_sub(1)).collect();
                let size = if e.is_dir { String::new() } else { e.size.map(human_size).unwrap_or_default() };
                let date = if date_w > 0 { e.modified.map(fmt_time).unwrap_or_default() } else { String::new() };
                let mut style =
                    if e.is_dir { Style::new().fg(t.link).add_modifier(Modifier::BOLD) } else { Style::new() };
                if p.marked.contains(&e.name) {
                    style = style.fg(t.warning);
                }
                if row == pos && active {
                    style = t.selection();
                } else if row == pos {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                let text = if date_w > 0 {
                    format!("{mark}{name:<name_w$} {size:>size_w$} {date:>date_w$}")
                } else {
                    format!("{mark}{name:<name_w$} {size:>size_w$}")
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
    }
}

fn is_readme(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "readme" || n.starts_with("readme.") || n == "00index.txt" || n == ".message" || n == "welcome.msg"
}

/// Bytes to printable text; binary files say so.
fn to_text(b: &[u8]) -> String {
    let sample = &b[..b.len().min(4096)];
    let nul = sample.iter().filter(|&&c| c == 0).count();
    if nul > 0 {
        return format!("(binary file, {} shown is the start)", human_size(b.len() as u64));
    }
    String::from_utf8_lossy(b).replace('\t', "    ").replace('\r', "")
}

fn fmt_time(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|d| d.format(&time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]")).ok())
        .unwrap_or_default()
}

impl Window for FilesWindow {
    fn title(&self) -> String {
        let p = &self.panes[self.active];
        let label = p.fs.as_ref().map(|f| f.label()).unwrap_or_else(|| "Files".into());
        format!("Files: {label}")
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        let p = &self.panes[1];
        Some(if p.path.is_empty() { p.target.url.to_string() } else { open::url_for(&p.target.url, &p.path) })
    }
    fn badge(&self) -> Option<String> {
        self.panes[self.active].fs.as_ref().and_then(|f| f.badge())
    }
    fn wants_text(&self) -> bool {
        self.prompt.is_some() || self.asker.question().is_some()
    }
    fn animates(&self) -> bool {
        self.panes.iter().any(|p| matches!(p.state, State::Connecting(_)) || p.loading)
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.viewer.is_some() {
            return vec![("Esc", "Close viewer"), ("↑↓", "Scroll")];
        }
        vec![
            ("Tab", "Other side"),
            ("c", "Copy"),
            ("Space", "Mark"),
            ("d", "Delete"),
            ("n", "New folder"),
            ("r", "Rename"),
            ("o", "Open"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 5 {
            return;
        }
        let footer_h = 1;
        let viewer_h = if self.viewer.is_some() { (area.height / 2).max(5) } else { 0 };
        let panes_h = area.height.saturating_sub(viewer_h + footer_h);
        let half = area.width / 2;
        self.draw_pane(frame, Rect { x: area.x, y: area.y, width: half, height: panes_h }, 0, cx);
        self.draw_pane(frame, Rect { x: area.x + half, y: area.y, width: area.width - half, height: panes_h }, 1, cx);
        if let Some((title, lines, scroll, readme)) = &self.viewer {
            let r = Rect { x: area.x, y: area.y + panes_h, width: area.width, height: viewer_h };
            let label = if *readme { format!(" {title} · README ") } else { format!(" {title} ") };
            let block = Block::default().borders(Borders::ALL).title(Span::styled(label, t.accent()));
            let inner = block.inner(r);
            frame.render_widget(block, r);
            let shown: Vec<Line> =
                lines.iter().skip(*scroll).take(inner.height as usize).map(|l| Line::from(l.clone())).collect();
            frame.render_widget(Paragraph::new(shown), inner);
        }
        let foot = Rect { x: area.x, y: area.bottom() - 1, width: area.width, height: 1 };
        if let Some((kind, input)) = &self.prompt {
            let label = match kind {
                Prompt::Mkdir => " New folder: ".to_string(),
                Prompt::Rename(old) => format!(" Rename {old} to: "),
                Prompt::Goto => " Open (path or URL): ".to_string(),
            };
            let field = Style::new().fg(t.input_fg).bg(t.input_bg);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(label.clone(), t.accent()),
                    Span::styled(input.value.clone(), field),
                ])),
                foot,
            );
            let x = foot.x + label.chars().count() as u16 + input.cursor_col() as u16;
            frame.set_cursor_position((x.min(foot.right().saturating_sub(1)), foot.y));
        } else if let Some((what, _)) = &self.confirm {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" Delete {what}? "), Style::new().fg(t.error).add_modifier(Modifier::BOLD)),
                    Span::styled("y", t.accent()),
                    Span::raw(" yes   "),
                    Span::styled("n", t.accent()),
                    Span::raw(" no"),
                ])),
                foot,
            );
        } else if let Some(n) = &self.note {
            frame.render_widget(Paragraph::new(Span::styled(format!(" {n}"), t.dim())), foot);
        }
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        if let Some((_, input)) = &mut self.prompt {
            match input.on_key(key) {
                InputResult::Submit => self.submit_prompt(cx),
                InputResult::Cancel => self.prompt = None,
                _ => {}
            }
            return Handled::Yes;
        }
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') => self.delete_confirmed(cx),
                _ => self.confirm = None,
            }
            return Handled::Yes;
        }
        // A README shown on arrival only takes Esc; the list keeps the rest.
        if let Some((_, _, _, true)) = &self.viewer
            && key.code == KeyCode::Esc
        {
            self.viewer = None;
            return Handled::Yes;
        }
        if let Some((_, lines, scroll, false)) = &mut self.viewer {
            let max = lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.viewer = None;
                    return Handled::Yes;
                }
                KeyCode::Char('j') if key.modifiers.is_empty() => {
                    *scroll = (*scroll + 1).min(max);
                    return Handled::Yes;
                }
                KeyCode::Char('k') if key.modifiers.is_empty() => {
                    *scroll = scroll.saturating_sub(1);
                    return Handled::Yes;
                }
                KeyCode::PageDown | KeyCode::Char(' ') if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    *scroll = (*scroll + 10).min(max);
                    return Handled::Yes;
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    return Handled::Yes;
                }
                _ => {}
            }
        }
        self.note = None;
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.active = 1 - self.active,
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => self.move_sel(-100_000),
            KeyCode::End => self.move_sel(100_000),
            KeyCode::Enter | KeyCode::Right => self.enter(cx),
            KeyCode::Backspace | KeyCode::Left => self.up(cx),
            KeyCode::Char(' ') | KeyCode::Insert => {
                let p = &mut self.panes[self.active];
                if let Some(name) = p.current().map(|e| e.name.clone())
                    && !p.marked.remove(&name)
                {
                    p.marked.insert(name);
                }
                self.move_sel(1);
            }
            KeyCode::Char('c') | KeyCode::F(5) => self.copy(cx),
            KeyCode::Char('n') | KeyCode::F(7) => self.prompt = Some((Prompt::Mkdir, LineInput::default())),
            KeyCode::Char('r') => {
                if let Some(e) = self.panes[self.active].current() {
                    let name = e.name.clone();
                    self.prompt = Some((Prompt::Rename(name.clone()), LineInput::new(&name)));
                }
            }
            KeyCode::Char('o') => self.prompt = Some((Prompt::Goto, LineInput::default())),
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::F(8) => {
                let items = self.panes[self.active].chosen();
                if !items.is_empty() {
                    let what = if items.len() == 1 { items[0].name.clone() } else { format!("{} items", items.len()) };
                    self.confirm = Some((what, items));
                }
            }
            KeyCode::Char('v') => {
                let p = &self.panes[self.active];
                if let (Some(fs), Some(e)) = (p.fs.clone(), p.current().cloned())
                    && !e.is_dir
                {
                    let path = fs.join(&p.path, &e.name);
                    self.view(fs, path, e.name, cx);
                }
            }
            KeyCode::Char('h') => self.show_hidden = !self.show_hidden,
            KeyCode::Char('g') => {
                let a = self.active;
                if self.panes[a].fs.is_some() {
                    self.refresh(a, cx);
                } else {
                    self.connect(a, cx);
                }
            }
            KeyCode::Esc if self.viewer.is_some() => self.viewer = None,
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.asker.question().is_some() {
            self.typed.push_str(text.lines().next().unwrap_or(""));
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        for i in 0..2 {
            let a = self.panes[i].area;
            if ev.column < a.x || ev.column >= a.right() || ev.row < a.y || ev.row >= a.bottom() {
                continue;
            }
            match ev.kind {
                MouseEventKind::Down(_) => {
                    let body_y = a.y + 2;
                    let was = (self.active, self.panes[i].selected);
                    self.active = i;
                    if ev.row >= body_y {
                        let row = self.panes[i].scroll + usize::from(ev.row - body_y);
                        let vis = self.visible(i);
                        if let Some(&idx) = vis.get(row) {
                            self.panes[i].selected = idx;
                            if was == (i, idx) {
                                self.enter(cx);
                            }
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.active = i;
                    self.move_sel(-3);
                }
                MouseEventKind::ScrollDown => {
                    self.active = i;
                    self.move_sel(3);
                }
                _ => {}
            }
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match m.kind.clone() {
            Kind::Connected { pane, fs, path } => {
                self.panes[pane].fs = Some(fs);
                self.panes[pane].state = State::Ready;
                self.list(pane, path, None, cx);
            }
            Kind::Failed { pane, error } => self.panes[pane].state = State::Failed(error),
            Kind::Listed { pane, path, entries, select } => {
                let p = &mut self.panes[pane];
                p.loading = false;
                // When the selected entry went away (deleted), stay near where it was.
                let keep = if p.path == path { p.selected.min(entries.len().saturating_sub(1)) } else { 0 };
                p.path = path;
                p.entries = entries;
                p.selected = select.and_then(|s| p.entries.iter().position(|e| e.name == s)).unwrap_or(keep);
                p.marked.retain(|m| p.entries.iter().any(|e| &e.name == m));
                if !self.show_hidden && p.entries.get(p.selected).is_some_and(|e| e.name.starts_with('.')) {
                    let vis = self.visible(pane);
                    self.panes[pane].selected = vis.first().copied().unwrap_or(0);
                }
            }
            Kind::ListFailed { pane, error } => {
                self.panes[pane].loading = false;
                self.note = Some(error);
            }
            Kind::Text { title, text, readme } => {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                self.viewer = Some((title, lines, 0, readme));
            }
            Kind::Done { note } => {
                self.note = Some(note);
                self.refresh(0, cx);
                self.refresh(1, cx);
            }
            Kind::OpFailed { error } => {
                self.note = Some(error.clone());
                cx.notify(alouette_core::Notification::error(error));
                self.refresh(0, cx);
                self.refresh(1, cx);
            }
        }
    }
}
