//! The Inquire window: type anything, every relevant source answers at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alouette_core::AppEvent;
use alouette_core::config::Inquire;
use alouette_core::event::ModulePayload;
use alouette_lookup::{Query, QueryKind, Section, reference, sources};
use alouette_net::Net;
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::input::{InputResult, LineInput};
use crate::report::{Report, Slot};

pub const MODULE: &str = "inquire";
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A source Inquire can ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Dns,
    ReverseDns,
    RdapDomain,
    RdapIp,
    RdapAsn,
    Whois,
    Asn,
    Ct,
    Tls,
    WebFinger,
    Finger,
    Wkd,
    Mx,
    Dict,
    Veronica,
    GeminiSearch,
    Wikipedia,
    Wayback,
    Arxiv,
    Crossref,
    Searx,
    Sru,
    OpenSearch(usize),
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Dns | Source::ReverseDns | Source::Mx => "DNS",
            Source::RdapDomain | Source::RdapIp | Source::RdapAsn => "RDAP",
            Source::Whois => "WHOIS",
            Source::Asn => "Team Cymru",
            Source::Ct => "crt.sh",
            Source::Tls => "TLS",
            Source::WebFinger => "WebFinger",
            Source::Finger => "Finger",
            Source::Wkd => "OpenPGP WKD",
            Source::Dict => "DICT",
            Source::Veronica => "Veronica-2",
            Source::GeminiSearch => "Kennedy",
            Source::Wikipedia => "Wikipedia",
            Source::Wayback => "Wayback",
            Source::Arxiv => "arXiv",
            Source::Crossref => "Crossref",
            Source::Searx => "SearXNG",
            Source::Sru => "Library",
            Source::OpenSearch(_) => "OpenSearch",
        }
    }

    /// The sources worth asking for a query, most useful first.
    pub fn for_query(q: &Query, cfg: &Inquire) -> Vec<Source> {
        use Source::*;
        let mut words = vec![];
        if let QueryKind::Words(w) = &q.kind {
            if alouette_lookup::reference::doi(w).is_some() {
                return vec![Crossref];
            }
            if w.to_ascii_lowercase().starts_with("arxiv:") {
                return vec![Arxiv];
            }
            if !cfg.wikipedia.is_empty() {
                words.push(Wikipedia);
            }
            if !cfg.searxng.is_empty() {
                words.push(Searx);
            }
            words.extend((0..cfg.opensearch.len()).map(OpenSearch));
            if !w.contains(' ') {
                words.push(Dict);
            }
            if cfg.scholarly {
                words.extend([Arxiv, Crossref]);
            }
            if !cfg.sru.is_empty() {
                words.push(Sru);
            }
            words.extend([Veronica, GeminiSearch]);
            if w.contains(' ') {
                words.push(Dict);
            }
        }
        match &q.kind {
            QueryKind::Domain(_) => vec![Dns, RdapDomain, Tls, Ct, Whois, Wayback],
            QueryKind::Ip(_) => vec![ReverseDns, Asn, RdapIp, Whois],
            QueryKind::Asn(_) => vec![Asn, RdapAsn],
            QueryKind::Address { .. } => vec![WebFinger, Finger, Wkd, Mx],
            QueryKind::Url(u) if u.scheme() == "https" || u.scheme() == "http" => vec![Tls, Dns, Wayback],
            QueryKind::Url(u) if u.scheme() == "gemini" => vec![Tls, Dns],
            QueryKind::Url(_) => vec![Dns],
            QueryKind::Words(_) => words,
        }
    }

    pub async fn run(self, net: &Net, q: &Query, cfg: &Inquire) -> Section {
        let host = q.host().unwrap_or_default();
        match (self, &q.kind) {
            (Source::Dns, _) => sources::dns_report(net, &host).await,
            (Source::ReverseDns, QueryKind::Ip(ip)) => sources::reverse_dns(net, *ip).await,
            (Source::RdapDomain, _) => sources::rdap(net, "domain", &host).await,
            (Source::RdapIp, QueryKind::Ip(ip)) => sources::rdap(net, "ip", &ip.to_string()).await,
            (Source::RdapAsn, QueryKind::Asn(n)) => sources::rdap(net, "autnum", &n.to_string()).await,
            (Source::Whois, QueryKind::Ip(ip)) => sources::whois(net, &ip.to_string()).await,
            (Source::Whois, _) => sources::whois(net, &host).await,
            (Source::Asn, QueryKind::Ip(ip)) => sources::asn(net, &ip.to_string()).await,
            (Source::Asn, QueryKind::Asn(n)) => sources::asn(net, &format!("AS{n}")).await,
            (Source::Ct, _) => sources::certificate_transparency(net, &host).await,
            (Source::Tls, QueryKind::Url(u)) => {
                let port = u.port().unwrap_or(if u.scheme() == "gemini" { 1965 } else { 443 });
                alouette_lookup::tls::inspect(net, &host, port).await
            }
            (Source::Tls, _) => alouette_lookup::tls::inspect(net, &host, 443).await,
            (Source::WebFinger, QueryKind::Address { user, host }) => sources::webfinger(net, user, host).await,
            (Source::Finger, QueryKind::Address { user, host }) => sources::finger(net, user, host).await,
            (Source::Wkd, QueryKind::Address { user, host }) => sources::openpgp_wkd(net, user, host).await,
            (Source::Mx, QueryKind::Address { host, .. }) => sources::dns_lookup(net, host, "MX").await,
            (Source::Dict, QueryKind::Words(w)) => sources::dict(net, w).await,
            (Source::Veronica, QueryKind::Words(w)) => sources::veronica(net, w).await,
            (Source::GeminiSearch, QueryKind::Words(w)) => sources::gemini_search(net, w).await,
            (Source::Wikipedia, QueryKind::Words(w)) => reference::wikipedia(net, &cfg.wikipedia, w).await,
            (Source::Wayback, QueryKind::Url(u)) => reference::wayback(net, u.as_str()).await,
            (Source::Wayback, _) => reference::wayback(net, &host).await,
            (Source::Arxiv, QueryKind::Words(w)) => reference::arxiv(net, w).await,
            (Source::Crossref, QueryKind::Words(w)) => reference::crossref(net, w).await,
            (Source::Searx, QueryKind::Words(w)) => reference::searxng(net, &cfg.searxng, w).await,
            (Source::Sru, QueryKind::Words(w)) => reference::sru(net, &cfg.sru, w).await,
            (Source::OpenSearch(i), QueryKind::Words(w)) => match cfg.opensearch.get(i) {
                Some(e) => reference::opensearch(net, &e.name, &e.url, w).await,
                None => Section::nothing("OpenSearch", "OpenSearch", "not configured"),
            },
            (s, _) => Section::nothing(s.name(), s.name(), "does not apply"),
        }
    }
}

