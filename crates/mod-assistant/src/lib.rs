//! The Assistant: press F2 in any window to summarize what it shows, draft a reply, triage a
//! list, explain terminal output, ask a question about it, or ask about your cached mail.
//!
//! It is off until the person picks a provider. Before anything is sent it shows exactly
//! what will go, with personal details redacted, and asks; a module can be allowed for good.
//! A cost meter keeps the month's tokens and estimated dollars, with an optional budget.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alouette_ai::detect::Found;
use alouette_ai::prompt::{Task, build, clip};
use alouette_ai::redact::{Counts, redact};
use alouette_ai::search::Hit;
use alouette_ai::usage::{Usage, dollars, estimate};
use alouette_ai::{Kind, Provider, Reply, Request, Role};
use alouette_core::AppEvent;
use alouette_core::config::Ai;
use alouette_core::event::ModulePayload;
use alouette_core::notify::Notification;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::text::ScrollText;
use alouette_ui::window::{AiContext, Cx, DrawCx, Handled, Window, take_ai_context};
use alouette_ui::{Launcher, Module};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use url::Url;

pub const MODULE: &str = "assistant";
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
enum Msg {
    Found(Vec<Found>),
    Searched(String, Vec<Hit>),
    Answer(Result<Reply, String>),
}

#[derive(Debug, Clone)]
struct Event {
    req: u64,
    msg: Msg,
}

pub struct AssistantModule {
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AssistantModule {
    /// `data_dir` holds the cost meter; `cache_dir` is searched by Ask Alouette.
    pub fn new(data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self { data_dir, cache_dir }
    }
}

impl Module for AssistantModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Assistant"
    }
    fn description(&self) -> &'static str {
        "Summarize, draft, triage and explain with an AI of your choice (off until you pick one)"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["ai"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Assistant", "ai:", "Press F2 in any window to work on what it shows"),
            Launcher::new("Assistant setup", "ai:setup", "Choose a CLI, a local model or an API key"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let ctx = take_ai_context().unwrap_or_default();
        let mut w = AssistantWindow::new(self.data_dir.join("ai-usage.json"), self.cache_dir.clone(), ctx);
        if url.path() == "setup" || !w.provider_ok(cx) {
            w.setup(cx);
        }
        Ok(Box::new(w))
    }
}

/// What the input line is for.
#[derive(Debug, Clone, PartialEq)]
enum Field {
    Ask,
    Draft,
    Search,
    FollowUp,
    Key(Kind),
    Model(Kind),
    Command,
}

impl Field {
    fn label(&self) -> &'static str {
        match self {
            Field::Ask | Field::FollowUp => " ask> ",
            Field::Draft => " how should it read? ",
            Field::Search => " ask Alouette> ",
            Field::Key(_) => " API key> ",
            Field::Model(_) => " model> ",
            Field::Command => " command> ",
        }
    }
}

enum Stage {
    Setup { found: Vec<Found>, sel: usize, detecting: bool },
    Menu { sel: usize },
    Input { field: Field, input: LineInput, back: Box<Stage> },
    Searching,
    Preview { req: Request, task: Task, counts: Counts, consent: bool, view: ScrollText },
    Waiting { task: Task, since: Instant },
    Answer { task: Task, view: ScrollText, text: String },
    Note(String),
}

pub struct AssistantWindow {
    ctx: AiContext,
    stage: Stage,
    req: u64,
    usage_path: PathBuf,
    usage: Usage,
    cache_dir: PathBuf,
    /// The conversation so far, for follow-up questions.
    convo: Vec<(Role, String)>,
    /// Setup in progress: the provider chosen and its key, before the config is written.
    pending: Option<(Kind, Option<String>)>,
    last_cost: Option<f64>,
    born: Instant,
}

fn menu_items(ctx: &AiContext) -> Vec<(char, Task, &'static str)> {
    let mut v = Vec::new();
    if !ctx.text.trim().is_empty() {
        if ctx.list {
            v.push(('t', Task::Triage, "Triage: what needs attention first"));
        }
        if ctx.kind.contains("terminal") {
            v.push(('e', Task::Explain, "Explain this output and suggest a next command"));
        }
        v.push(('s', Task::Summarize, "Summarize"));
        if !ctx.list && !ctx.kind.contains("terminal") {
            v.push(('d', Task::Draft(String::new()), "Draft a reply"));
        }
        v.push(('a', Task::Ask(String::new()), "Ask a question about it"));
    }
    v.push(('/', Task::Search(String::new()), "Ask Alouette: search your cached mail and answer"));
    v
}

