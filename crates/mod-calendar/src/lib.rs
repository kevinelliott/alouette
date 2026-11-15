//! Calendar: agenda, week and month views over local calendars, CalDAV calendars and ICS
//! subscriptions, with reminders while Alouette runs.
//!
//! URLs:
//! - `calendar:` the default view
//! - `calendar:week`, `calendar:month`, `calendar:agenda` a view
//! - `calendar:2026-03-14` the agenda from that day
//! - `webcal://example.com/holidays.ics` follow a calendar

pub mod window;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alouette_core::{AppEvent, Notification};
use alouette_pim::event::{Calendar, Event};
use alouette_pim::store::Stores;
use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use url::Url;

pub const MODULE: &str = "calendar";

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One event as stored: which calendar, which file.
#[derive(Debug, Clone)]
pub struct Stored {
    pub calendar: String,
    pub file: String,
    pub event: Event,
}

/// Every event in every calendar folder.
pub fn load_all(stores: &Stores) -> Vec<Stored> {
    let mut out = vec![];
    for cal in stores.calendars() {
        for item in stores.calendar(&cal).list() {
            for e in Calendar::parse(&item.text).events {
                out.push(Stored { calendar: cal.clone(), file: item.file.clone(), event: e });
            }
        }
    }
    out
}

/// A day typed by a person: `2026-03-14`, `today`, `tomorrow`, `+3`, `fri`, `14/3` or
/// `3/14` (read day first when the first number is over 12).
pub fn parse_day(s: &str, today: Date) -> Option<Date> {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "" | "today" => return Some(today),
        "tomorrow" => return today.tomorrow().ok(),
        "yesterday" => return today.yesterday().ok(),
        _ => {}
    }
    if let Some(n) = s.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
        return today.checked_add(n.days()).ok();
    }
    if let Ok(d) = s.parse::<Date>() {
        return Some(d);
    }
    let days = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    if let Some(i) = days.iter().position(|d| s.starts_with(d)) {
        let want = i as i8;
        let now = today.weekday().to_monday_zero_offset();
        let ahead = (want - now).rem_euclid(7);
        let ahead = if ahead == 0 { 7 } else { ahead };
        return today.checked_add((ahead as i64).days()).ok();
    }
    let parts: Vec<&str> = s.split(['/', '.']).collect();
    if parts.len() >= 2 {
        let a: i8 = parts[0].parse().ok()?;
        let b: i8 = parts[1].parse().ok()?;
        let y: i16 = parts
            .get(2)
            .and_then(|y| y.parse().ok())
            .map(|y: i16| if y < 100 { 2000 + y } else { y })
            .unwrap_or(today.year());
        let (d, m) = if a > 12 { (a, b) } else { (b, a) };
        return Date::new(y, m, d).ok();
    }
    None
}

/// A time typed by a person: `9`, `9:30`, `0930`, `2pm`, `14:00`.
pub fn parse_time(s: &str) -> Option<Time> {
    let s = s.trim().to_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    let (s, pm, am) = if let Some(x) = s.strip_suffix("pm") {
        (x.to_string(), true, false)
    } else if let Some(x) = s.strip_suffix("am") {
        (x.to_string(), false, true)
    } else {
        (s, false, false)
    };
    let (h, m) = match s.split_once([':', '.', 'h']) {
        Some((h, m)) => (h.parse::<i8>().ok()?, if m.is_empty() { 0 } else { m.parse::<i8>().ok()? }),
        None if s.len() > 2 => (s[..s.len() - 2].parse().ok()?, s[s.len() - 2..].parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    let h = match (h, pm, am) {
        (12, false, true) => 0,
        (h, true, _) if h < 12 => h + 12,
        (h, _, _) => h,
    };
    Time::new(h, m, 0, 0).ok()
}

/// Minutes as people say them: "10 minutes", "1 hour", "2 days".
pub fn say_minutes(m: i64) -> String {
    match m {
        0 => "at the start".into(),
        m if m % 1440 == 0 => format!("{} day{}", m / 1440, if m == 1440 { "" } else { "s" }),
        m if m % 60 == 0 => format!("{} hour{}", m / 60, if m == 60 { "" } else { "s" }),
        m => format!("{m} minute{}", if m == 1 { "" } else { "s" }),
    }
}

pub struct CalendarModule {
    data_dir: PathBuf,
    reminders: Arc<AtomicBool>,
    started: bool,
    view: Arc<Mutex<String>>,
}

impl CalendarModule {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            reminders: Arc::new(AtomicBool::new(true)),
            started: false,
            view: Arc::new(Mutex::new("agenda".into())),
        }
    }
}

/// Check for reminders due between `after` and `now`, as notifications.
pub fn due_reminders(data_dir: &Path, after: &Zoned, now: &Zoned, tz: &TimeZone) -> Vec<String> {
    let stores = Stores::new(data_dir);
    let mut out = vec![];
    for s in load_all(&stores) {
        let e = &s.event;
        if e.recurrence_id.is_some() {
            // Overrides carry their own alarms; checked like any event.
        }
        if let Some((at, start)) = e.next_alarm(after, now, tz) {
            let mins = (start.timestamp().as_second() - at.timestamp().as_second()) / 60;
            let when = if e.all_day() { start.strftime("%A").to_string() } else { start.strftime("%H:%M").to_string() };
            let lead = if mins <= 0 { "now".to_string() } else { format!("in {}", say_minutes(mins)) };
            let place = e.location.as_deref().map(|l| format!(", {l}")).unwrap_or_default();
            out.push(format!("{} at {when}{place} ({lead})", if e.summary.is_empty() { "Event" } else { &e.summary }));
        }
    }
    out
}

