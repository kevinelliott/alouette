//! The demo module shows how a module plugs into the shell: it registers the `demo:`
//! scheme, adds launchers to the Modules menu and runs background tasks the Tasks window
//! can cancel and retry. Real modules (Burrow, Mail, ...) follow the same shape.

use std::time::Duration;

use alouette_core::{AppEvent, Notification, TaskState};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use alouette_ui::{Launcher, Module};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{LineGauge, Paragraph};
use url::Url;

pub struct DemoModule;

impl Module for DemoModule {
    fn id(&self) -> &'static str {
        "demo"
    }
    fn name(&self) -> &'static str {
        "Demo"
    }
    fn description(&self) -> &'static str {
        "Background jobs and links, to try the shell"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["demo"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Background jobs", "demo:jobs", "Start fake downloads and watch them in Tasks"),
            Launcher::new("Links", "demo:hello", "A page of links that open other windows"),
        ]
    }
    fn open(&mut self, url: &Url, _cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        match url.path().trim_matches('/') {
            "jobs" | "" => Ok(Box::new(JobsWindow::default())),
            "hello" => Ok(Box::new(LinksWindow::default())),
            other => Err(format!("the demo module has no page called {other}")),
        }
    }
}

// ---------------------------------------------------------------------------- jobs

#[derive(Default)]
pub struct JobsWindow {
    started: usize,
    /// Failed tasks already announced with a toast.
    announced: std::collections::HashSet<alouette_core::TaskId>,
}

const JOBS: &[(char, &str, &str)] = &[
    ('d', "Download", "a 6 second fake download with progress"),
    ('f', "Failing job", "fails half way, so you can retry it"),
    ('l', "Long job", "runs until cancelled, with no known end"),
    ('b', "Burst", "starts five short downloads at once"),
];