impl AssistantWindow {
    fn new(usage_path: PathBuf, cache_dir: PathBuf, ctx: AiContext) -> Self {
        let usage = Usage::load(&usage_path);
        let stage = if ctx.text.trim().is_empty() {
            Stage::Input { field: Field::Search, input: LineInput::default(), back: Box::new(Stage::Menu { sel: 0 }) }
        } else {
            Stage::Menu { sel: 0 }
        };
        Self {
            ctx,
            stage,
            req: 0,
            usage_path,
            usage,
            cache_dir,
            convo: Vec::new(),
            pending: None,
            last_cost: None,
            born: Instant::now(),
        }
    }

    fn key_for(kind: Kind, cx: &Cx) -> Option<String> {
        if !kind.needs_key() {
            return None;
        }
        cx.secrets.get(&kind.secret_key()).ok().flatten()
    }

    fn provider(&self, cfg: &Ai, cx: &Cx) -> Result<Provider, String> {
        if !cfg.enabled {
            return Err("the Assistant is off".into());
        }
        let kind = cfg.provider.as_deref().and_then(Kind::parse).ok_or("no provider chosen yet")?;
        Provider::from_config(cfg, Self::key_for(kind, cx))
    }

    fn provider_ok(&self, cx: &Cx) -> bool {
        self.provider(&cx.config.ai, cx).is_ok()
    }

    fn next_req(&mut self) -> u64 {
        self.req = NEXT.fetch_add(1, Ordering::Relaxed);
        self.req
    }