/// A finished source, sent back to the window.
#[derive(Debug, Clone)]
pub struct Answer {
    pub req: u64,
    pub slot: usize,
    pub section: Section,
}

pub struct InquireWindow {
    input: LineInput,
    editing: bool,
    report: Report,
    req: u64,
    started: Option<Instant>,
    query: Option<Query>,
}

impl InquireWindow {
    pub fn new(initial: &str, cx: &mut Cx) -> Self {
        let mut w = Self {
            input: LineInput::new(initial),
            editing: initial.is_empty(),
            report: Report::default(),
            req: 0,
            started: None,
            query: None,
        };
        if !initial.is_empty() {
            w.run(cx);
        } else {
            w.report.push_log(alouette_lookup::Out::new(
                alouette_lookup::Style::Dim,
                "Type a domain, IP address, AS number, user@host, URL or words, then press Enter.",
            ));
        }
        w
    }

    fn run(&mut self, cx: &mut Cx) {
        let q = Query::parse(&self.input.value);
        if q.raw.is_empty() {
            return;
        }
        let cfg = cx.config.inquire.clone();
        let srcs = Source::for_query(&q, &cfg);
        self.req = NEXT.fetch_add(1, Ordering::Relaxed);
        self.started = Some(Instant::now());
        self.report.clear();
        self.report
            .set_slots(srcs.iter().map(|s| Slot::Waiting { source: s.name().into(), title: describe(&q) }).collect());
        if let QueryKind::Url(u) = &q.kind {
            self.report.push_log(alouette_lookup::Out::link(format!("Open {u}"), u.to_string()));
        }
        for (slot, src) in srcs.into_iter().enumerate() {
            let net = cx.net.clone();
            let q2 = q.clone();
            let cfg2 = cfg.clone();
            let req = self.req;
            cx.tasks.spawn(MODULE, format!("Inquire {}: {}", src.name(), q.raw), move |ctx| {
                let net = net.clone();
                let q = q2.clone();
                let cfg = cfg2.clone();
                async move {
                    let section = src.run(&net, &q, &cfg).await;
                    let failed = section.lines.iter().any(|l| l.style == alouette_lookup::Style::Bad)
                        && section.lines.len() == 1;
                    let err = failed.then(|| section.lines[0].text.clone());
                    ctx.send(AppEvent::Module {
                        module: MODULE,
                        payload: ModulePayload::new(Answer { req, slot, section }),
                    });
                    match err {
                        Some(e) => Err(e),
                        None => Ok(()),
                    }
                }
            });
        }
        if let Some(store) = cx.store {
            let _ = store.add_history(&format!("inquire:{}", q.raw), &format!("Inquire {}", q.raw), MODULE);
        }
        self.query = Some(q);
        self.editing = false;
    }
}