impl JobsWindow {
    fn start(&mut self, which: char, cx: &mut Cx) -> bool {
        let n = self.started + 1;
        match which {
            'd' => {
                cx.tasks.spawn("demo", format!("Download file{n}.zip"), |ctx| async move {
                    for i in 0..=60 {
                        ctx.progress(i as f32 / 60.0);
                        ctx.detail(format!("{} of 600 KB", i * 10));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok(())
                });
            }
            'f' => {
                cx.tasks.spawn("demo", format!("Flaky transfer {n}"), |ctx| async move {
                    for i in 0..=20 {
                        ctx.progress(i as f32 / 40.0);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err("connection reset by peer (on purpose)".into())
                });
            }
            'l' => {
                cx.tasks.spawn("demo", format!("Watch feed {n}"), |ctx| async move {
                    let mut polls = 0u32;
                    loop {
                        polls += 1;
                        ctx.detail(format!("polled {polls} times"));
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                });
            }
            'b' => {
                for k in 0..5u64 {
                    cx.tasks.spawn("demo", format!("Mirror chunk {}", k + 1), move |ctx| async move {
                        let steps = 10 + k * 5;
                        for i in 0..=steps {
                            ctx.progress(i as f32 / steps as f32);
                            tokio::time::sleep(Duration::from_millis(120)).await;
                        }
                        Ok(())
                    });
                }
            }
            _ => return false,
        }
        self.started = n;
        true
    }
}

impl Window for JobsWindow {
    fn title(&self) -> String {
        "Demo: background jobs".into()
    }
    fn module(&self) -> &'static str {
        "demo"
    }
    fn url(&self) -> Option<String> {
        Some("demo:jobs".into())
    }
    fn animates(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("d", "Download"), ("f", "Fail"), ("l", "Long"), ("b", "Burst"), ("t", "Tasks")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let mut lines = vec![
            Line::styled("Start a job. It runs in the background while you keep working.", t.window()),
            Line::raw(""),
        ];
        for (k, name, what) in JOBS {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k}  "), t.accent().add_modifier(Modifier::BOLD)),
                Span::styled(format!("{name:<12}"), t.window().add_modifier(Modifier::BOLD)),
                Span::styled(*what, t.dim()),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("  t  opens the Tasks window to cancel (c) or retry (r) jobs.", t.dim()));
        lines.push(Line::raw(""));
        let head = lines.len() as u16;
        let [top, rest] = Layout::vertical([Constraint::Length(head), Constraint::Min(0)]).areas(area);
        frame.render_widget(Paragraph::new(lines), top);

        let tasks: Vec<_> = cx.tasks.list().into_iter().filter(|i| i.module == "demo").rev().collect();
        for (row, info) in tasks.iter().take(rest.height as usize).enumerate() {
            let r = Rect { x: rest.x + 2, y: rest.y + row as u16, width: rest.width.saturating_sub(4), height: 1 };
            let [name, gauge] = Layout::horizontal([Constraint::Length(24), Constraint::Min(10)]).areas(r);
            frame.render_widget(Paragraph::new(info.title.clone()).style(t.window()), name);
            let (label, style) = match &info.state {
                TaskState::Running => (String::new(), Style::new().fg(t.accent)),
                TaskState::Done => ("done ".into(), Style::new().fg(t.success)),
                TaskState::Failed(_) => ("failed ".into(), Style::new().fg(t.error)),
                TaskState::Cancelled => ("cancelled ".into(), Style::new().fg(t.warning)),
            };
            match info.progress {
                Some(p) => frame.render_widget(
                    LineGauge::default()
                        .ratio(p as f64)
                        .label(Span::styled(format!("{label}{:>3}%", (p * 100.0) as u32), style))
                        .filled_style(style)
                        .unfilled_style(t.dim()),
                    gauge,
                ),
                None => {
                    frame.render_widget(Paragraph::new(Span::styled(format!("{label}{}", info.detail), style)), gauge)
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        match key.code {
            KeyCode::Char('t') => {
                cx.open_url("about:tasks");
                Handled::Yes
            }
            KeyCode::Char(c) => self.start(c, cx).into(),
            _ => Handled::No,
        }
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        if let AppEvent::TaskUpdated(id) = ev
            && let Some(info) = cx.tasks.get(*id)
            && info.module == "demo"
            && let TaskState::Failed(e) = &info.state
            && self.announced.insert(*id)
        {
            cx.notify(Notification::warning(format!("{} failed: {e}", info.title)));
        }
    }
}

// ---------------------------------------------------------------------------- links

const LINKS: &[(&str, &str)] = &[
    ("about:welcome", "The welcome page"),
    ("about:tasks", "Background tasks"),
    ("about:minuet", "A short history of Minuet"),
    ("demo:jobs", "Start some background jobs"),
    ("gopher://gopher.floodgap.com/", "Floodgap Gopher, in Burrow"),
    ("gemini://geminiprotocol.net/", "Project Gemini, in Burrow"),
    ("finger://graph.no/oslo", "The Oslo forecast over Finger"),
];

#[derive(Default)]
pub struct LinksWindow {
    sel: usize,
}

impl Window for LinksWindow {
    fn title(&self) -> String {
        "Demo: links".into()
    }
    fn module(&self) -> &'static str {
        "demo"
    }
    fn url(&self) -> Option<String> {
        Some("demo:hello".into())
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑↓", "Choose"), ("Enter", "Open")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let mut lines = vec![
            Line::styled("Every link goes through the URL router, which picks the module.", t.window()),
            Line::raw(""),
        ];
        for (i, (url, what)) in LINKS.iter().enumerate() {
            let base = if i == self.sel { t.selection() } else { t.link() };
            lines.push(Line::from(vec![
                Span::styled(format!(" {url:<34}"), base),
                Span::styled(format!(" {what}"), if i == self.sel { t.selection() } else { t.dim() }),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.sel = (self.sel + 1).min(LINKS.len() - 1),
            KeyCode::Enter => cx.open_url(LINKS[self.sel].0),
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, cx: &mut Cx) -> Handled {
        if let MouseEventKind::Down(MouseButton::Left) = ev.kind {
            let row = ev.row.saturating_sub(area.y) as usize;
            if let Some(i) = row.checked_sub(2).filter(|i| *i < LINKS.len()) {
                self.sel = i;
                cx.open_url(LINKS[i].0);
                return Handled::Yes;
            }
        }
        Handled::No
    }
}