impl Module for CalendarModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Calendar"
    }
    fn description(&self) -> &'static str {
        "Agenda, week and month views; CalDAV, ICS subscriptions and reminders"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["calendar", "webcal", "webcals"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("Calendar", "calendar:", "Your agenda; a adds an event, v changes the view"),
            Launcher::new("Calendar: month", "calendar:month", "The month at a glance"),
        ]
    }

    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        let mut open = window::Open { view: cx.config.calendar.view.clone(), ..Default::default() };
        match url.scheme() {
            "webcal" | "webcals" => open.subscribe = Some(url.to_string()),
            _ => {
                let p = url.path().trim_matches('/');
                match p {
                    "" => {}
                    "agenda" | "week" | "month" => open.view = p.to_string(),
                    d => open.day = d.parse().ok(),
                }
            }
        }
        if let Ok(mut v) = self.view.lock() {
            v.clone_from(&open.view);
        }
        Ok(Box::new(window::CalendarWindow::new(self.data_dir.clone(), open, cx)))
    }

    fn start(&mut self, cx: &mut Cx) {
        if self.started {
            return;
        }
        self.started = true;
        self.reminders.store(cx.config.calendar.reminders, Ordering::Relaxed);
        let (on, dir, events) = (self.reminders.clone(), self.data_dir.clone(), cx.tasks.events());
        tokio::spawn(async move {
            let tz = TimeZone::system();
            let mut last = Zoned::now().with_time_zone(tz.clone());
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                let now = Zoned::now().with_time_zone(tz.clone());
                if on.load(Ordering::Relaxed) {
                    let (d, a, n, t) = (dir.clone(), last.clone(), now.clone(), tz.clone());
                    let due =
                        tokio::task::spawn_blocking(move || due_reminders(&d, &a, &n, &t)).await.unwrap_or_default();
                    for r in due {
                        let _ = events.send(AppEvent::Notify(Notification::info(format!("Reminder: {r}"))));
                    }
                }
                last = now;
            }
        });
    }

    fn on_event(&mut self, ev: &AppEvent, _cx: &mut Cx) {
        if let AppEvent::ConfigReloaded(c) = ev {
            self.reminders.store(c.calendar.reminders, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn typed_days_and_times() {
        let today = date(2026, 3, 11); // a Wednesday
        assert_eq!(parse_day("tomorrow", today), Some(date(2026, 3, 12)));
        assert_eq!(parse_day("fri", today), Some(date(2026, 3, 13)));
        assert_eq!(parse_day("wed", today), Some(date(2026, 3, 18)));
        assert_eq!(parse_day("+7", today), Some(date(2026, 3, 18)));
        assert_eq!(parse_day("2026-04-01", today), Some(date(2026, 4, 1)));
        assert_eq!(parse_day("25/12", today), Some(date(2026, 12, 25)));
        assert_eq!(parse_day("12/25/27", today), Some(date(2027, 12, 25)));
        assert_eq!(parse_day("nonsense", today), None);
        assert_eq!(parse_time("9"), Time::new(9, 0, 0, 0).ok());
        assert_eq!(parse_time("2pm"), Time::new(14, 0, 0, 0).ok());
        assert_eq!(parse_time("12am"), Time::new(0, 0, 0, 0).ok());
        assert_eq!(parse_time("0930"), Time::new(9, 30, 0, 0).ok());
        assert_eq!(parse_time("14:05"), Time::new(14, 5, 0, 0).ok());
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(say_minutes(60), "1 hour");
        assert_eq!(say_minutes(10), "10 minutes");
    }

    #[test]
    fn reminders_fire_once() {
        let dir = std::env::temp_dir().join(format!("alouette-cal-rem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stores = Stores::new(&dir);
        let tz = TimeZone::get("Europe/London").unwrap();
        let start = date(2026, 6, 1).at(9, 0, 0, 0).to_zoned(tz.clone()).unwrap();
        let mut e = Event::new(
            "Standup",
            alouette_pim::When::Zoned(start.clone()),
            alouette_pim::When::Zoned(start.checked_add(15.minutes()).unwrap()),
        );
        e.alarms = vec![-600];
        alouette_pim::itip::save(&stores, "local", &[e]).unwrap();
        let t = |h, m| date(2026, 6, 1).at(h, m, 0, 0).to_zoned(tz.clone()).unwrap();
        assert!(due_reminders(&dir, &t(8, 40), &t(8, 49), &tz).is_empty());
        let due = due_reminders(&dir, &t(8, 49), &t(8, 51), &tz);
        assert_eq!(due, ["Standup at 09:00 (in 10 minutes)"]);
        assert!(due_reminders(&dir, &t(8, 51), &t(8, 53), &tz).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
