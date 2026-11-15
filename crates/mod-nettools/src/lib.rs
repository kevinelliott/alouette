//! NetTools: the network toolbox, Minuet's Ping and IP Finder grown up. Type a command such
//! as `ping example.org`; it runs in the background and its output streams in.

pub mod command;
pub mod tools;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alouette_core::AppEvent;
use alouette_core::event::ModulePayload;
use alouette_lookup::{Out, Style};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use alouette_ui::{Launcher, Module};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use mod_inquire::input::{InputResult, LineInput};
use mod_inquire::report::{Report, Slot};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style as RStyle;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use url::Url;

use command::Command;
use tools::Msg;

pub const MODULE: &str = "nettools";
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct Event {
    req: u64,
    msg: Msg,
}

pub struct NetToolsModule;

impl Module for NetToolsModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "NetTools"
    }
    fn description(&self) -> &'static str {
        "Ping, traceroute, DNS, WHOIS, RDAP, ports, TLS and mDNS"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["net"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Toolbox", "net:", "Type ping, trace, dns, whois, ports, tls…"),
            Launcher::new("Ping", "net:ping/", "Is it up, and how far away?"),
            Launcher::new("Traceroute", "net:trace/", "The route packets take"),
            Launcher::new("DNS lookup", "net:dns/", "Any record type, over your chosen DNS"),
            Launcher::new("TLS inspector", "net:tls/", "Protocol, key exchange and certificates"),
            Launcher::new("Port check", "net:ports/", "Which services answer"),
            Launcher::new("Local network", "net:mdns", "Devices and services nearby (mDNS)"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        Ok(Box::new(ToolWindow::new(&command::from_url_path(url.path()), cx)))
    }
}

pub struct ToolWindow {
    input: LineInput,
    editing: bool,
    report: Report,
    req: u64,
    running: bool,
    started: Instant,
    title: String,
    url: String,
}

impl ToolWindow {
    pub fn new(initial: &str, cx: &mut Cx) -> Self {
        let mut w = Self {
            input: LineInput::new(initial),
            editing: true,
            report: Report::default(),
            req: 0,
            running: false,
            started: Instant::now(),
            title: "NetTools".into(),
            url: "net:".into(),
        };
        // A complete command (with a target) runs at once; a bare tool name waits for one.
        if Command::parse(initial).is_ok() && (initial.split_whitespace().count() > 1 || initial.trim() == "mdns") {
            w.run(cx);
        } else {
            if !initial.is_empty() && !initial.ends_with(' ') {
                w.input.set(&format!("{} ", initial.trim()));
            }
            w.show_help();
        }
        w
    }

    fn show_help(&mut self) {
        self.report.clear();
        self.report.push_log(Out::new(Style::Heading, "Commands"));
        for (c, what) in command::HELP {
            self.report.push_log(Out::new(Style::Field, format!("{c:<30} {what}")));
        }
    }

    fn run(&mut self, cx: &mut Cx) {
        let cmd = match Command::parse(&self.input.value) {
            Ok(c) => c,
            Err(e) => {
                self.show_help();
                self.report.push_log(Out::new(Style::Bad, e));
                return;
            }
        };
        self.req = NEXT.fetch_add(1, Ordering::Relaxed);
        self.report.clear();
        let streaming =
            matches!(cmd, Command::Ping { .. } | Command::Trace { .. } | Command::Ports { .. } | Command::Mdns);
        self.report.follow = streaming;
        if !streaming {
            self.report.set_slots(vec![Slot::Waiting { source: "NetTools".into(), title: self.input.value.clone() }]);
        }
        self.running = true;
        self.started = Instant::now();
        self.title = self.input.value.trim().to_string();
        self.url = cmd.url();
        self.editing = false;
        let req = self.req;
        let net = cx.net.clone();
        cx.tasks.spawn(MODULE, self.title.clone(), move |ctx| {
            let cmd = cmd.clone();
            let net = net.clone();
            async move {
                let sender = ctx.clone();
                tools::run(cmd, net, ctx, move |msg| {
                    sender.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Event { req, msg }) })
                })
                .await
            }
        });
    }
}

impl Window for ToolWindow {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(self.url.clone())
    }
    fn wants_text(&self) -> bool {
        self.editing
    }
    fn animates(&self) -> bool {
        self.running
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.editing {
            vec![("Enter", "Run"), ("Esc", "Output")]
        } else {
            vec![("/", "New command"), ("r", "Run again"), ("Tab", "Link")]
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 3 {
            return;
        }
        let field = RStyle::new().fg(t.input_fg).bg(t.input_bg);
        let label = " net> ";
        let mut spans = vec![Span::styled(label, t.accent()), Span::styled(self.input.value.clone(), field)];
        if self.running {
            spans.push(Span::styled(format!("   running {}s", self.started.elapsed().as_secs()), t.dim()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(field), Rect { height: 1, ..area });
        if self.editing && cx.focused {
            let x = area.x as usize + label.len() + self.input.cursor_col();
            frame.set_cursor_position(((x as u16).min(area.right().saturating_sub(1)), area.y));
        }
        let body = Rect { y: area.y + 2, height: area.height - 2, ..area };
        self.report.draw(frame, body, t, self.started.elapsed().as_millis());
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if self.editing {
            return match self.input.on_key(key) {
                InputResult::Submit => {
                    self.run(cx);
                    Handled::Yes
                }
                InputResult::Cancel => {
                    self.editing = false;
                    Handled::Yes
                }
                InputResult::Edited => Handled::Yes,
                InputResult::Ignored => Handled::No,
            };
        }
        match key.code {
            KeyCode::Char('/') | KeyCode::Char('i') => {
                self.editing = true;
                self.input.set("");
                Handled::Yes
            }
            KeyCode::Char('e') => {
                self.editing = true;
                Handled::Yes
            }
            KeyCode::Char('r') => {
                self.run(cx);
                Handled::Yes
            }
            _ => self.report.on_key(key, cx),
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, cx: &mut Cx) -> Handled {
        if ev.row == area.y {
            self.editing = true;
            return Handled::Yes;
        }
        let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
        self.report.on_mouse(ev, body, cx)
    }

    fn on_event(&mut self, ev: &AppEvent, _cx: &mut Cx) {
        if let AppEvent::Module { module: MODULE, payload } = ev
            && let Some(e) = payload.downcast_ref::<Event>()
            && e.req == self.req
        {
            match &e.msg {
                Msg::Line(o) => self.report.push_log(o.clone()),
                Msg::Section(s) => self.report.fill(0, s.clone()),
                Msg::Done => self.running = false,
            }
        }
    }
}
