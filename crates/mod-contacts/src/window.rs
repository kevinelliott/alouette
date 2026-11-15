//! The Contacts window: people on the left, the open card on the right with what you can
//! do with each address.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alouette_core::config::DavAccount;
use alouette_core::event::{EventSender, ModulePayload};
use alouette_core::{AppEvent, Notification};
use alouette_pim::Contact;
use alouette_pim::contact::{Typed, fediverse_url};
use alouette_pim::dav::Kind as DavKind;
use alouette_pim::store::{Folder, Stores, file_for};
use alouette_ui::ask::Asker;
use alouette_ui::chrome::fit;
use alouette_ui::form::{Form, FormResult};
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::{MODULE, Open, account, next_id};

/// Changes to the address books, from this window or another (such as Mail adding a
/// sender). Every Contacts window reloads.
#[derive(Clone)]
pub struct Changed;

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Synced { book: String, result: Result<String, String> },
    Found(Result<Vec<Contact>, String>),
    Tick,
}

fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Msg { win, kind }) });
}

/// Tell every Contacts window that the books changed.
pub fn changed(events: &EventSender) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Changed) });
}

#[derive(Clone)]
struct Row {
    book: String,
    file: String,
    contact: Contact,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Card,
}

enum Prompt {
    Filter(LineInput),
    Directory(LineInput),
    Import(LineInput),
}

pub struct ContactsWindow {
    id: u64,
    stores: Stores,
    rows: Vec<Row>,
    /// Directory results, shown instead of the books while set.
    found: Option<Vec<Contact>>,
    filter: String,
    sel: usize,
    scroll: usize,
    focus: Focus,
    action: usize,
    prompt: Option<Prompt>,
    editor: Option<(Form, Option<Row>)>,
    confirm_delete: bool,
    note: Option<String>,
    syncing: usize,
    asker: Asker,
    typed: String,
    alive: Arc<AtomicBool>,
    areas: [Rect; 2],
}

impl Drop for ContactsWindow {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        self.asker.answer(None);
    }
}

/// The books Alouette knows: "local" and every CardDAV book in the config.
fn book_names(cx_books: &[DavAccount]) -> Vec<String> {
    let mut v = vec!["local".to_string()];
    v.extend(cx_books.iter().map(|b| b.name.clone()));
    v
}

fn join_values(v: &[Typed]) -> String {
    v.iter().map(|t| t.value.clone()).collect::<Vec<_>>().join(", ")
}

/// Values typed as "a, b": kinds kept for values that were already there.
fn merge(old: &[Typed], text: &str) -> Vec<Typed> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|v| old.iter().find(|o| o.value == v).cloned().unwrap_or_else(|| Typed::new("", v)))
        .collect()
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// What can be done with a card: a label and the URL that does it.
pub fn actions(c: &Contact) -> Vec<(String, String)> {
    let mut out = vec![];
    for e in &c.emails {
        out.push((format!("Write to {}", e.value), format!("mailto:{}", e.value)));
    }
    for h in &c.fediverse {
        if let Some(u) = fediverse_url(h) {
            out.push((format!("See {h} on the fediverse"), u));
        }
    }
    for s in &c.ssh {
        let target = if s.contains("://") { s.clone() } else { format!("ssh://{s}") };
        out.push((format!("Connect to {s} over SSH"), target));
    }
    for ch in &c.chat {
        out.push((format!("Chat at {ch}"), ch.clone()));
    }
    for u in &c.urls {
        out.push((format!("Open {u}"), u.clone()));
    }
    if let Some(e) = c.email() {
        out.push(("Look up the address".into(), format!("inquire:{e}")));
    }
    out
}

