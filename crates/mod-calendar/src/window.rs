//! The Calendar window: an agenda, a week or a month, with the chosen event below.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alouette_core::config::{CalSub, DavAccount};
use alouette_core::event::{EventSender, ModulePayload};
use alouette_core::{AppEvent, Notification};
use alouette_pim::dav::Kind as DavKind;
use alouette_pim::event::{Calendar, Event, When, instances};
use alouette_pim::rrule::{Freq, RRule};
use alouette_pim::store::Stores;
use alouette_ui::ask::Asker;
use alouette_ui::chrome::fit;
use alouette_ui::form::{Form, FormResult};
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, ToSpan, Zoned};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::{MODULE, Stored, load_all, next_id, parse_day, parse_time, say_minutes};

#[derive(Debug, Clone, Default)]
pub struct Open {
    pub view: String,
    pub day: Option<Date>,
    pub subscribe: Option<String>,
}

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Synced { name: String, result: Result<String, String> },
    Tick,
}

fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Msg { win, kind }) });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Agenda,
    Week,
    Month,
}

enum Prompt {
    Subscribe(LineInput),
    Import(LineInput),
    GoTo(LineInput),
}

/// One occurrence, owned.
#[derive(Clone)]
struct Inst {
    idx: usize,
    start: Zoned,
    end: Zoned,
    all_day: bool,
}

const AGENDA_DAYS: i64 = 60;

pub struct CalendarWindow {
    id: u64,
    stores: Stores,
    stored: Vec<Stored>,
    events: Vec<Event>,
    tz: TimeZone,
    view: View,
    day: Date,
    sel: usize,
    scroll: usize,
    editor: Option<(Form, Option<usize>)>,
    prompt: Option<Prompt>,
    /// Waiting for y (or o, for one occurrence) to delete.
    confirm: Option<Inst>,
    note: Option<String>,
    syncing: usize,
    asker: Asker,
    typed: String,
    alive: Arc<AtomicBool>,
    week_start: Weekday,
    areas: Vec<(Rect, Date)>,
}

impl Drop for CalendarWindow {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        self.asker.answer(None);
    }
}

/// Calendars that can be written to: "local" and the CalDAV accounts.
fn writable(cx_accounts: &[DavAccount]) -> Vec<String> {
    let mut v = vec!["local".to_string()];
    v.extend(cx_accounts.iter().map(|a| a.name.clone()));
    v
}

fn sub_name(url: &str) -> String {
    let u = url.replace("webcal://", "https://").replace("webcals://", "https://");
    let parsed = url::Url::parse(&u).ok();
    let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("calendar").trim_start_matches("www.").to_string();
    let stem = parsed
        .as_ref()
        .and_then(|u| u.path_segments().and_then(|mut s| s.rfind(|x| !x.is_empty()).map(str::to_string)))
        .map(|s| s.trim_end_matches(".ics").to_string())
        .filter(|s| !s.is_empty() && s.len() < 30);
    match stem {
        Some(s) => format!("{host} {s}"),
        None => host,
    }
}

fn repeat_rule(text: &str, start: Date) -> Result<Option<RRule>, String> {
    let t = text.trim().to_lowercase();
    Ok(Some(match t.as_str() {
        "" | "none" | "no" | "never" => return Ok(None),
        "daily" | "every day" => RRule::new(Freq::Daily),
        "weekly" | "every week" => RRule::new(Freq::Weekly),
        "fortnightly" | "every 2 weeks" => RRule { interval: 2, ..RRule::new(Freq::Weekly) },
        "monthly" | "every month" => RRule::new(Freq::Monthly),
        "yearly" | "annually" | "every year" => RRule::new(Freq::Yearly),
        "weekdays" | "every weekday" => RRule {
            byday: [Weekday::Monday, Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday]
                .into_iter()
                .map(|w| (0, w))
                .collect(),
            ..RRule::new(Freq::Weekly)
        },
        _ => match RRule::parse(&text.trim().to_uppercase()) {
            Some(r) => r,
            None => {
                let _ = start;
                return Err("Repeat: none, daily, weekdays, weekly, fortnightly, monthly, yearly, or an RRULE".into());
            }
        },
    }))
}

fn repeat_text(r: &Option<RRule>) -> String {
    let Some(r) = r else { return String::new() };
    let plain = |f| RRule::new(f).to_value();
    let v = r.to_value();
    if v == plain(Freq::Daily) {
        "daily".into()
    } else if v == plain(Freq::Weekly) {
        "weekly".into()
    } else if v == plain(Freq::Monthly) {
        "monthly".into()
    } else if v == plain(Freq::Yearly) {
        "yearly".into()
    } else if v == "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR" {
        "weekdays".into()
    } else if v == "FREQ=WEEKLY;INTERVAL=2" {
        "fortnightly".into()
    } else {
        v
    }
}