    fn send_event(ctx: &alouette_core::task::TaskCtx, req: u64, msg: Msg) {
        ctx.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Event { req, msg }) });
    }

    fn setup(&mut self, cx: &mut Cx) {
        self.stage = Stage::Setup { found: Vec::new(), sel: 0, detecting: true };
        let req = self.next_req();
        let net = cx.net.clone();
        let base = cx.config.ai.base_url.clone().filter(|_| cx.config.ai.provider.as_deref() == Some("ollama"));
        cx.tasks.spawn(MODULE, "Looking for AI providers", move |ctx| {
            let net = net.clone();
            let base = base.clone();
            async move {
                let found = alouette_ai::detect::detect(&net, base.as_deref()).await;
                Self::send_event(&ctx, req, Msg::Found(found));
                Ok(())
            }
        });
    }

    /// Write the chosen provider to the config and turn the Assistant on.
    fn choose(&mut self, kind: Kind, model: Option<String>, command: Option<String>, cx: &mut Cx) {
        if let Some((_, Some(key))) = &self.pending
            && let Err(e) = cx.secrets.set(&kind.secret_key(), key)
        {
            self.stage = Stage::Note(format!("Could not store the key: {e}"));
            return;
        }
        self.pending = None;
        let keep_base = cx.config.ai.provider.as_deref() == Some(kind.id());
        cx.edit_config(move |c| {
            c.ai.enabled = true;
            c.ai.provider = Some(kind.id().to_string());
            c.ai.model = model;
            if !keep_base {
                c.ai.base_url = None;
            }
            if command.is_some() {
                c.ai.command = command;
            }
        });
        cx.notify(Notification::success(format!("Assistant on: {}", kind.label())));
        self.stage = if self.ctx.text.trim().is_empty() {
            Stage::Input { field: Field::Search, input: LineInput::default(), back: Box::new(Stage::Menu { sel: 0 }) }
        } else {
            Stage::Menu { sel: 0 }
        };
    }

    fn pick_found(&mut self, f: Found, cx: &mut Cx) {
        let back = Box::new(Stage::Setup { found: Vec::new(), sel: 0, detecting: false });
        match f.kind {
            Kind::ClaudeCli | Kind::CodexCli | Kind::GeminiCli => self.choose(f.kind, None, None, cx),
            Kind::Ollama => {
                let first = f.models.first().cloned().unwrap_or_default();
                self.pending = Some((Kind::Ollama, None));
                self.stage = Stage::Input { field: Field::Model(Kind::Ollama), input: LineInput::new(&first), back };
            }
            Kind::Command => {
                let cur = cx.config.ai.command.clone().unwrap_or_default();
                self.stage = Stage::Input { field: Field::Command, input: LineInput::new(&cur), back };
            }
            k => {
                if f.ready {
                    self.pending = Some((k, None));
                    self.after_key(k, cx);
                } else {
                    self.stage = Stage::Input { field: Field::Key(k), input: LineInput::default(), back };
                }
            }
        }
    }

    fn after_key(&mut self, k: Kind, cx: &mut Cx) {
        if k.default_model().is_some() {
            self.choose(k, None, None, cx);
        } else {
            let cur = cx.config.ai.model.clone().filter(|_| cx.config.ai.provider.as_deref() == Some(k.id()));
            self.stage = Stage::Input {
                field: Field::Model(k),
                input: LineInput::new(&cur.unwrap_or_default()),
                back: Box::new(Stage::Setup { found: Vec::new(), sel: 0, detecting: false }),
            };
        }
    }

    /// Build the request for a task and show the preview, or send at once when allowed.
    fn prepare(&mut self, task: Task, content: Option<String>, cx: &mut Cx) {
        let cfg = cx.config.ai.clone();
        let provider = match self.provider(&cfg, cx) {
            Ok(p) => p,
            Err(e) => {
                self.stage = Stage::Note(format!("{e}. Press p to choose a provider."));
                return;
            }
        };
        let raw = content.unwrap_or_else(|| self.ctx.text.clone());
        let clipped = clip(&raw, cfg.max_chars.max(1000));
        let (text, counts) = if cfg.redact { redact(&clipped) } else { (clipped, Counts::default()) };
        let (title, _) = if cfg.redact { redact(&self.ctx.title) } else { (self.ctx.title.clone(), Counts::default()) };
        let kind = if self.ctx.kind.is_empty() { "text" } else { &self.ctx.kind };
        let mut req = if matches!(task, Task::Ask(_)) && !self.convo.is_empty() {
            // A follow-up: the conversation so far, then the new question.
            let mut r = Request::new(build(&task, kind, &title, "", cfg.max_tokens).system, "");
            r.messages = self.convo.clone();
            if let Task::Ask(q) = &task {
                r.messages.push((Role::User, q.clone()));
            }
            r.max_tokens = cfg.max_tokens;
            r
        } else {
            build(&task, kind, &title, &text, cfg.max_tokens)
        };
        req.max_tokens = cfg.max_tokens.max(256);
        let module = if matches!(task, Task::Search(_)) { "mail" } else { self.ctx.module.as_str() };
        let consent = provider.kind.is_remote() && !module.is_empty() && !cfg.allow.iter().any(|m| m == module);
        if cfg.preview || consent {
            let view = ScrollText::new(preview_lines(&req));
            self.stage = Stage::Preview { req, task, counts, consent, view };
        } else {
            self.send(req, task, cx);
        }
    }

    fn send(&mut self, req: Request, task: Task, cx: &mut Cx) {
        let cfg = cx.config.ai.clone();
        let provider = match self.provider(&cfg, cx) {
            Ok(p) => p,
            Err(e) => {
                self.stage = Stage::Note(e);
                return;
            }
        };
        self.usage = Usage::load(&self.usage_path);
        if self.usage.over_budget(cfg.monthly_budget) {
            self.stage = Stage::Note(format!(
                "This month's budget of {} is used up ({} so far). Raise ai.monthly_budget to go on.",
                dollars(cfg.monthly_budget),
                dollars(self.usage.month().cost)
            ));
            return;
        }
        self.convo = req.messages.clone();
        let id = self.next_req();
        let net = cx.net.clone();
        let title = format!("Assistant: {}", task.label());
        cx.tasks.spawn(MODULE, title, move |ctx| {
            let (net, provider, req) = (net.clone(), provider.clone(), req.clone());
            async move {
                let r = provider.ask(&net, &req).await;
                let failed = r.as_ref().err().cloned();
                Self::send_event(&ctx, id, Msg::Answer(r));
                failed.map_or(Ok(()), Err)
            }
        });
        self.stage = Stage::Waiting { task, since: Instant::now() };
    }

    fn search(&mut self, q: String, cx: &mut Cx) {
        let id = self.next_req();
        let root = self.cache_dir.join("mail");
        cx.tasks.spawn(MODULE, "Searching cached mail", move |ctx| {
            let (root, q) = (root.clone(), q.clone());
            async move {
                let q2 = q.clone();
                let hits = tokio::task::spawn_blocking(move || alouette_ai::search::search(&root, &q2, 8))
                    .await
                    .unwrap_or_default();
                Self::send_event(&ctx, id, Msg::Searched(q, hits));
                Ok(())
            }
        });
        self.stage = Stage::Searching;
    }

    fn answer_lines(text: &str, task: &Task, theme: &alouette_ui::Theme) -> Vec<Line<'static>> {
        text.lines()
            .map(|l| {
                if l.trim_start().starts_with("$ ") {
                    Line::styled(l.to_string(), theme.accent().add_modifier(Modifier::BOLD))
                } else if l.starts_with("To do:") || (matches!(task, Task::Triage) && l.starts_with("now")) {
                    Line::styled(l.to_string(), Style::new().fg(theme.warning))
                } else {
                    Line::raw(l.to_string())
                }
            })
            .collect()
    }

    fn meter(&self, cfg: &Ai) -> String {
        let m = self.usage.month();
        let mut s = format!(
            "this month {} · {} request{}",
            dollars(m.cost),
            m.requests,
            if m.requests == 1 { "" } else { "s" }
        );
        if cfg.monthly_budget > 0.0 {
            s.push_str(&format!(" of {} budget", dollars(cfg.monthly_budget)));
        }
        if m.unpriced > 0 {
            s.push_str(&format!(" · {} unpriced", m.unpriced));
        }
        s
    }

    fn on_input_submit(&mut self, field: Field, value: String, cx: &mut Cx) {
        let v = value.trim().to_string();
        match field {
            Field::Ask | Field::FollowUp if !v.is_empty() => self.prepare(Task::Ask(v), None, cx),
            Field::Draft => self.prepare(Task::Draft(v), None, cx),
            Field::Search if !v.is_empty() => self.search(v, cx),
            Field::Key(k) if !v.is_empty() => {
                self.pending = Some((k, Some(v)));
                self.after_key(k, cx);
            }
            // Empty keeps the key already stored.
            Field::Key(k) if Self::key_for(k, cx).is_some() => {
                self.pending = Some((k, None));
                self.after_key(k, cx);
            }
            Field::Model(k) if !v.is_empty() => {
                let key = self.pending.take().and_then(|p| p.1);
                self.pending = Some((k, key));
                self.choose(k, Some(v), None, cx);
            }
            Field::Command if !v.is_empty() => self.choose(Kind::Command, None, Some(v), cx),
            _ => {}
        }
    }

    fn run_menu(&mut self, i: usize, cx: &mut Cx) {
        let items = menu_items(&self.ctx);
        let Some((_, task, _)) = items.get(i).cloned() else { return };
        self.convo.clear();
        let back = Box::new(Stage::Menu { sel: i });
        match task {
            Task::Ask(_) => self.stage = Stage::Input { field: Field::Ask, input: LineInput::default(), back },
            Task::Draft(_) => self.stage = Stage::Input { field: Field::Draft, input: LineInput::default(), back },
            Task::Search(_) => self.stage = Stage::Input { field: Field::Search, input: LineInput::default(), back },
            t => self.prepare(t, None, cx),
        }
    }
}