impl ContactsWindow {
    pub fn new(data_dir: PathBuf, open: Open, cx: &mut Cx) -> Self {
        let wake = cx.tasks.events();
        let asker = Asker::new(move || {
            let _ = wake.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(()) });
        });
        let mut w = ContactsWindow {
            id: next_id(),
            stores: Stores::new(&data_dir),
            rows: vec![],
            found: None,
            filter: open.query.clone(),
            sel: 0,
            scroll: 0,
            focus: Focus::List,
            action: 0,
            prompt: None,
            editor: None,
            confirm_delete: false,
            note: None,
            syncing: 0,
            asker,
            typed: String::new(),
            alive: Arc::new(AtomicBool::new(true)),
            areas: [Rect::default(); 2],
        };
        w.reload(cx);
        if let Some((name, email)) = open.new {
            // Already known? Show that card instead.
            if let Some(i) = w.visible().iter().position(|r| !email.is_empty() && r.contact.has_email(&email)) {
                w.sel = i;
                w.focus = Focus::Card;
                w.note = Some(format!("{} is already in your contacts", w.visible()[i].contact.name));
            } else {
                let mut c = Contact::new(if name.is_empty() { &email } else { &name });
                if !email.is_empty() {
                    c.emails.push(Typed::new("", &email));
                }
                w.edit(Some(c), None, cx);
            }
        }
        if w.rows.is_empty() && w.editor.is_none() {
            w.note = Some("No contacts yet. Press a to add one, or i to import a .vcf file.".into());
        }
        w.sync_all(cx);
        let (alive, id, events) = (w.alive.clone(), w.id, cx.tasks.events());
        let every = std::time::Duration::from_secs(cx.config.contacts.sync_mins.max(1) * 60);
        if !cx.config.contacts.books.is_empty() {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if !alive.load(Ordering::Relaxed) {
                        return;
                    }
                    send(&events, id, Kind::Tick);
                }
            });
        }
        w
    }

    fn reload(&mut self, cx: &Cx) {
        let mut rows = vec![];
        let mut names = book_names(&cx.config.contacts.books);
        for b in self.stores.books() {
            if !names.contains(&b) {
                names.push(b);
            }
        }
        for book in names {
            for item in self.stores.book(&book).list() {
                for c in Contact::parse_all(&item.text) {
                    rows.push(Row { book: book.clone(), file: item.file.clone(), contact: c });
                }
            }
        }
        rows.sort_by_key(|r| r.contact.sort_key());
        let keep = self.visible().get(self.sel).map(|r| r.contact.uid.clone());
        self.rows = rows;
        if let Some(uid) = keep
            && let Some(i) = self.visible().iter().position(|r| r.contact.uid == uid)
        {
            self.sel = i;
        }
        self.sel = self.sel.min(self.visible().len().saturating_sub(1));
    }

    fn visible(&self) -> Vec<Row> {
        if let Some(f) = &self.found {
            return f
                .iter()
                .map(|c| Row { book: "directory".into(), file: String::new(), contact: c.clone() })
                .collect();
        }
        self.rows.iter().filter(|r| self.filter.is_empty() || r.contact.matches(&self.filter)).cloned().collect()
    }

    fn current(&self) -> Option<Row> {
        self.visible().get(self.sel).cloned()
    }

    fn folder(&self, book: &str) -> Folder {
        self.stores.book(book)
    }

    fn sync_all(&mut self, cx: &Cx) {
        for acct in cx.config.contacts.books.clone() {
            self.sync(acct, cx);
        }
    }

    fn sync(&mut self, acct: DavAccount, cx: &Cx) {
        if acct.url.is_empty() {
            return;
        }
        self.syncing += 1;
        let folder = self.folder(&acct.name);
        let (id, events, net, secrets, asker) =
            (self.id, cx.tasks.events(), cx.net.clone(), cx.secrets.clone(), self.asker.clone());
        cx.tasks.spawn(MODULE, format!("Sync contacts: {}", acct.name), move |_ctx| {
            let (acct, folder, events, net, secrets, asker) =
                (acct.clone(), folder.clone(), events.clone(), net.clone(), secrets.clone(), asker.clone());
            async move {
                let r = account::sync(&net, &secrets, &asker, &acct, DavKind::Contacts, &folder).await;
                let result = r.as_ref().map(|r| r.summary()).map_err(|e| e.clone());
                send(&events, id, Kind::Synced { book: acct.name.clone(), result });
                r.map(|_| ())
            }
        });
    }

    fn sync_book(&mut self, book: &str, cx: &Cx) {
        if let Some(a) = cx.config.contacts.books.iter().find(|b| b.name == book).cloned() {
            self.sync(a, cx);
        }
    }

    /// Open the editor for a card. `row` is where it lives now, for an edit.
    fn edit(&mut self, c: Option<Contact>, row: Option<Row>, cx: &Cx) {
        let c = c.or_else(|| row.as_ref().map(|r| r.contact.clone())).unwrap_or_else(|| Contact::new(""));
        let books = book_names(&cx.config.contacts.books);
        let default_book =
            row.as_ref().map(|r| r.book.clone()).unwrap_or_else(|| books.last().cloned().unwrap_or_default());
        let title = if row.is_some() { "Edit contact" } else { "New contact" };
        let form = Form::new(title)
            .field("Name", &c.name, "Ada Lovelace")
            .field("Email", &join_values(&c.emails), "ada@example.org, ada@home.example")
            .field("Phone", &join_values(&c.phones), "+44 20 7946 0000")
            .field("Organisation", c.org.as_deref().unwrap_or(""), "")
            .field("Title", c.title.as_deref().unwrap_or(""), "")
            .field("Fediverse", &c.fediverse.join(", "), "@ada@mastodon.social")
            .field("SSH", &c.ssh.join(", "), "ada@engine.example.org, or a saved host name")
            .field("Chat", &c.chat.join(", "), "matrix:u/ada:example.org, irc://libera.chat/ada")
            .field("Web", &c.urls.join(", "), "https://ada.example")
            .field("Birthday", c.birthday.as_deref().unwrap_or(""), "1815-12-10")
            .field("Note", c.note.as_deref().unwrap_or(""), "")
            .field("Book", &default_book, &books.join(", "));
        let mut row = row;
        if row.is_none() {
            row = Some(Row { book: String::new(), file: String::new(), contact: c });
        }
        self.editor = Some((form, row));
    }

    fn save_editor(&mut self, cx: &mut Cx) {
        let Some((form, row)) = self.editor.take() else { return };
        let books = {
            let mut b = book_names(&cx.config.contacts.books);
            b.extend(self.stores.books());
            b
        };
        let name = form.get("Name");
        let book = form.get("Book");
        let fail = |mut form: Form, msg: &str, row: Option<Row>, me: &mut Self| {
            form.note = Some(msg.to_string());
            me.editor = Some((form, row));
        };
        if name.is_empty() && form.get("Email").is_empty() {
            return fail(form, "Give the contact a name or an address.", row, self);
        }
        if !books.contains(&book) {
            return fail(form, &format!("Pick a book: {}", books.join(", ")), row, self);
        }
        let old = row.clone().map(|r| r.contact).unwrap_or_default();
        let mut c = old.clone();
        if c.uid.is_empty() {
            c.uid = alouette_pim::contact::new_uid();
        }
        let name = if name.is_empty() { form.get("Email") } else { name };
        if name != old.name {
            let fresh = Contact::new(&name);
            c.given = fresh.given;
            c.family = fresh.family;
        }
        c.name = name;
        c.emails = merge(&old.emails, &form.get("Email"));
        c.phones = merge(&old.phones, &form.get("Phone"));
        c.org = Some(form.get("Organisation")).filter(|s| !s.is_empty());
        c.title = Some(form.get("Title")).filter(|s| !s.is_empty());
        c.fediverse = split_list(&form.get("Fediverse"))
            .into_iter()
            .map(|h| alouette_pim::contact::fediverse_handle(&h).unwrap_or(h))
            .collect();
        c.ssh = split_list(&form.get("SSH"));
        c.chat = split_list(&form.get("Chat"));
        c.urls = split_list(&form.get("Web"));
        c.birthday = Some(form.get("Birthday")).filter(|s| !s.is_empty());
        c.note = Some(form.get("Note")).filter(|s| !s.is_empty());
        let text = c.to_vcard();
        let (old_book, old_file) = row.map(|r| (r.book, r.file)).unwrap_or_default();
        let file = if old_book == book && !old_file.is_empty() { old_file.clone() } else { file_for(&c.uid, "vcf") };
        if let Err(e) = self.folder(&book).put_local(&file, &text) {
            self.note = Some(format!("Could not save: {e}"));
            return;
        }
        // Moved to another book: out of the old one.
        if !old_file.is_empty()
            && !old_book.is_empty()
            && old_book != "directory"
            && (old_book != book || old_file != file)
        {
            let _ = self.folder(&old_book).delete_local(&old_file);
            self.sync_book(&old_book, cx);
        }
        self.found = None;
        self.reload(cx);
        if let Some(i) = self.visible().iter().position(|r| r.contact.uid == c.uid) {
            self.sel = i;
        }
        self.note = Some(format!("Saved {}", c.name));
        self.sync_book(&book, cx);
        changed(&cx.tasks.events());
    }

    fn delete(&mut self, cx: &mut Cx) {
        let Some(r) = self.current() else { return };
        if r.book == "directory" {
            return;
        }
        match self.folder(&r.book).delete_local(&r.file) {
            Ok(()) => {
                self.note = Some(format!("Deleted {}", r.contact.name));
                self.reload(cx);
                self.sync_book(&r.book, cx);
                changed(&cx.tasks.events());
            }
            Err(e) => self.note = Some(format!("Could not delete: {e}")),
        }
    }

    fn search_directories(&mut self, q: String, cx: &Cx) {
        let dirs = cx.config.contacts.directories.clone();
        if dirs.is_empty() {
            self.note = Some("No directories set up. Add one under [[contacts.directories]].".into());
            return;
        }
        self.note =
            Some(format!("Searching {} for {q}…", dirs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")));
        let (id, events, net, secrets, asker) =
            (self.id, cx.tasks.events(), cx.net.clone(), cx.secrets.clone(), self.asker.clone());
        cx.tasks.spawn(MODULE, format!("Directory search: {q}"), move |_ctx| {
            let (dirs, q, events, net, secrets, asker) =
                (dirs.clone(), q.clone(), events.clone(), net.clone(), secrets.clone(), asker.clone());
            async move {
                let mut all = vec![];
                let mut errors = vec![];
                for d in &dirs {
                    let Some(pw) = account::directory_password(&secrets, &asker, &d.name, &d.bind_dn).await else {
                        errors.push(format!("{}: cancelled", d.name));
                        continue;
                    };
                    match alouette_pim::ldap::search(&net, &d.url, &d.bind_dn, &pw, &q).await {
                        Ok(found) => all.extend(found.iter().map(|e| e.to_contact())),
                        Err(e) => {
                            if e.contains("did not accept") {
                                account::forget_directory_password(&secrets, &d.name);
                            }
                            errors.push(format!("{}: {e}", d.name));
                        }
                    }
                }
                let n = all.len();
                let res = if all.is_empty() && !errors.is_empty() { Err(errors.join("; ")) } else { Ok(all) };
                send(&events, id, Kind::Found(res.clone()));
                let _ = n;
                res.map(|_| ())
            }
        });
    }

    fn import(&mut self, path: &str, cx: &mut Cx) {
        let path = path.trim();
        let path = match path.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| rest.into()),
            None => PathBuf::from(path),
        };
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                self.note = Some(format!("{}: {e}", path.display()));
                return;
            }
        };
        let cards = Contact::parse_all(&text);
        if cards.is_empty() {
            self.note = Some(format!("{} has no contacts in it", path.display()));
            return;
        }
        let book = book_names(&cx.config.contacts.books).last().cloned().unwrap_or_else(|| "local".into());
        let folder = self.folder(&book);
        let mut n = 0;
        for mut c in cards {
            if c.uid.is_empty() {
                c.uid = alouette_pim::contact::new_uid();
            }
            if folder.put_local(&file_for(&c.uid, "vcf"), &c.to_vcard()).is_ok() {
                n += 1;
            }
        }
        self.note = Some(format!("Imported {n} contacts into {book}"));
        self.reload(cx);
        self.sync_book(&book, cx);
        changed(&cx.tasks.events());
    }

    fn open_action(&mut self, i: usize, cx: &mut Cx) {
        let Some(r) = self.current() else { return };
        if let Some((_, url)) = actions(&r.contact).get(i) {
            cx.open_url(url.clone());
        }
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let block = Block::default().borders(Borders::RIGHT).border_style(t.dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[0] = inner;
        let rows = self.visible();
        let h = inner.height as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + h {
            self.scroll = self.sel + 1 - h;
        }
        let w = inner.width as usize;
        let focused = cx.focused && self.focus == Focus::List;
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(h)
            .map(|(i, r)| {
                let mark = if i == self.sel { "▸" } else { " " };
                let extra = r.contact.org.clone().or_else(|| r.contact.email().map(str::to_string)).unwrap_or_default();
                let nw = (r.contact.name.chars().count() + 1).min(w.saturating_sub(1));
                let name = fit(&r.contact.name, nw);
                let rest = fit(&format!(" {extra}"), w.saturating_sub(nw + 1));
                if i == self.sel && focused {
                    Line::from(Span::styled(format!("{mark}{name}{rest}"), t.selection()))
                } else {
                    Line::from(vec![Span::raw(format!("{mark}{name}")), Span::styled(rest, t.dim())])
                }
            })
            .collect();
        if lines.is_empty() {
            let msg = if self.found.is_some() {
                "Nobody in the directory matches.".to_string()
            } else if !self.filter.is_empty() {
                format!("Nobody matches \"{}\". Esc shows everyone.", self.filter)
            } else {
                "No contacts yet. a adds one.".into()
            };
            frame.render_widget(Paragraph::new(msg).style(t.dim()).wrap(Wrap { trim: false }), inner);
        } else {
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    fn draw_card(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.areas[1] = area;
        let Some(r) = self.current() else { return };
        let c = &r.contact;
        let mut lines: Vec<Line> =
            vec![Line::from(Span::styled(c.name.clone(), t.accent().add_modifier(Modifier::BOLD)))];
        let job = [c.title.clone(), c.org.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
        if !job.is_empty() {
            lines.push(Line::from(job));
        }
        if let Some(n) = &c.nickname {
            lines.push(Line::from(Span::styled(format!("“{n}”"), t.dim())));
        }
        lines.push(Line::default());
        let field = |label: &str, value: String| {
            Line::from(vec![Span::styled(format!("{label:>10}  "), t.dim()), Span::raw(value)])
        };
        let kind = |k: &str| if k.is_empty() { String::new() } else { format!(" ({k})") };
        for e in &c.emails {
            lines.push(field("Email", format!("{}{}", e.value, kind(&e.kind))));
        }
        for p in &c.phones {
            lines.push(field("Phone", format!("{}{}", p.value, kind(&p.kind))));
        }
        for a in &c.addresses {
            lines.push(field("Address", format!("{}{}", a.value, kind(&a.kind))));
        }
        for h in &c.fediverse {
            lines.push(field("Fediverse", h.clone()));
        }
        for s in &c.ssh {
            lines.push(field("SSH", s.clone()));
        }
        for s in &c.chat {
            lines.push(field("Chat", s.clone()));
        }
        for u in &c.urls {
            lines.push(field("Web", u.clone()));
        }
        if let Some(b) = &c.birthday {
            lines.push(field("Birthday", b.clone()));
        }
        if !c.categories.is_empty() {
            lines.push(field("Groups", c.categories.join(", ")));
        }
        lines.push(field("Book", r.book.clone()));
        if let Some(n) = &c.note {
            lines.push(Line::default());
            for l in n.lines() {
                lines.push(Line::from(l.to_string()));
            }
        }
        let acts = actions(c);
        if !acts.is_empty() {
            lines.push(Line::default());
            let focused = cx.focused && self.focus == Focus::Card;
            for (i, (label, _)) in acts.iter().enumerate() {
                let key = if i < 9 { format!("{}", i + 1) } else { " ".into() };
                let st = if focused && i == self.action { t.selection() } else { Style::new() };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {key} "), t.accent()),
                    Span::styled(label.clone(), st),
                ]));
            }
        }
        if r.book == "directory" {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("From the directory: s saves this card to your contacts.", t.dim())));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }
}