impl CalendarWindow {
    pub fn new(data_dir: PathBuf, open: Open, cx: &mut Cx) -> Self {
        let wake = cx.tasks.events();
        let asker = Asker::new(move || {
            let _ = wake.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(()) });
        });
        let tz = TimeZone::system();
        let today = Zoned::now().with_time_zone(tz.clone()).date();
        let mut w = CalendarWindow {
            id: next_id(),
            stores: Stores::new(&data_dir),
            stored: vec![],
            events: vec![],
            tz,
            view: match open.view.as_str() {
                "week" => View::Week,
                "month" => View::Month,
                _ => View::Agenda,
            },
            day: open.day.unwrap_or(today),
            sel: 0,
            scroll: 0,
            editor: None,
            prompt: None,
            confirm: None,
            note: None,
            syncing: 0,
            asker,
            typed: String::new(),
            alive: Arc::new(AtomicBool::new(true)),
            week_start: if cx.config.calendar.week_start.eq_ignore_ascii_case("sunday") {
                Weekday::Sunday
            } else {
                Weekday::Monday
            },
            areas: vec![],
        };
        w.reload();
        if let Some(u) = open.subscribe {
            w.subscribe(u, cx);
        }
        w.sync_all(cx);
        let (alive, id, events) = (w.alive.clone(), w.id, cx.tasks.events());
        let every = std::time::Duration::from_secs(cx.config.calendar.sync_mins.max(1) * 60);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                send(&events, id, Kind::Tick);
            }
        });
        w
    }

    fn today(&self) -> Date {
        Zoned::now().with_time_zone(self.tz.clone()).date()
    }

    fn reload(&mut self) {
        self.stored = load_all(&self.stores);
        self.events = self.stored.iter().map(|s| s.event.clone()).collect();
    }

    fn range(&self, from: Date, to: Date) -> Vec<Inst> {
        let (Ok(f), Ok(t)) = (from.to_zoned(self.tz.clone()), to.to_zoned(self.tz.clone())) else { return vec![] };
        instances(&self.events, &f, &t, &self.tz)
            .into_iter()
            .filter_map(|i| {
                let idx = self.events.iter().position(|e| std::ptr::eq(e, i.event))?;
                Some(Inst { idx, start: i.start, end: i.end, all_day: i.all_day })
            })
            .collect()
    }

    /// Occurrences touching one day.
    fn on_day(&self, d: Date) -> Vec<Inst> {
        self.range(d, d.tomorrow().unwrap_or(d))
    }

    fn agenda(&self) -> Vec<Inst> {
        self.range(self.day, self.day.checked_add(AGENDA_DAYS.days()).unwrap_or(self.day))
    }

    fn selected(&self) -> Option<Inst> {
        match self.view {
            View::Agenda => self.agenda().get(self.sel).cloned(),
            _ => self.on_day(self.day).get(self.sel).cloned(),
        }
    }

    fn week_of(&self, d: Date) -> Date {
        let back = (d.weekday().to_monday_zero_offset() - self.week_start.to_monday_zero_offset()).rem_euclid(7);
        d.checked_sub((back as i64).days()).unwrap_or(d)
    }

    fn move_day(&mut self, n: i64) {
        if let Ok(d) = self.day.checked_add(n.days()) {
            self.day = d;
            self.sel = 0;
            self.scroll = 0;
        }
    }

    fn sync_all(&mut self, cx: &Cx) {
        for a in cx.config.calendar.accounts.clone() {
            self.sync_account(a, cx);
        }
        for s in cx.config.calendar.subscriptions.clone() {
            self.refresh_sub(s, cx);
        }
    }

    fn sync_account(&mut self, acct: DavAccount, cx: &Cx) {
        if acct.url.is_empty() {
            return;
        }
        self.syncing += 1;
        let folder = self.stores.calendar(&acct.name);
        let (id, events, net, secrets, asker) =
            (self.id, cx.tasks.events(), cx.net.clone(), cx.secrets.clone(), self.asker.clone());
        cx.tasks.spawn(MODULE, format!("Sync calendar: {}", acct.name), move |_ctx| {
            let (acct, folder, events, net, secrets, asker) =
                (acct.clone(), folder.clone(), events.clone(), net.clone(), secrets.clone(), asker.clone());
            async move {
                let r = mod_contacts::account::sync(&net, &secrets, &asker, &acct, DavKind::Calendars, &folder).await;
                let result = r.as_ref().map(|r| r.summary()).map_err(|e| e.clone());
                send(&events, id, Kind::Synced { name: acct.name.clone(), result });
                r.map(|_| ())
            }
        });
    }

    fn refresh_sub(&mut self, sub: CalSub, cx: &Cx) {
        self.syncing += 1;
        let folder = self.stores.calendar(&sub.name);
        let (id, events, net) = (self.id, cx.tasks.events(), cx.net.clone());
        cx.tasks.spawn(MODULE, format!("Refresh calendar: {}", sub.name), move |_ctx| {
            let (sub, folder, events, net) = (sub.clone(), folder.clone(), events.clone(), net.clone());
            async move {
                let r = alouette_pim::dav::subscribe(&net, &sub.url, &folder).await;
                let result = r.as_ref().map(|n| format!("{n} events")).map_err(|e| e.clone());
                send(&events, id, Kind::Synced { name: sub.name.clone(), result });
                r.map(|_| ())
            }
        });
    }

    fn sync_named(&mut self, name: &str, cx: &Cx) {
        if let Some(a) = cx.config.calendar.accounts.iter().find(|a| a.name == name).cloned() {
            self.sync_account(a, cx);
        }
    }

    fn subscribe(&mut self, url: String, cx: &mut Cx) {
        let url = url.trim().to_string();
        if !url.contains("://") {
            self.note = Some("Give the calendar's address, such as webcal://example.com/holidays.ics".into());
            return;
        }
        if let Some(s) = cx.config.calendar.subscriptions.iter().find(|s| s.url == url).cloned() {
            self.note = Some(format!("You already follow this calendar as {}", s.name));
            self.refresh_sub(s, cx);
            return;
        }
        let mut name = sub_name(&url);
        let taken: Vec<String> = cx
            .config
            .calendar
            .subscriptions
            .iter()
            .map(|s| s.name.clone())
            .chain(writable(&cx.config.calendar.accounts))
            .collect();
        let base = name.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base} {n}");
            n += 1;
        }
        let sub = CalSub { name: name.clone(), url };
        let s2 = sub.clone();
        cx.edit_config(move |c| c.calendar.subscriptions.push(s2));
        self.note = Some(format!("Following {name}"));
        self.refresh_sub(sub, cx);
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
        let cal = Calendar::parse(&text);
        if cal.events.is_empty() {
            self.note = Some(format!("{} has no events in it", path.display()));
            return;
        }
        let target = writable(&cx.config.calendar.accounts).last().cloned().unwrap_or_else(|| "local".into());
        let mut by_uid: BTreeMap<String, Vec<Event>> = BTreeMap::new();
        for mut e in cal.events {
            if e.uid.is_empty() {
                e.uid = alouette_pim::contact::new_uid();
            }
            by_uid.entry(e.uid.clone()).or_default().push(e);
        }
        let n = by_uid.len();
        for evs in by_uid.values() {
            if let Err(e) = alouette_pim::itip::save(&self.stores, &target, evs) {
                self.note = Some(format!("Could not save: {e}"));
                return;
            }
        }
        self.reload();
        self.note = Some(format!("Imported {n} events into {target}"));
        self.sync_named(&target, cx);
    }

    fn edit(&mut self, which: Option<usize>, cx: &Cx) {
        let cals = writable(&cx.config.calendar.accounts);
        let (title, form) = match which.and_then(|i| self.stored.get(i)) {
            Some(s) => {
                let e = &s.event;
                let start = e.start.as_ref().map(|w| w.in_zone(&self.tz));
                let end = e.end.as_ref().map(|w| w.in_zone(&self.tz)).or_else(|| {
                    start
                        .as_ref()
                        .and_then(|s| e.duration.and_then(|d| s.checked_add(SignedDuration::from_secs(d)).ok()))
                });
                let date = start.as_ref().map(|z| z.date().to_string()).unwrap_or_default();
                let (st, en) = if e.all_day() {
                    let last =
                        end.as_ref().and_then(|z| z.date().yesterday().ok()).map(|d| d.to_string()).unwrap_or_default();
                    (String::new(), if last == date { String::new() } else { last })
                } else {
                    (
                        start.as_ref().map(|z| z.strftime("%H:%M").to_string()).unwrap_or_default(),
                        end.as_ref().map(|z| z.strftime("%H:%M").to_string()).unwrap_or_default(),
                    )
                };
                let rem = e.alarms.first().map(|a| (-a / 60).to_string()).unwrap_or_default();
                (
                    "Edit event",
                    Form::new("")
                        .field("Title", &e.summary, "")
                        .field("Date", &date, "")
                        .field("Start", &st, "empty for all day")
                        .field("End", &en, "a time, or a last day for all-day events")
                        .field("Repeat", &repeat_text(&e.rrule), "none, daily, weekdays, weekly, monthly, yearly")
                        .field("Location", e.location.as_deref().unwrap_or(""), "")
                        .field("Reminder", &rem, "minutes before; empty for none")
                        .field("Notes", &e.description.clone().unwrap_or_default().replace('\n', " / "), "")
                        .field("Calendar", &s.calendar, &cals.join(", ")),
                )
            }
            None => {
                let default_rem = cx.config.calendar.default_alarm_mins;
                (
                    "New event",
                    Form::new("")
                        .field("Title", "", "Lunch with Ada")
                        .field("Date", &self.day.to_string(), "2026-03-14, tomorrow, fri, +3")
                        .field("Start", "", "9:30, 2pm; empty for all day")
                        .field("End", "", "a time; empty for an hour")
                        .field("Repeat", "", "none, daily, weekdays, weekly, monthly, yearly")
                        .field("Location", "", "")
                        .field(
                            "Reminder",
                            &if default_rem > 0 { default_rem.to_string() } else { String::new() },
                            "minutes before; empty for none",
                        )
                        .field("Notes", "", "")
                        .field("Calendar", cals.last().map(String::as_str).unwrap_or("local"), &cals.join(", ")),
                )
            }
        };
        let mut form = form;
        form.title = title.into();
        self.editor = Some((form, which));
    }

    fn save_editor(&mut self, cx: &mut Cx) {
        let Some((mut form, which)) = self.editor.take() else { return };
        let cals = writable(&cx.config.calendar.accounts);
        let fail = |mut f: Form, msg: String, me: &mut Self| {
            f.note = Some(msg);
            me.editor = Some((f, which));
        };
        let title = form.get("Title");
        if title.is_empty() {
            return fail(form, "Give the event a title.".into(), self);
        }
        let Some(date) = parse_day(&form.get("Date"), self.today()) else {
            return fail(form, "Date: such as 2026-03-14, tomorrow or fri".into(), self);
        };
        let cal = form.get("Calendar");
        if !cals.contains(&cal) {
            return fail(form, format!("Calendar: one of {}", cals.join(", ")), self);
        }
        let start_t = form.get("Start");
        let end_t = form.get("End");
        let (start, end) = if start_t.is_empty() {
            let last = if end_t.is_empty() { Some(date) } else { parse_day(&end_t, date) };
            let Some(last) = last.filter(|l| *l >= date) else {
                return fail(form, "End: the last day, on or after the start".into(), self);
            };
            (When::Date(date), When::Date(last.tomorrow().unwrap_or(last)))
        } else {
            let Some(st) = parse_time(&start_t) else {
                return fail(form, "Start: a time such as 9:30 or 2pm".into(), self);
            };
            let et = if end_t.is_empty() {
                None
            } else {
                match parse_time(&end_t) {
                    Some(t) => Some(t),
                    None => return fail(form, "End: a time such as 10:30".into(), self),
                }
            };
            let sdt = date.to_datetime(st);
            let edt = match et {
                Some(t) if date.to_datetime(t) > sdt => date.to_datetime(t),
                Some(t) => date.tomorrow().unwrap_or(date).to_datetime(t),
                None => sdt.checked_add(1.hour()).unwrap_or(sdt),
            };
            match self.tz.iana_name() {
                Some(_) => match (sdt.to_zoned(self.tz.clone()), edt.to_zoned(self.tz.clone())) {
                    (Ok(s), Ok(e)) => (When::Zoned(s), When::Zoned(e)),
                    _ => return fail(form, "That time does not exist here".into(), self),
                },
                None => (When::Floating(sdt), When::Floating(edt)),
            }
        };
        let rrule = match repeat_rule(&form.get("Repeat"), date) {
            Ok(r) => r,
            Err(e) => return fail(form, e, self),
        };
        let alarms = match form.get("Reminder").trim_end_matches(" minutes").trim() {
            "" => vec![],
            m => match m.parse::<i64>() {
                Ok(n) if n >= 0 => vec![-n * 60],
                _ => return fail(form, "Reminder: minutes before, such as 10".into(), self),
            },
        };
        form.note = None;
        let old = which.and_then(|i| self.stored.get(i).cloned());
        let mut e = old.as_ref().map(|s| s.event.clone()).unwrap_or_default();
        if e.uid.is_empty() {
            e.uid = alouette_pim::contact::new_uid();
        } else {
            e.sequence += 1;
        }
        e.summary = title;
        e.start = Some(start);
        e.end = Some(end);
        e.duration = None;
        e.rrule = rrule;
        e.location = Some(form.get("Location")).filter(|s| !s.is_empty());
        e.description = Some(form.get("Notes").replace(" / ", "\n")).filter(|s| !s.is_empty());
        e.alarms = alarms;
        // Keep the overrides that share this event's file.
        let mut group: Vec<Event> = old
            .as_ref()
            .map(|o| {
                self.stored
                    .iter()
                    .filter(|s| {
                        s.file == o.file
                            && s.calendar == o.calendar
                            && s.event.uid == o.event.uid
                            && s.event.recurrence_id.is_some()
                    })
                    .map(|s| s.event.clone())
                    .collect()
            })
            .unwrap_or_default();
        group.insert(0, e.clone());
        if let Err(err) = alouette_pim::itip::save(&self.stores, &cal, &group) {
            self.note = Some(format!("Could not save: {err}"));
            return;
        }
        if let Some(o) = &old
            && o.calendar != cal
        {
            self.sync_named(&o.calendar.clone(), cx);
        }
        self.reload();
        self.day = date;
        self.sel = 0;
        if let Some(i) = self.selection_for(&e.uid) {
            self.sel = i;
        }
        self.note = Some(format!("Saved {}", e.summary));
        self.sync_named(&cal, cx);
    }

    fn selection_for(&self, uid: &str) -> Option<usize> {
        let list = if self.view == View::Agenda { self.agenda() } else { self.on_day(self.day) };
        list.iter().position(|i| self.events[i.idx].uid == uid)
    }

    fn delete(&mut self, inst: Inst, only_this: bool, cx: &mut Cx) {
        let Some(s) = self.stored.get(inst.idx).cloned() else { return };
        let folder = self.stores.calendar(&s.calendar);
        let res = if only_this && s.event.rrule.is_some() && s.event.recurrence_id.is_none() {
            let mut group: Vec<Event> = self
                .stored
                .iter()
                .filter(|x| x.file == s.file && x.calendar == s.calendar && x.event.uid == s.event.uid)
                .map(|x| x.event.clone())
                .collect();
            if let Some(m) = group.iter_mut().find(|e| e.recurrence_id.is_none()) {
                let ex = match &m.start {
                    Some(When::Date(_)) => When::Date(inst.start.date()),
                    Some(When::Floating(_)) => When::Floating(inst.start.datetime()),
                    Some(When::Zoned(z)) => When::Zoned(inst.start.with_time_zone(z.time_zone().clone())),
                    None => When::Date(inst.start.date()),
                };
                m.exdates.push(ex);
                m.sequence += 1;
            }
            let refs: Vec<&Event> = group.iter().collect();
            folder.put_local(&s.file, &Calendar::write(&refs, None))
        } else if s.event.recurrence_id.is_some() && only_this {
            // A changed occurrence: drop it and skip that date in the series.
            let mut group: Vec<Event> = self
                .stored
                .iter()
                .filter(|x| x.file == s.file && x.calendar == s.calendar && x.event.uid == s.event.uid)
                .map(|x| x.event.clone())
                .filter(|e| e.recurrence_id != s.event.recurrence_id)
                .collect();
            if let (Some(m), Some(r)) =
                (group.iter_mut().find(|e| e.recurrence_id.is_none()), s.event.recurrence_id.clone())
            {
                m.exdates.push(r);
            }
            let refs: Vec<&Event> = group.iter().collect();
            folder.put_local(&s.file, &Calendar::write(&refs, None))
        } else {
            folder.delete_local(&s.file)
        };
        match res {
            Ok(()) => {
                self.note = Some(format!("Deleted {}", s.event.summary));
                self.reload();
                self.sel = self.sel.saturating_sub(1);
                self.sync_named(&s.calendar, cx);
            }
            Err(e) => self.note = Some(format!("Could not delete: {e}")),
        }
    }

    fn event_label(&self, i: &Inst, width: usize) -> String {
        let e = &self.events[i.idx];
        let when = if i.all_day { "all day".to_string() } else { i.start.strftime("%H:%M").to_string() };
        let title = if e.summary.is_empty() { "(untitled)" } else { &e.summary };
        fit(&format!("{when:>7} {title}"), width)
    }

    fn style_for(&self, i: &Inst, cx: &DrawCx) -> Style {
        let e = &self.events[i.idx];
        let t = cx.theme;
        let mut st = Style::new();
        if e.status.as_deref() == Some("TENTATIVE") {
            st = st.add_modifier(Modifier::ITALIC);
        }
        if i.end < Zoned::now() {
            st = st.patch(t.dim());
        }
        st
    }

    fn draw_agenda(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let list = self.agenda();
        let today = self.today();
        let w = area.width as usize;
        let mut lines: Vec<Line> = vec![];
        let mut sel_line = 0;
        let mut last_day: Option<Date> = None;
        for (n, i) in list.iter().enumerate() {
            let d = i.start.date().max(self.day);
            if last_day != Some(d) {
                if last_day.is_some() {
                    lines.push(Line::default());
                }
                let rel = match (d - today).get_days() {
                    0 => " · today".to_string(),
                    1 => " · tomorrow".to_string(),
                    _ => String::new(),
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{rel}", d.strftime("%A %-d %B %Y")),
                    t.accent().add_modifier(Modifier::BOLD),
                )));
                last_day = Some(d);
            }
            if n == self.sel {
                sel_line = lines.len();
            }
            let mut label = self.event_label(i, w.saturating_sub(10));
            let e = &self.events[i.idx];
            if let Some(l) = &e.location {
                label = fit(&format!("{} · {l}", label.trim_end()), w.saturating_sub(10));
            }
            let cal = fit(&self.stored[i.idx].calendar, 9);
            let st = if n == self.sel && cx.focused { t.selection() } else { self.style_for(i, cx) };
            lines.push(Line::from(vec![Span::styled(label, st), Span::raw(" "), Span::styled(cal, t.dim())]));
        }
        if list.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Nothing in the {AGENDA_DAYS} days from {}. a adds an event.", self.day.strftime("%-d %B")),
                t.dim(),
            )));
        }
        let h = area.height as usize;
        if sel_line < self.scroll + 1 {
            self.scroll = sel_line.saturating_sub(1);
        } else if sel_line >= self.scroll + h {
            self.scroll = sel_line + 1 - h;
        }
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll as u16, 0)), area);
    }

    fn draw_week(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let first = self.week_of(self.day);
        let today = self.today();
        let cw = area.width / 7;
        self.areas.clear();
        for k in 0..7u16 {
            let Ok(d) = first.checked_add((k as i64).days()) else { continue };
            let r = Rect {
                x: area.x + k * cw,
                y: area.y,
                width: if k == 6 { area.width - 6 * cw } else { cw },
                height: area.height,
            };
            self.areas.push((r, d));
            let mut head = Style::new().add_modifier(Modifier::BOLD);
            if d == today {
                head = t.accent().add_modifier(Modifier::BOLD);
            }
            if d == self.day && cx.focused {
                head = head.add_modifier(Modifier::REVERSED);
            }
            let mut lines =
                vec![Line::from(Span::styled(fit(&d.strftime(" %a %-d").to_string(), r.width as usize), head))];
            for (n, i) in self.on_day(d).iter().enumerate() {
                let e = &self.events[i.idx];
                let when = if i.all_day { "·".to_string() } else { i.start.strftime("%H:%M").to_string() };
                let text = fit(&format!("{when} {}", e.summary), r.width.saturating_sub(1) as usize);
                let st =
                    if d == self.day && n == self.sel && cx.focused { t.selection() } else { self.style_for(i, cx) };
                lines.push(Line::from(Span::styled(text, st)));
            }
            frame.render_widget(Paragraph::new(lines), r);
        }
    }

    fn draw_month(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let first = self.day.first_of_month();
        let grid = self.week_of(first);
        let today = self.today();
        let cw = area.width / 7;
        let names: Vec<String> = (0..7)
            .filter_map(|k| grid.checked_add((k as i64).days()).ok())
            .map(|d| d.strftime("%a").to_string())
            .collect();
        let header: Vec<Span> = names
            .iter()
            .map(|n| Span::styled(fit(&format!(" {n}"), cw as usize), Style::new().add_modifier(Modifier::BOLD)))
            .collect();
        frame.render_widget(Paragraph::new(Line::from(header)), Rect { height: 1, ..area });
        let rows = 6u16;
        let ch = (area.height.saturating_sub(1)) / rows;
        if ch == 0 {
            return;
        }
        self.areas.clear();
        for wk in 0..rows {
            for k in 0..7u16 {
                let Ok(d) = grid.checked_add(((wk * 7 + k) as i64).days()) else { continue };
                let r = Rect {
                    x: area.x + k * cw,
                    y: area.y + 1 + wk * ch,
                    width: if k == 6 { area.width - 6 * cw } else { cw },
                    height: ch,
                };
                self.areas.push((r, d));
                let mut head = if d.month() == first.month() { Style::new() } else { t.dim() };
                if d == today {
                    head = t.accent().add_modifier(Modifier::BOLD);
                }
                if d == self.day && cx.focused {
                    head = head.add_modifier(Modifier::REVERSED);
                }
                let mut lines = vec![Line::from(Span::styled(format!("{:>2}", d.day()), head))];
                let evs = self.on_day(d);
                let room = ch.saturating_sub(1) as usize;
                for (n, i) in evs.iter().enumerate().take(room) {
                    if n + 1 == room && evs.len() > room {
                        lines.push(Line::from(Span::styled(format!("+{} more", evs.len() - n), t.dim())));
                        break;
                    }
                    let e = &self.events[i.idx];
                    let st = if d == self.day && n == self.sel && cx.focused {
                        t.selection()
                    } else {
                        self.style_for(i, cx)
                    };
                    lines.push(Line::from(Span::styled(fit(&e.summary, r.width.saturating_sub(1) as usize), st)));
                }
                frame.render_widget(Paragraph::new(lines), r);
            }
        }
    }

    fn detail_lines(&self, i: &Inst, cx: &DrawCx) -> Vec<Line<'static>> {
        let t = cx.theme;
        let e = &self.events[i.idx];
        let s = &self.stored[i.idx];
        let mut out = vec![Line::from(Span::styled(
            if e.summary.is_empty() { "(untitled)".to_string() } else { e.summary.clone() },
            t.accent().add_modifier(Modifier::BOLD),
        ))];
        let when = if i.all_day {
            let last = i.end.date().yesterday().unwrap_or(i.end.date());
            if last == i.start.date() {
                format!("{}, all day", i.start.strftime("%A %-d %B"))
            } else {
                format!("{} to {}", i.start.strftime("%A %-d %B"), last.strftime("%A %-d %B"))
            }
        } else if i.start.date() == i.end.date() {
            format!("{} to {}", i.start.strftime("%A %-d %B, %H:%M"), i.end.strftime("%H:%M"))
        } else {
            format!("{} to {}", i.start.strftime("%A %-d %B, %H:%M"), i.end.strftime("%A %-d %B, %H:%M"))
        };
        let field = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:>9}  "), t.dim()), Span::raw(v)]);
        out.push(field("When", when));
        if let Some(r) = &e.rrule {
            out.push(field("Repeats", r.describe()));
        }
        if let Some(l) = &e.location {
            out.push(field("Where", l.clone()));
        }
        if let Some(o) = &e.organizer {
            out.push(field("Organiser", o.label()));
        }
        if !e.attendees.is_empty() {
            let who: Vec<String> = e
                .attendees
                .iter()
                .map(|a| {
                    let n = a.name.clone().unwrap_or_else(|| a.email.clone());
                    match a.partstat.as_str() {
                        "ACCEPTED" => format!("{n} ✓"),
                        "DECLINED" => format!("{n} ✗"),
                        "TENTATIVE" => format!("{n} ?"),
                        _ => n,
                    }
                })
                .collect();
            out.push(field("Guests", who.join(", ")));
        }
        if !e.alarms.is_empty() {
            let r: Vec<String> = e.alarms.iter().map(|a| say_minutes(-a / 60) + " before").collect();
            out.push(field("Reminder", r.join(", ")));
        }
        out.push(field("Calendar", s.calendar.clone()));
        if let Some(d) = &e.description {
            for l in d.lines().take(6) {
                out.push(Line::from(l.to_string()));
            }
        }
        out
    }
}