/// The request as it will be sent, for the preview.
fn preview_lines(req: &Request) -> Vec<Line<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !req.system.is_empty() {
        lines.push(Line::styled("[system]", dim));
        lines.extend(req.system.lines().map(|l| Line::styled(l.to_string(), dim)));
    }
    for (role, text) in &req.messages {
        lines.push(Line::styled(if *role == Role::User { "[you]" } else { "[assistant]" }, dim));
        lines.extend(text.lines().map(|l| Line::raw(l.to_string())));
    }
    lines
}

fn pct_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl Window for AssistantWindow {
    fn title(&self) -> String {
        if self.ctx.title.is_empty() { "Assistant".into() } else { format!("Assistant: {}", self.ctx.title) }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some("ai:".into())
    }
    fn wants_text(&self) -> bool {
        matches!(self.stage, Stage::Input { .. })
    }
    fn animates(&self) -> bool {
        matches!(self.stage, Stage::Waiting { .. } | Stage::Searching | Stage::Setup { detecting: true, .. })
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.stage {
            Stage::Setup { .. } => vec![("Enter", "Use this"), ("Esc", "Back")],
            Stage::Menu { .. } => vec![("Enter", "Run"), ("p", "Provider")],
            Stage::Input { .. } => vec![("Enter", "Go"), ("Esc", "Back")],
            Stage::Preview { consent: true, .. } => {
                vec![("y", "Send"), ("a", "Always for this module"), ("Esc", "Cancel")]
            }
            Stage::Preview { .. } => vec![("Enter", "Send"), ("Esc", "Cancel")],
            Stage::Answer { task: Task::Draft(_), .. } if self.ctx.reply_url.is_some() => {
                vec![("r", "Reply with this"), ("f", "Follow up"), ("Esc", "Menu")]
            }
            Stage::Answer { .. } => vec![("f", "Follow up"), ("Esc", "Menu")],
            _ => vec![("Esc", "Menu")],
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 4 {
            return;
        }
        // A newer F2 press while this window has focus brings its content here.
        if cx.focused
            && alouette_ui::window::ai_context_pending()
            && let Some(c) = take_ai_context()
            && (!c.text.is_empty() || self.ctx.text.is_empty())
        {
            *self = AssistantWindow::new(self.usage_path.clone(), self.cache_dir.clone(), c);
        }
        let cfg = &cx.config.ai;
        let who = if cfg.enabled {
            cfg.provider.clone().map(|p| match &cfg.model {
                Some(m) if !m.is_empty() => format!("{p} · {m}"),
                _ => p,
            })
        } else {
            None
        };
        let head = Line::from(vec![
            Span::styled(" Assistant ", t.accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!("· {} · {}", who.unwrap_or_else(|| "off".into()), self.meter(cfg)), t.dim()),
        ]);
        frame.render_widget(Paragraph::new(head), Rect { height: 1, ..area });
        let about = if self.ctx.text.trim().is_empty() {
            " Nothing from a window: ask about your cached mail".to_string()
        } else {
            format!(
                " On: {} \"{}\" ({} characters{})",
                if self.ctx.kind.is_empty() { "text" } else { &self.ctx.kind },
                self.ctx.title,
                self.ctx.text.chars().count(),
                if self.ctx.module.is_empty() { String::new() } else { format!(", from {}", self.ctx.module) }
            )
        };
        frame.render_widget(Paragraph::new(Line::styled(about, t.dim())), Rect { y: area.y + 1, height: 1, ..area });
        let body = Rect { y: area.y + 3, height: area.height - 3, ..area };
        let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame_no = (cx.now.saturating_duration_since(self.born).as_millis() / 100) as usize;
        match &mut self.stage {
            Stage::Setup { found, sel, detecting } => {
                let mut lines = vec![
                    Line::styled("Choose where the Assistant sends its requests", t.accent()),
                    Line::raw("Nothing is sent anywhere until you choose; choosing turns the Assistant on."),
                    Line::raw("CLIs use their own sign-in. Local models never leave this machine."),
                    Line::raw(""),
                ];
                if *detecting {
                    lines.push(Line::styled(format!("{} Looking…", spin[frame_no % spin.len()]), t.dim()));
                }
                for (i, f) in found.iter().enumerate() {
                    let mark = if f.ready { "●" } else { "○" };
                    let where_ = if f.kind.is_remote() { "sends to the provider" } else { "stays here" };
                    let style = if i == *sel { t.selection() } else { t.window() };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {mark} {:<38}", f.kind.label()), style),
                        Span::styled(format!(" {} · {where_}", f.detail), t.dim()),
                    ]));
                }
                frame.render_widget(Paragraph::new(lines), body);
            }
            Stage::Menu { sel } => {
                let mut lines = vec![];
                for (i, (k, _, what)) in menu_items(&self.ctx).iter().enumerate() {
                    let style = if i == *sel { t.selection() } else { t.window() };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {k} "), t.accent()),
                        Span::styled(format!(" {what} "), style),
                    ]));
                }
                lines.push(Line::raw(""));
                lines.push(Line::styled(" p  Change provider", t.dim()));
                frame.render_widget(Paragraph::new(lines), body);
            }
            Stage::Input { field, input, .. } => {
                let label = field.label();
                let shown = if matches!(field, Field::Key(_)) {
                    "•".repeat(input.value.chars().count())
                } else {
                    input.value.clone()
                };
                let field_style = Style::new().fg(t.input_fg).bg(t.input_bg);
                let mut lines =
                    vec![Line::from(vec![Span::styled(label, t.accent()), Span::styled(shown, field_style)])];
                lines.push(Line::raw(""));
                let help = match field {
                    Field::Search => {
                        "Searches mail kept offline (mail.offline_cache) here, then sends the best matches."
                    }
                    Field::Draft => "Optional: a few words such as \"say yes, but not Friday\". Enter to go.",
                    Field::Key(_) => {
                        "Kept in your system keychain, never in the config file. Empty keeps the stored key."
                    }
                    Field::Model(Kind::Ollama) => "A model you have pulled, such as llama3.2.",
                    Field::Model(_) => "The model name the provider uses.",
                    Field::Command => "A program that reads the prompt on stdin, such as: llm -m mymodel",
                    _ => "Your question about this content.",
                };
                lines.push(Line::styled(help, t.dim()));
                frame.render_widget(Paragraph::new(lines), body);
                if cx.focused {
                    let x = body.x as usize + label.len() + input.cursor_col();
                    frame.set_cursor_position(((x as u16).min(body.right().saturating_sub(1)), body.y));
                }
            }
            Stage::Searching => {
                let l = Line::styled(format!("{} Searching cached mail…", spin[frame_no % spin.len()]), t.dim());
                frame.render_widget(Paragraph::new(l), body);
            }
            Stage::Preview { req, task, counts, consent, view } => {
                let cfg = &cx.config.ai;
                let kind = cfg.provider.as_deref().and_then(Kind::parse);
                let model = cfg.model.clone().or_else(|| kind.and_then(|k| k.default_model()).map(String::from));
                let est = kind.and_then(|k| {
                    estimate(k, model.as_deref().unwrap_or(""), req.estimate_tokens(), cfg.price_in.zip(cfg.price_out))
                });
                let mut top = vec![
                    Line::styled(format!("{}: this is exactly what will be sent", task.label()), t.accent()),
                    Line::raw(format!(
                        "{} characters, about {} tokens{} · redacted: {}",
                        req.chars(),
                        req.estimate_tokens(),
                        match (est, kind.is_some_and(|k| !k.is_remote())) {
                            (_, true) => ", free (stays on this machine)".to_string(),
                            (Some(c), _) => format!(", about {}", dollars(c)),
                            (None, _) => ", cost unknown (set ai.price_in and ai.price_out)".to_string(),
                        },
                        counts.describe()
                    )),
                ];
                if *consent {
                    top.push(Line::styled(
                        format!(
                            "This sends {} content to {}. y sends once; a allows {} from now on.",
                            if self.ctx.module.is_empty() { "mail" } else { &self.ctx.module },
                            cfg.provider.as_deref().unwrap_or("the provider"),
                            if self.ctx.module.is_empty() { "mail" } else { &self.ctx.module },
                        ),
                        Style::new().fg(t.warning),
                    ));
                }
                let n = top.len() as u16 + 1;
                frame.render_widget(Paragraph::new(top), Rect { height: n, ..body });
                view.draw(frame, Rect { y: body.y + n, height: body.height.saturating_sub(n), ..body }, t);
            }
            Stage::Waiting { task, since } => {
                let l = Line::styled(
                    format!("{} {}… {}s", spin[frame_no % spin.len()], task.label(), since.elapsed().as_secs()),
                    t.dim(),
                );
                frame.render_widget(Paragraph::new(l), body);
            }
            Stage::Answer { view, .. } => {
                let foot = self
                    .last_cost
                    .map(|c| format!("this answer: {}", dollars(c)))
                    .unwrap_or_else(|| "this answer: cost unknown".into());
                let h = body.height.saturating_sub(1);
                view.draw(frame, Rect { height: h, ..body }, t);
                frame.render_widget(
                    Paragraph::new(Line::styled(foot, t.dim())),
                    Rect { y: body.y + h, height: 1, ..body },
                );
            }
            Stage::Note(s) => {
                frame.render_widget(
                    Paragraph::new(s.clone()).wrap(ratatui::widgets::Wrap { trim: false }).style(t.window()),
                    body,
                );
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return Handled::No;
        }
        let stage = std::mem::replace(&mut self.stage, Stage::Note(String::new()));
        match stage {
            Stage::Setup { found, mut sel, detecting } => match key.code {
                KeyCode::Up => {
                    sel = sel.saturating_sub(1);
                    self.stage = Stage::Setup { found, sel, detecting };
                }
                KeyCode::Down => {
                    sel = (sel + 1).min(found.len().saturating_sub(1));
                    self.stage = Stage::Setup { found, sel, detecting };
                }
                KeyCode::Enter => match found.get(sel).cloned() {
                    Some(f) => self.pick_found(f, cx),
                    None => self.stage = Stage::Setup { found, sel, detecting },
                },
                KeyCode::Esc => {
                    self.stage = if self.provider_ok(cx) {
                        Stage::Menu { sel: 0 }
                    } else {
                        Stage::Setup { found, sel, detecting }
                    };
                    if !self.provider_ok(cx) {
                        return Handled::No;
                    }
                }
                _ => {
                    self.stage = Stage::Setup { found, sel, detecting };
                    return Handled::No;
                }
            },
            Stage::Menu { mut sel } => {
                let items = menu_items(&self.ctx);
                match key.code {
                    KeyCode::Up => {
                        sel = sel.saturating_sub(1);
                        self.stage = Stage::Menu { sel };
                    }
                    KeyCode::Down => {
                        sel = (sel + 1).min(items.len().saturating_sub(1));
                        self.stage = Stage::Menu { sel };
                    }
                    KeyCode::Enter => {
                        self.stage = Stage::Menu { sel };
                        self.run_menu(sel, cx);
                    }
                    KeyCode::Char('p') => self.setup(cx),
                    KeyCode::Char(c) => {
                        self.stage = Stage::Menu { sel };
                        match items.iter().position(|(k, ..)| *k == c) {
                            Some(i) => self.run_menu(i, cx),
                            None => return Handled::No,
                        }
                    }
                    _ => {
                        self.stage = Stage::Menu { sel };
                        return Handled::No;
                    }
                }
            }
            Stage::Input { field, mut input, back } => match input.on_key(key) {
                InputResult::Submit => {
                    let v = input.value.clone();
                    self.stage = Stage::Input { field: field.clone(), input, back };
                    self.on_input_submit(field, v, cx);
                }
                InputResult::Cancel => {
                    self.pending = None;
                    self.stage = match *back {
                        Stage::Setup { .. } => {
                            self.setup(cx);
                            return Handled::Yes;
                        }
                        b => b,
                    };
                }
                InputResult::Edited => self.stage = Stage::Input { field, input, back },
                InputResult::Ignored => {
                    self.stage = Stage::Input { field, input, back };
                    return Handled::No;
                }
            },
            Stage::Preview { req, task, counts, consent, mut view } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter if !consent || key.code == KeyCode::Char('y') => {
                    self.send(req, task, cx)
                }
                KeyCode::Char('a') if consent => {
                    let module = if self.ctx.module.is_empty() || matches!(task, Task::Search(_)) {
                        "mail".to_string()
                    } else {
                        self.ctx.module.clone()
                    };
                    cx.edit_config(move |c| {
                        if !c.ai.allow.contains(&module) {
                            c.ai.allow.push(module);
                        }
                    });
                    self.send(req, task, cx);
                }
                KeyCode::Esc | KeyCode::Char('n') => self.stage = Stage::Menu { sel: 0 },
                _ => {
                    let h = view.on_key(key);
                    self.stage = Stage::Preview { req, task, counts, consent, view };
                    return h;
                }
            },
            Stage::Answer { task, mut view, text } => match key.code {
                KeyCode::Char('r') if matches!(task, Task::Draft(_)) && self.ctx.reply_url.is_some() => {
                    let base = self.ctx.reply_url.clone().unwrap_or_default();
                    let sep = if base.contains('?') { '&' } else { '?' };
                    cx.open_url(format!("{base}{sep}body={}", pct_encode(&text)));
                    self.stage = Stage::Answer { task, view, text };
                }
                KeyCode::Char('f') => {
                    self.convo.push((Role::Assistant, text.clone()));
                    self.stage = Stage::Input {
                        field: Field::FollowUp,
                        input: LineInput::default(),
                        back: Box::new(Stage::Answer { task, view, text }),
                    };
                }
                KeyCode::Esc => self.stage = Stage::Menu { sel: 0 },
                _ => {
                    let h = view.on_key(key);
                    self.stage = Stage::Answer { task, view, text };
                    return h;
                }
            },
            Stage::Waiting { task, since } => {
                if key.code == KeyCode::Esc {
                    // The task runs on; its answer is ignored.
                    self.req = 0;
                    self.stage = Stage::Menu { sel: 0 };
                } else {
                    self.stage = Stage::Waiting { task, since };
                    return Handled::No;
                }
            }
            Stage::Searching => {
                if key.code == KeyCode::Esc {
                    self.req = 0;
                    self.stage = Stage::Menu { sel: 0 };
                } else {
                    self.stage = Stage::Searching;
                    return Handled::No;
                }
            }
            Stage::Note(s) => match key.code {
                KeyCode::Char('p') => self.setup(cx),
                KeyCode::Esc | KeyCode::Enter => {
                    self.stage = if self.provider_ok(cx) { Stage::Menu { sel: 0 } } else { Stage::Note(s) };
                    if !self.provider_ok(cx) {
                        self.setup(cx);
                    }
                }
                _ => {
                    self.stage = Stage::Note(s);
                    return Handled::No;
                }
            },
        }
        Handled::Yes
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if let Stage::Input { input, .. } = &mut self.stage {
            let v = format!("{}{}", input.value, text.replace(['\r', '\n'], " ").trim());
            input.set(&v);
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        match &mut self.stage {
            Stage::Answer { view, .. } | Stage::Preview { view, .. } => return view.on_mouse(ev),
            _ => {}
        }
        Handled::No
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(e) = payload.downcast_ref::<Event>() else { return };
        if e.req != self.req || self.req == 0 {
            return;
        }
        match &e.msg {
            Msg::Found(found) => {
                if let Stage::Setup { detecting, found: f, sel } = &mut self.stage {
                    *f = found.clone();
                    *detecting = false;
                    let current = cx.config.ai.provider.as_deref().and_then(Kind::parse);
                    *sel = found
                        .iter()
                        .position(|x| Some(x.kind) == current)
                        .or_else(|| found.iter().position(|x| x.ready))
                        .unwrap_or(0);
                }
            }
            Msg::Searched(q, hits) => {
                if hits.is_empty() {
                    self.stage = Stage::Note(format!(
                        "Nothing in the cache matches \"{q}\".\n\nAsk Alouette searches mail kept offline. Turn that on \
                         with mail.offline_cache = true; messages you open are kept from then on."
                    ));
                } else {
                    self.prepare(Task::Search(q.clone()), Some(alouette_ai::search::as_content(hits)), cx);
                }
            }
            Msg::Answer(r) => {
                let task = match &self.stage {
                    Stage::Waiting { task, .. } => task.clone(),
                    _ => return,
                };
                match r {
                    Ok(reply) => {
                        let kind = cx.config.ai.provider.as_deref().and_then(Kind::parse).unwrap_or(Kind::Command);
                        self.usage = Usage::load(&self.usage_path);
                        self.last_cost = self.usage.add(kind, reply, cx.config.ai.price_in.zip(cx.config.ai.price_out));
                        self.usage.save();
                        let view = ScrollText::new(Self::answer_lines(&reply.text, &task, cx.theme));
                        self.stage = Stage::Answer { task, view, text: reply.text.clone() };
                    }
                    Err(err) => self.stage = Stage::Note(format!("{} failed: {err}", task.label())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_fit_the_content() {
        let mail = AiContext { kind: "email".into(), text: "hi".into(), module: "mail".into(), ..Default::default() };
        let keys: String = menu_items(&mail).iter().map(|i| i.0).collect();
        assert_eq!(keys, "sda/");
        let term = AiContext { kind: "terminal output".into(), text: "$ ls".into(), ..Default::default() };
        assert_eq!(menu_items(&term).iter().map(|i| i.0).collect::<String>(), "esa/");
        let list = AiContext { kind: "mailbox".into(), text: "a\nb".into(), list: true, ..Default::default() };
        assert_eq!(menu_items(&list).iter().map(|i| i.0).collect::<String>(), "tsa/");
        assert_eq!(menu_items(&AiContext::default()).len(), 1);
        assert_eq!(pct_encode("a b&c"), "a%20b%26c");
    }
}