fn describe(q: &Query) -> String {
    match &q.kind {
        QueryKind::Domain(d) => d.clone(),
        QueryKind::Ip(ip) => ip.to_string(),
        QueryKind::Asn(n) => format!("AS{n}"),
        QueryKind::Address { user, host } => format!("{user}@{host}"),
        QueryKind::Url(u) => u.to_string(),
        QueryKind::Words(w) => format!("\"{w}\""),
    }
}

impl Window for InquireWindow {
    fn title(&self) -> String {
        match &self.query {
            Some(q) => format!("Inquire: {}", q.raw),
            None => "Inquire".into(),
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(match &self.query {
            Some(q) => format!("inquire:{}", q.raw),
            None => "inquire:".into(),
        })
    }
    fn wants_text(&self) -> bool {
        self.editing
    }
    fn animates(&self) -> bool {
        self.report.waiting() > 0
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.editing {
            vec![("Enter", "Inquire"), ("Esc", "Results")]
        } else {
            vec![("/", "New query"), ("Tab", "Link"), ("Enter", "Open")]
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 3 {
            return;
        }
        let field = Style::new().fg(t.input_fg).bg(t.input_bg);
        let label = " Inquire ▸ ";
        let mut spans = vec![Span::styled(label, t.accent()), Span::styled(self.input.value.clone(), field)];
        let waiting = self.report.waiting();
        if waiting > 0 {
            spans.push(Span::styled(format!("   {waiting} still answering"), t.dim()));
        } else if self.query.is_some() {
            spans.push(Span::styled(format!("   {} sources answered", self.report.slots.len()), t.dim()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(field), Rect { height: 1, ..area });
        if self.editing && cx.focused {
            let x = area.x as usize + label.chars().count() + self.input.cursor_col();
            frame.set_cursor_position(((x as u16).min(area.right().saturating_sub(1)), area.y));
        }
        let body = Rect { y: area.y + 2, height: area.height - 2, ..area };
        let tick = self.started.map(|s| s.elapsed().as_millis()).unwrap_or(0);
        self.report.draw(frame, body, t, tick);
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
        let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
        if ev.row == area.y {
            self.editing = true;
            return Handled::Yes;
        }
        self.report.on_mouse(ev, body, cx)
    }

    fn on_event(&mut self, ev: &AppEvent, _cx: &mut Cx) {
        if let AppEvent::Module { module: MODULE, payload } = ev
            && let Some(a) = payload.downcast_ref::<Answer>()
            && a.req == self.req
        {
            self.report.fill(a.slot, a.section.clone());
        }
    }
}