impl Window for CalendarWindow {
    fn title(&self) -> String {
        match self.view {
            View::Agenda => "Calendar".into(),
            View::Week => format!("Calendar: week of {}", self.week_of(self.day).strftime("%-d %B")),
            View::Month => format!("Calendar: {}", self.day.strftime("%B %Y")),
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some("calendar:".into())
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.editor.is_some() {
            return vec![("Tab", "next field"), ("Ctrl+S", "save"), ("Esc", "cancel")];
        }
        vec![
            ("v", "view"),
            ("←→", "day"),
            ("t", "today"),
            ("a", "add"),
            ("e", "edit"),
            ("d", "delete"),
            ("s", "follow"),
            ("g", "sync"),
        ]
    }
    fn wants_text(&self) -> bool {
        self.prompt.is_some() || self.editor.is_some() || self.asker.question().is_some()
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        if payload.downcast_ref::<alouette_pim::Changed>().is_some() {
            self.reload();
            return;
        }
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match &m.kind {
            Kind::Synced { name, result } => {
                self.syncing = self.syncing.saturating_sub(1);
                match result {
                    Ok(s) if s != "up to date" => self.note = Some(format!("{name}: {s}")),
                    Ok(_) => {}
                    Err(e) => {
                        self.note = Some(format!("{name}: {e}"));
                        cx.notify(Notification::warning(format!("Calendar, {name}: {e}")));
                    }
                }
                self.reload();
            }
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
        if let Some(Prompt::Subscribe(i) | Prompt::Import(i) | Prompt::GoTo(i)) = &mut self.prompt {
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
            let input = match p {
                Prompt::Subscribe(i) | Prompt::Import(i) | Prompt::GoTo(i) => i,
            };
            match input.on_key(key) {
                InputResult::Submit => {
                    let v = input.value.trim().to_string();
                    match self.prompt.take() {
                        Some(Prompt::Subscribe(_)) if !v.is_empty() => self.subscribe(v, cx),
                        Some(Prompt::Import(_)) if !v.is_empty() => self.import(&v, cx),
                        Some(Prompt::GoTo(_)) => match parse_day(&v, self.today()) {
                            Some(d) => {
                                self.day = d;
                                self.sel = 0;
                                self.scroll = 0;
                            }
                            None => self.note = Some(format!("{v} is not a day Alouette understands")),
                        },
                        _ => {}
                    }
                }
                InputResult::Cancel => self.prompt = None,
                _ => {}
            }
            return Handled::Yes;
        }
        if let Some(inst) = self.confirm.take() {
            match key.code {
                KeyCode::Char('y' | 'Y') => self.delete(inst, false, cx),
                KeyCode::Char('o' | 'O') => self.delete(inst, true, cx),
                _ => self.note = Some("Kept".into()),
            }
            return Handled::Yes;
        }
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return Handled::No;
        }
        let count = match self.view {
            View::Agenda => self.agenda().len(),
            _ => self.on_day(self.day).len(),
        };
        match key.code {
            KeyCode::Char('v') => {
                self.view = match self.view {
                    View::Agenda => View::Week,
                    View::Week => View::Month,
                    View::Month => View::Agenda,
                };
                self.sel = 0;
                self.scroll = 0;
            }
            KeyCode::Char('1') => self.view = View::Agenda,
            KeyCode::Char('2') => self.view = View::Week,
            KeyCode::Char('3') => self.view = View::Month,
            KeyCode::Char('t') => {
                self.day = self.today();
                self.sel = 0;
                self.scroll = 0;
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_day(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_day(1),
            KeyCode::Up | KeyCode::Char('k') => {
                if self.view == View::Month && self.sel == 0 {
                    self.move_day(-7);
                } else {
                    self.sel = self.sel.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.view == View::Month && self.sel + 1 >= count {
                    self.move_day(7);
                } else {
                    self.sel = (self.sel + 1).min(count.saturating_sub(1));
                }
            }
            KeyCode::PageUp => match self.view {
                View::Month => {
                    self.day = self.day.checked_sub(1.month()).unwrap_or(self.day);
                    self.sel = 0;
                }
                _ => self.move_day(-7),
            },
            KeyCode::PageDown => match self.view {
                View::Month => {
                    self.day = self.day.checked_add(1.month()).unwrap_or(self.day);
                    self.sel = 0;
                }
                _ => self.move_day(7),
            },
            KeyCode::Enter => {
                if self.view != View::Agenda {
                    self.view = View::Agenda;
                    self.sel = 0;
                    self.scroll = 0;
                } else if let Some(i) = self.selected()
                    && let Some(u) = self.events[i.idx].url.clone()
                {
                    cx.open_url(u);
                }
            }
            KeyCode::Char('a') | KeyCode::Char('n') => self.edit(None, cx),
            KeyCode::Char('e') => {
                if let Some(i) = self.selected() {
                    let s = &self.stored[i.idx];
                    let subs: Vec<&str> = cx.config.calendar.subscriptions.iter().map(|s| s.name.as_str()).collect();
                    if subs.contains(&s.calendar.as_str()) {
                        self.note = Some(format!("{} is a followed calendar; it cannot be changed here", s.calendar));
                    } else {
                        // Edit the series, not one changed occurrence.
                        let master = self
                            .stored
                            .iter()
                            .position(|x| {
                                x.event.uid == s.event.uid && x.file == s.file && x.event.recurrence_id.is_none()
                            })
                            .unwrap_or(i.idx);
                        self.edit(Some(master), cx);
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(i) = self.selected() {
                    let e = &self.events[i.idx];
                    let s = &self.stored[i.idx];
                    if cx.config.calendar.subscriptions.iter().any(|x| x.name == s.calendar) {
                        self.note = Some(format!("{} is a followed calendar; it cannot be changed here", s.calendar));
                    } else {
                        self.note = Some(if e.rrule.is_some() || e.recurrence_id.is_some() {
                            format!("Delete {}? y every time, o only this one, any other key keeps it.", e.summary)
                        } else {
                            format!("Delete {}? y deletes, any other key keeps it.", e.summary)
                        });
                        self.confirm = Some(i);
                    }
                }
            }
            KeyCode::Char('s') => self.prompt = Some(Prompt::Subscribe(LineInput::new(""))),
            KeyCode::Char('i') => self.prompt = Some(Prompt::Import(LineInput::new("~/"))),
            KeyCode::Char('o') | KeyCode::Char('/') => self.prompt = Some(Prompt::GoTo(LineInput::new(""))),
            KeyCode::Char('g') => {
                if cx.config.calendar.accounts.is_empty() && cx.config.calendar.subscriptions.is_empty() {
                    self.note =
                        Some("No CalDAV calendars or followed calendars; events here stay on this computer.".into());
                } else {
                    self.note = Some("Syncing…".into());
                    self.sync_all(cx);
                }
                self.reload();
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, d)) = self
                    .areas
                    .iter()
                    .find(|(r, _)| ev.column >= r.x && ev.column < r.right() && ev.row >= r.y && ev.row < r.bottom())
                {
                    self.day = *d;
                    self.sel = 0;
                }
                Handled::Yes
            }
            MouseEventKind::ScrollUp => {
                self.sel = self.sel.saturating_sub(1);
                Handled::Yes
            }
            MouseEventKind::ScrollDown => {
                self.sel += 1;
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 6 {
            return;
        }
        let detail_h = if area.height > 18 { 7 } else { 3 };
        let main = Rect { height: area.height - detail_h - 1, ..area };
        let detail = Rect { y: main.bottom(), height: detail_h, ..area };
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        if self.view != View::Agenda {
            self.areas.clear();
        }
        match self.view {
            View::Agenda => self.draw_agenda(frame, main, cx),
            View::Week => self.draw_week(frame, main, cx),
            View::Month => self.draw_month(frame, main, cx),
        }
        let block = Block::default().borders(Borders::TOP).border_style(t.dim());
        let inner = block.inner(detail);
        frame.render_widget(block, detail);
        if let Some(i) = self.selected() {
            frame.render_widget(Paragraph::new(self.detail_lines(&i, cx)).wrap(Wrap { trim: false }), inner);
        } else {
            let msg = format!("{}: nothing planned.", self.day.strftime("%A %-d %B"));
            frame.render_widget(Paragraph::new(Span::styled(msg, t.dim())), inner);
        }
        let status = match &self.prompt {
            Some(Prompt::Subscribe(i)) => format!("Follow the calendar at: {}", i.value),
            Some(Prompt::Import(i)) => format!("Import events from (.ics): {}", i.value),
            Some(Prompt::GoTo(i)) => format!("Go to the day: {}", i.value),
            None => {
                let mut s = self.note.clone().unwrap_or_default();
                if s.is_empty() && self.syncing > 0 {
                    s = "Syncing…".into();
                }
                s
            }
        };
        frame.render_widget(
            Paragraph::new(Span::styled(fit(&format!(" {status}"), foot.width as usize), t.dim())),
            foot,
        );
        if let Some(Prompt::Subscribe(i) | Prompt::Import(i) | Prompt::GoTo(i)) = &self.prompt {
            let label = status.chars().count() - i.value.chars().count();
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
    fn repeats_and_names() {
        let d = jiff::civil::date(2026, 1, 1);
        assert_eq!(repeat_rule("weekdays", d).unwrap().unwrap().to_value(), "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR");
        assert_eq!(repeat_text(&repeat_rule("weekdays", d).unwrap()), "weekdays");
        assert_eq!(repeat_text(&repeat_rule("FREQ=MONTHLY;BYDAY=-1FR", d).unwrap()), "FREQ=MONTHLY;BYDAY=-1FR");
        assert!(repeat_rule("now and then", d).is_err());
        assert!(repeat_rule("", d).unwrap().is_none());
        assert_eq!(sub_name("webcal://www.example.com/cal/holidays.ics"), "example.com holidays");
    }
}