impl Window for ContactsWindow {
    fn title(&self) -> String {
        match (&self.found, self.filter.is_empty()) {
            (Some(_), _) => "Contacts: directory".into(),
            (None, false) => format!("Contacts: {}", self.filter),
            _ => "Contacts".into(),
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some("contacts:".into())
    }

    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.editor.is_some() {
            return vec![("Tab", "next field"), ("Ctrl+S", "save"), ("Esc", "cancel")];
        }
        vec![
            ("a", "add"),
            ("e", "edit"),
            ("1-9", "act"),
            ("/", "find"),
            ("L", "directory"),
            ("i", "import"),
            ("g", "sync"),
        ]
    }

    fn wants_text(&self) -> bool {
        self.prompt.is_some() || self.editor.is_some() || self.asker.question().is_some()
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        if payload.downcast_ref::<Changed>().is_some() {
            self.reload(cx);
            return;
        }
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match &m.kind {
            Kind::Synced { book, result } => {
                self.syncing = self.syncing.saturating_sub(1);
                match result {
                    Ok(s) => {
                        if s != "up to date" {
                            self.note = Some(format!("{book}: {s}"));
                        }
                    }
                    Err(e) => {
                        self.note = Some(format!("{book}: {e}"));
                        cx.notify(Notification::warning(format!("Contacts, {book}: {e}")));
                    }
                }
                self.reload(cx);
            }
            Kind::Found(r) => match r {
                Ok(v) => {
                    self.note = Some(format!("{} found in the directory. Esc goes back.", v.len()));
                    self.found = Some(v.clone());
                    self.sel = 0;
                    self.scroll = 0;
                }
                Err(e) => self.note = Some(e.clone()),
            },
            Kind::Tick => self.sync_all(cx),
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.asker.question().is_some() {
            self.typed.push_str(text.trim_end_matches(['\r', '\n']));
            return Handled::Yes;
        }
        if let Some((f, _)) = &mut self.editor {
            f.on_paste(text);
            return Handled::Yes;
        }
        if let Some(Prompt::Filter(i) | Prompt::Directory(i) | Prompt::Import(i)) = &mut self.prompt {
            let v = format!("{}{}", i.value, text.trim());
            i.set(&v);
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        if let Some((form, _)) = &mut self.editor {
            match form.on_key(key) {
                FormResult::Submit => self.save_editor(cx),
                FormResult::Cancel => self.editor = None,
                _ => {}
            }
            return Handled::Yes;
        }
        if let Some(p) = &mut self.prompt {
            let (input, live) = match p {
                Prompt::Filter(i) => (i, true),
                Prompt::Directory(i) | Prompt::Import(i) => (i, false),
            };
            match input.on_key(key) {
                InputResult::Submit => {
                    let v = input.value.trim().to_string();
                    match self.prompt.take() {
                        Some(Prompt::Directory(_)) if !v.is_empty() => self.search_directories(v, cx),
                        Some(Prompt::Import(_)) if !v.is_empty() => self.import(&v, cx),
                        _ => {}
                    }
                }
                InputResult::Cancel => {
                    if live {
                        self.filter.clear();
                    }
                    self.prompt = None;
                }
                InputResult::Edited if live => {
                    self.filter = input.value.clone();
                    self.sel = 0;
                    self.scroll = 0;
                }
                _ => {}
            }
            return Handled::Yes;
        }
        if self.confirm_delete {
            self.confirm_delete = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.delete(cx);
            } else {
                self.note = Some("Kept".into());
            }
            return Handled::Yes;
        }
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return Handled::No;
        }
        let n = self.visible().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::List => self.sel = self.sel.saturating_sub(1),
                Focus::Card => self.action = self.action.saturating_sub(1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::List => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
                Focus::Card => {
                    let a = self.current().map(|r| actions(&r.contact).len()).unwrap_or(0);
                    self.action = (self.action + 1).min(a.saturating_sub(1));
                }
            },
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Tab | KeyCode::Right => {
                self.focus = if self.focus == Focus::List { Focus::Card } else { Focus::List };
                self.action = 0;
            }
            KeyCode::Left => self.focus = Focus::List,
            KeyCode::Enter => match self.focus {
                Focus::List => {
                    self.focus = Focus::Card;
                    self.action = 0;
                }
                Focus::Card => self.open_action(self.action, cx),
            },
            KeyCode::Char(d @ '1'..='9') => self.open_action(d as usize - '1' as usize, cx),
            KeyCode::Esc => {
                if self.found.take().is_some() {
                    self.sel = 0;
                    self.note = None;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                } else if self.focus == Focus::Card {
                    self.focus = Focus::List;
                } else {
                    return Handled::No;
                }
            }
            KeyCode::Char('/') => {
                self.found = None;
                self.prompt = Some(Prompt::Filter(LineInput::new(&self.filter)));
            }
            KeyCode::Char('L') => self.prompt = Some(Prompt::Directory(LineInput::new(""))),
            KeyCode::Char('i') => self.prompt = Some(Prompt::Import(LineInput::new("~/"))),
            KeyCode::Char('a') | KeyCode::Char('n') => self.edit(None, None, cx),
            KeyCode::Char('e') => match self.current() {
                Some(r) if r.book == "directory" => self.edit(Some(r.contact), None, cx),
                Some(r) => self.edit(None, Some(r), cx),
                None => {}
            },
            KeyCode::Char('s') => {
                if let Some(r) = self.current().filter(|r| r.book == "directory") {
                    self.edit(Some(r.contact), None, cx);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(r) = self.current().filter(|r| r.book != "directory") {
                    self.confirm_delete = true;
                    self.note = Some(format!("Delete {}? y deletes, any other key keeps.", r.contact.name));
                }
            }
            KeyCode::Char('m') => {
                if let Some(e) = self.current().and_then(|r| r.contact.email().map(str::to_string)) {
                    cx.open_url(format!("mailto:{e}"));
                }
            }
            KeyCode::Char('g') => {
                if cx.config.contacts.books.is_empty() {
                    self.note = Some("No CardDAV books set up; contacts here are kept on this computer.".into());
                } else {
                    self.note = Some("Syncing…".into());
                    self.sync_all(cx);
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        let [list, card] = self.areas;
        let inside = |r: Rect| ev.column >= r.x && ev.column < r.right() && ev.row >= r.y && ev.row < r.bottom();
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) if inside(list) => {
                let i = self.scroll + (ev.row - list.y) as usize;
                if i < self.visible().len() {
                    self.sel = i;
                    self.focus = Focus::List;
                }
                Handled::Yes
            }
            MouseEventKind::Down(MouseButton::Left) if inside(card) => {
                self.focus = Focus::Card;
                let _ = cx;
                Handled::Yes
            }
            MouseEventKind::ScrollUp => {
                self.sel = self.sel.saturating_sub(3);
                Handled::Yes
            }
            MouseEventKind::ScrollDown => {
                self.sel = (self.sel + 3).min(self.visible().len().saturating_sub(1));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 3 {
            return;
        }
        let body = Rect { height: area.height - 1, ..area };
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let lw = (area.width / 3).clamp(20, 44).min(area.width);
        let left = Rect { width: lw, ..body };
        let right = Rect { x: body.x + lw + 1, width: body.width.saturating_sub(lw + 1), ..body };
        self.draw_list(frame, left, cx);
        self.draw_card(frame, right, cx);
        let status = match &self.prompt {
            Some(Prompt::Filter(i)) => format!("Find: {}", i.value),
            Some(Prompt::Directory(i)) => format!("Search the directory for: {}", i.value),
            Some(Prompt::Import(i)) => format!("Import contacts from (.vcf): {}", i.value),
            None => {
                let mut s = self.note.clone().unwrap_or_default();
                if self.syncing > 0 && s.is_empty() {
                    s = "Syncing…".into();
                }
                if s.is_empty() {
                    s = format!("{} contacts", self.rows.len());
                }
                s
            }
        };
        frame.render_widget(
            Paragraph::new(Span::styled(fit(&format!(" {status}"), foot.width as usize), t.dim())),
            foot,
        );
        if let Some(Prompt::Filter(i) | Prompt::Directory(i) | Prompt::Import(i)) = &self.prompt {
            let label = status.len() - i.value.len();
            let x = foot.x + 1 + label as u16 + i.cursor_col() as u16;
            if x < foot.right() {
                frame.set_cursor_position((x, foot.y));
            }
        }
        if let Some((f, _)) = &self.editor {
            f.draw(frame, area, t);
        }
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_card_links_mail_fediverse_and_ssh() {
        let mut c = Contact::new("Ada Lovelace");
        c.emails.push(Typed::new("work", "ada@example.org"));
        c.fediverse.push("@ada@mastodon.social".into());
        c.ssh.push("ada@engine.example.org".into());
        let a: Vec<String> = actions(&c).into_iter().map(|(_, u)| u).collect();
        assert_eq!(
            a,
            [
                "mailto:ada@example.org",
                "https://mastodon.social/@ada",
                "ssh://ada@engine.example.org",
                "inquire:ada@example.org"
            ]
        );
        assert_eq!(merge(&[Typed::new("work", "a@x")], "a@x, b@y")[0].kind, "work");
    }
}
