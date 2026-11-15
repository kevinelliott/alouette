//! Calendar events (iCalendar VEVENT), their times and their occurrences.

use std::collections::HashMap;

use jiff::civil::{Date, DateTime};
use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp, ToSpan, Zoned};

use crate::line::{self, Component, Prop};
use crate::rrule::RRule;

/// A start or end time as iCalendar has them.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    /// A whole day.
    Date(Date),
    /// A time in a named zone, or UTC.
    Zoned(Zoned),
    /// The same wall-clock time wherever you are.
    Floating(DateTime),
}

/// Zones named by Windows (Outlook and Exchange invites) mapped to IANA names.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Mountain Standard Time", "America/Denver"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Central Standard Time", "America/Chicago"),
    ("Eastern Standard Time", "America/New_York"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("India Standard Time", "Asia/Kolkata"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("UTC", "UTC"),
    ("Coordinated Universal Time", "UTC"),
];

/// Zone names a file defined in its own VTIMEZONE blocks, mapped to a fixed offset (the
/// standard one). Used only when the name is not one the zone database knows.
pub type ZoneHints = HashMap<String, Offset>;

pub fn zone(tzid: &str, hints: &ZoneHints) -> TimeZone {
    let id = tzid.trim().trim_start_matches('/');
    if let Ok(tz) = TimeZone::get(id) {
        return tz;
    }
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(id))
        && let Ok(tz) = TimeZone::get(iana)
    {
        return tz;
    }
    // "(UTC-05:00) Eastern Time (US & Canada)" and other labels.
    for (w, iana) in WINDOWS_ZONES {
        let stem = w.trim_end_matches(" Standard Time");
        if id.contains(stem)
            && let Ok(tz) = TimeZone::get(iana)
        {
            return tz;
        }
    }
    if let Some(off) = hints.get(id) {
        return TimeZone::fixed(*off);
    }
    TimeZone::system()
}

fn parse_offset(s: &str) -> Option<Offset> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    let h: i32 = rest.get(0..2)?.parse().ok()?;
    let m: i32 = rest.get(2..4).unwrap_or("00").parse().ok()?;
    let sec: i32 = rest.get(4..6).unwrap_or("00").parse().unwrap_or(0);
    Offset::from_seconds(sign * (h * 3600 + m * 60 + sec)).ok()
}

/// Fixed offsets for the VTIMEZONE blocks in a VCALENDAR.
pub fn zone_hints(cal: &Component) -> ZoneHints {
    let mut out = HashMap::new();
    for tz in cal.child("VTIMEZONE") {
        let Some(id) = tz.text("TZID") else { continue };
        let std = tz.child("STANDARD").next().or_else(|| tz.child("DAYLIGHT").next());
        if let Some(off) = std.and_then(|s| s.text("TZOFFSETTO")).and_then(|o| parse_offset(&o)) {
            out.insert(id, off);
        }
    }
    out
}

fn parse_date(s: &str) -> Option<Date> {
    let s = s.trim();
    if s.len() < 8 {
        return None;
    }
    Date::new(s[0..4].parse().ok()?, s[4..6].parse().ok()?, s[6..8].parse().ok()?).ok()
}

/// `20260105T090000` (and `Z`): the civil time, and whether it was UTC.
pub fn parse_datetime(s: &str) -> Option<(DateTime, bool)> {
    let s = s.trim();
    let d = parse_date(s)?;
    let t = s.get(9..)?;
    let utc = t.ends_with('Z') || t.ends_with('z');
    let t = t.trim_end_matches(['Z', 'z']);
    let h: i8 = t.get(0..2)?.parse().ok()?;
    let m: i8 = t.get(2..4)?.parse().ok()?;
    let sec: i8 = t.get(4..6).unwrap_or("00").parse().ok()?;
    Some((d.at(h, m, sec.min(59), 0), utc))
}

impl When {
    pub fn parse(p: &Prop, hints: &ZoneHints) -> Option<When> {
        let v = p.value.trim();
        let is_date =
            p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && !v.contains('T'));
        if is_date {
            return parse_date(v).map(When::Date);
        }
        let (dt, utc) = parse_datetime(v)?;
        if utc {
            return dt.to_zoned(TimeZone::UTC).ok().map(When::Zoned);
        }
        match p.param("TZID") {
            Some(id) => dt.to_zoned(zone(id, hints)).ok().map(When::Zoned),
            None => Some(When::Floating(dt)),
        }
    }

    /// The property, with TZID and VALUE=DATE as needed.
    pub fn to_prop(&self, name: &str) -> Prop {
        match self {
            When::Date(d) => Prop::new(name, d.strftime("%Y%m%d").to_string()).with_param("VALUE", "DATE"),
            When::Floating(dt) => Prop::new(name, dt.strftime("%Y%m%dT%H%M%S").to_string()),
            When::Zoned(z) => match z.time_zone().iana_name() {
                Some("UTC") | Some("Etc/UTC") | None => {
                    Prop::new(name, z.timestamp().strftime("%Y%m%dT%H%M%SZ").to_string())
                }
                Some(id) => Prop::new(name, z.datetime().strftime("%Y%m%dT%H%M%S").to_string()).with_param("TZID", id),
            },
        }
    }

    pub fn is_date(&self) -> bool {
        matches!(self, When::Date(_))
    }

    /// The moment this starts, seen from `tz` (a date starts at midnight there).
    pub fn in_zone(&self, tz: &TimeZone) -> Zoned {
        match self {
            When::Date(d) => d.to_zoned(tz.clone()).unwrap_or_else(|_| Zoned::now().with_time_zone(tz.clone())),
            When::Floating(dt) => dt.to_zoned(tz.clone()).unwrap_or_else(|_| Zoned::now().with_time_zone(tz.clone())),
            When::Zoned(z) => z.with_time_zone(tz.clone()),
        }
    }

    /// The wall-clock time in the event's own zone, and that zone.
    fn civil(&self, viewer: &TimeZone) -> (DateTime, TimeZone) {
        match self {
            When::Date(d) => (d.to_datetime(jiff::civil::Time::midnight()), viewer.clone()),
            When::Floating(dt) => (*dt, viewer.clone()),
            When::Zoned(z) => (z.datetime(), z.time_zone().clone()),
        }
    }

    pub fn date(&self, tz: &TimeZone) -> Date {
        match self {
            When::Date(d) => *d,
            w => w.in_zone(tz).date(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Person {
    pub email: String,
    pub name: Option<String>,
    /// NEEDS-ACTION, ACCEPTED, DECLINED, TENTATIVE or DELEGATED.
    pub partstat: String,
    pub role: String,
    pub rsvp: bool,
}

impl Person {
    fn parse(p: &Prop) -> Person {
        let v = p.value.trim();
        let email = if v.len() >= 7 && v[..7].eq_ignore_ascii_case("mailto:") { &v[7..] } else { v };
        Person {
            email: email.to_string(),
            name: p.param("CN").map(str::to_string).filter(|s| !s.is_empty()),
            partstat: p.param("PARTSTAT").unwrap_or("NEEDS-ACTION").to_ascii_uppercase(),
            role: p.param("ROLE").unwrap_or("REQ-PARTICIPANT").to_ascii_uppercase(),
            rsvp: p.param("RSVP").is_some_and(|r| r.eq_ignore_ascii_case("TRUE")),
        }
    }

    pub fn to_prop(&self, name: &str, attendee: bool) -> Prop {
        let mut p = Prop::new(name, format!("mailto:{}", self.email));
        if let Some(n) = &self.name {
            p = p.with_param("CN", n);
        }
        if attendee {
            p = p.with_param("PARTSTAT", &self.partstat).with_param("ROLE", &self.role);
            if self.rsvp {
                p = p.with_param("RSVP", "TRUE");
            }
        }
        p
    }

    pub fn label(&self) -> String {
        match &self.name {
            Some(n) => format!("{n} <{}>", self.email),
            None => self.email.clone(),
        }
    }
}

/// `-PT15M`, `P1D`, `P1W`, `-P1DT2H` as seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            u => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (u, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

pub fn format_duration(secs: i64) -> String {
    let neg = secs < 0;
    let mut s = secs.abs();
    let mut out = String::from(if neg { "-P" } else { "P" });
    if s == 0 {
        return "PT0S".into();
    }
    if s % (7 * 86400) == 0 {
        return format!("{out}{}W", s / (7 * 86400));
    }
    if s >= 86400 {
        out.push_str(&format!("{}D", s / 86400));
        s %= 86400;
    }
    if s > 0 {
        out.push('T');
        if s >= 3600 {
            out.push_str(&format!("{}H", s / 3600));
            s %= 3600;
        }
        if s >= 60 {
            out.push_str(&format!("{}M", s / 60));
            s %= 60;
        }
        if s > 0 {
            out.push_str(&format!("{s}S"));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<When>,
    pub end: Option<When>,
    /// DURATION in seconds, when the event gives that instead of an end.
    pub duration: Option<i64>,
    pub rrule: Option<RRule>,
    pub exdates: Vec<When>,
    pub rdates: Vec<When>,
    /// Set on an event that changes one occurrence of a recurring one.
    pub recurrence_id: Option<When>,
    /// TENTATIVE, CONFIRMED or CANCELLED.
    pub status: Option<String>,
    pub organizer: Option<Person>,
    pub attendees: Vec<Person>,
    /// Reminders, as seconds from the start (negative is before).
    pub alarms: Vec<i64>,
    pub sequence: i64,
    pub url: Option<String>,
    pub categories: Vec<String>,
    pub raw: Component,
}

const KNOWN: &[&str] = &[
    "UID",
    "SUMMARY",
    "LOCATION",
    "DESCRIPTION",
    "DTSTART",
    "DTEND",
    "DURATION",
    "RRULE",
    "EXDATE",
    "RDATE",
    "RECURRENCE-ID",
    "STATUS",
    "ORGANIZER",
    "ATTENDEE",
    "SEQUENCE",
    "URL",
    "CATEGORIES",
    "DTSTAMP",
    "LAST-MODIFIED",
];

/// One occurrence of an event.
#[derive(Debug, Clone)]
pub struct Instance<'a> {
    pub event: &'a Event,
    pub start: Zoned,
    pub end: Zoned,
    pub all_day: bool,
}

impl Event {
    pub fn new(summary: &str, start: When, end: When) -> Self {
        Event {
            uid: crate::contact::new_uid(),
            summary: summary.to_string(),
            start: Some(start),
            end: Some(end),
            ..Default::default()
        }
    }

    pub fn from_component(v: &Component, hints: &ZoneHints) -> Event {
        let when = |n: &str| v.get(n).and_then(|p| When::parse(p, hints));
        let list = |n: &str| -> Vec<When> {
            v.all(n)
                .flat_map(|p| {
                    p.value
                        .split(',')
                        .filter_map(|x| When::parse(&Prop { value: x.to_string(), ..p.clone() }, hints))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        Event {
            uid: v.text("UID").unwrap_or_default(),
            summary: v.text("SUMMARY").unwrap_or_default(),
            location: v.text("LOCATION"),
            description: v.text("DESCRIPTION"),
            start: when("DTSTART"),
            end: when("DTEND"),
            duration: v.get("DURATION").and_then(|p| parse_duration(&p.value)),
            rrule: v.get("RRULE").and_then(|p| RRule::parse(&p.value)),
            exdates: list("EXDATE"),
            rdates: list("RDATE"),
            recurrence_id: when("RECURRENCE-ID"),
            status: v.text("STATUS").map(|s| s.to_ascii_uppercase()),
            organizer: v.get("ORGANIZER").map(Person::parse),
            attendees: v.all("ATTENDEE").map(Person::parse).collect(),
            alarms: v
                .child("VALARM")
                .filter_map(|a| {
                    let t = a.get("TRIGGER")?;
                    if t.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE-TIME")) {
                        return None;
                    }
                    parse_duration(&t.value)
                })
                .collect(),
            sequence: v.text("SEQUENCE").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
            url: v.text("URL"),
            categories: v
                .all("CATEGORIES")
                .flat_map(|p| line::split_structured(&p.value, ','))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            raw: v.clone(),
        }
    }

    pub fn to_component(&self) -> Component {
        let mut v = Component::new("VEVENT");
        v.props.push(Prop::new("UID", self.uid.clone()));
        v.props.push(Prop::new("DTSTAMP", Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string()));
        if let Some(s) = &self.start {
            v.props.push(s.to_prop("DTSTART"));
        }
        if let Some(e) = &self.end {
            v.props.push(e.to_prop("DTEND"));
        } else if let Some(d) = self.duration {
            v.props.push(Prop::new("DURATION", format_duration(d)));
        }
        if let Some(r) = &self.recurrence_id {
            v.props.push(r.to_prop("RECURRENCE-ID"));
        }
        v.props.push(Prop::text("SUMMARY", &self.summary));
        if let Some(l) = &self.location {
            v.props.push(Prop::text("LOCATION", l));
        }
        if let Some(d) = &self.description {
            v.props.push(Prop::text("DESCRIPTION", d));
        }
        if let Some(r) = &self.rrule {
            v.props.push(Prop::new("RRULE", r.to_value()));
        }
        for x in &self.exdates {
            v.props.push(x.to_prop("EXDATE"));
        }
        for x in &self.rdates {
            v.props.push(x.to_prop("RDATE"));
        }
        if let Some(s) = &self.status {
            v.props.push(Prop::new("STATUS", s.clone()));
        }
        if let Some(o) = &self.organizer {
            v.props.push(o.to_prop("ORGANIZER", false));
        }
        for a in &self.attendees {
            v.props.push(a.to_prop("ATTENDEE", true));
        }
        if self.sequence > 0 {
            v.props.push(Prop::new("SEQUENCE", self.sequence.to_string()));
        }
        if let Some(u) = &self.url {
            v.props.push(Prop::new("URL", u.clone()));
        }
        if !self.categories.is_empty() {
            let c: Vec<String> = self.categories.iter().map(|c| line::escape(c)).collect();
            v.props.push(Prop::new("CATEGORIES", c.join(",")));
        }
        for p in &self.raw.props {
            if !KNOWN.contains(&p.name.as_str()) {
                v.props.push(p.clone());
            }
        }
        // Alarms are rewritten from `alarms`; other children are kept.
        v.children = self.raw.children.iter().filter(|c| c.name != "VALARM").cloned().collect();
        for a in &self.alarms {
            let mut al = Component::new("VALARM");
            al.props.push(Prop::new("ACTION", "DISPLAY"));
            al.props.push(Prop::new("TRIGGER", format_duration(*a)));
            al.props.push(Prop::text("DESCRIPTION", &self.summary));
            v.children.push(al);
        }
        v.props.push(Prop::new("LAST-MODIFIED", Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string()));
        v
    }

    pub fn all_day(&self) -> bool {
        self.start.as_ref().is_some_and(When::is_date)
    }

    pub fn cancelled(&self) -> bool {
        self.status.as_deref() == Some("CANCELLED")
    }

    /// How long each occurrence lasts.
    fn length(&self, viewer: &TimeZone) -> Length {
        let Some(start) = &self.start else { return Length::Secs(0) };
        if let (When::Date(s), Some(When::Date(e))) = (start, &self.end) {
            return Length::Days((*e - *s).get_days().max(1) as i64);
        }
        if let Some(e) = &self.end {
            let d = e.in_zone(viewer).timestamp().duration_since(start.in_zone(viewer).timestamp());
            return Length::Secs(d.as_secs().max(0));
        }
        match self.duration {
            Some(d) if start.is_date() && d % 86400 == 0 => Length::Days((d / 86400).max(1)),
            Some(d) => Length::Secs(d.max(0)),
            None if start.is_date() => Length::Days(1),
            None => Length::Secs(0),
        }
    }

    /// Occurrence start times that overlap `from..to`, in order, before overrides.
    pub fn occurrences(&self, from: &Zoned, to: &Zoned, viewer: &TimeZone) -> Vec<(Zoned, Zoned)> {
        let Some(start) = &self.start else { return vec![] };
        let (civil, tz) = start.civil(viewer);
        let len = self.length(viewer);
        let end_of = |s: &Zoned| -> Zoned {
            match len {
                Length::Days(n) => s.checked_add(n.days()).unwrap_or_else(|_| s.clone()),
                Length::Secs(n) => s.checked_add(SignedDuration::from_secs(n)).unwrap_or_else(|_| s.clone()),
            }
        };
        let mut starts: Vec<Zoned> = match &self.rrule {
            None => vec![start.in_zone(&tz)],
            Some(r) => {
                let to_civil = to.with_time_zone(tz.clone()).datetime();
                let until = r.until.as_deref().and_then(|u| {
                    if u.len() == 8 {
                        // A date: the whole of that day counts.
                        parse_date(u).map(|d| d.to_datetime(jiff::civil::Time::MAX))
                    } else {
                        let (dt, utc) = parse_datetime(u)?;
                        if utc {
                            dt.to_zoned(TimeZone::UTC).ok().map(|z| z.with_time_zone(tz.clone()).datetime())
                        } else {
                            Some(dt)
                        }
                    }
                });
                r.expand(civil, until, to_civil).into_iter().filter_map(|dt| dt.to_zoned(tz.clone()).ok()).collect()
            }
        };
        for r in &self.rdates {
            starts.push(r.in_zone(&tz));
        }
        let ex: Vec<Timestamp> = self.exdates.iter().map(|x| x.in_zone(&tz).timestamp()).collect();
        let ex_days: Vec<Date> = self.exdates.iter().filter(|x| x.is_date()).map(|x| x.date(&tz)).collect();
        starts.sort();
        starts.dedup();
        starts
            .into_iter()
            .filter(|s| !ex.contains(&s.timestamp()) && !(start.is_date() && ex_days.contains(&s.date())))
            .map(|s| {
                let e = end_of(&s);
                (s.with_time_zone(viewer.clone()), e.with_time_zone(viewer.clone()))
            })
            .filter(|(s, e)| {
                // An event with no length still shows at its start.
                (e > from || (s == e && s >= from)) && s < to
            })
            .collect()
    }

    /// The next time an alarm of this event fires after `after`, with the occurrence.
    pub fn next_alarm(&self, after: &Zoned, horizon: &Zoned, tz: &TimeZone) -> Option<(Zoned, Zoned)> {
        if self.alarms.is_empty() || self.cancelled() {
            return None;
        }
        let max_before = self.alarms.iter().map(|a| -a).max().unwrap_or(0).max(0);
        let from = after
            .checked_sub(SignedDuration::from_secs(self.alarms.iter().map(|a| a.abs()).max().unwrap_or(0)))
            .ok()?;
        let to = horizon.checked_add(SignedDuration::from_secs(max_before)).ok()?;
        let mut best: Option<(Zoned, Zoned)> = None;
        for (s, _) in self.occurrences(&from, &to, tz) {
            for a in &self.alarms {
                let Ok(at) = s.checked_add(SignedDuration::from_secs(*a)) else { continue };
                if at > *after && at <= *horizon && best.as_ref().is_none_or(|(b, _)| at < *b) {
                    best = Some((at, s.clone()));
                }
            }
        }
        best
    }
}

#[derive(Clone, Copy)]
enum Length {
    Days(i64),
    Secs(i64),
}

/// A parsed .ics file: its method (for invitations) and its events.
#[derive(Debug, Clone, Default)]
pub struct Calendar {
    pub method: Option<String>,
    pub events: Vec<Event>,
    /// The VCALENDAR's own properties and its other children (VTIMEZONE, VTODO).
    pub raw: Component,
}

impl Calendar {
    pub fn parse(text: &str) -> Calendar {
        let mut cal = Calendar::default();
        for c in line::parse(text) {
            if c.name != "VCALENDAR" {
                continue;
            }
            let hints = zone_hints(&c);
            if cal.method.is_none() {
                cal.method = c.text("METHOD").map(|m| m.to_ascii_uppercase());
            }
            for e in c.child("VEVENT") {
                cal.events.push(Event::from_component(e, &hints));
            }
            let mut rest = c.clone();
            rest.children.retain(|x| x.name != "VEVENT");
            if cal.raw.name.is_empty() {
                cal.raw = rest;
            } else {
                cal.raw.children.extend(rest.children);
            }
        }
        cal
    }

    /// Write events (one master and its overrides, usually) as a whole file.
    pub fn write(events: &[&Event], method: Option<&str>) -> String {
        let mut c = Component::new("VCALENDAR");
        c.props.push(Prop::new("VERSION", "2.0"));
        c.props.push(Prop::new("PRODID", "-//Alouette//Calendar//EN"));
        if let Some(m) = method {
            c.props.push(Prop::new("METHOD", m));
        }
        for e in events {
            c.children.push(e.to_component());
        }
        c.write()
    }
}

/// Every occurrence of `events` within `from..to`, with overrides (RECURRENCE-ID) taking
/// the place of the occurrences they change and cancelled ones left out. Sorted by start.
pub fn instances<'a>(events: &'a [Event], from: &Zoned, to: &Zoned, tz: &TimeZone) -> Vec<Instance<'a>> {
    let mut overridden: HashMap<&str, Vec<Timestamp>> = HashMap::new();
    for e in events {
        if let Some(r) = &e.recurrence_id {
            overridden.entry(e.uid.as_str()).or_default().push(r.in_zone(tz).timestamp());
        }
    }
    let mut out = vec![];
    for e in events {
        if e.cancelled() && e.recurrence_id.is_none() {
            continue;
        }
        let skip = if e.recurrence_id.is_none() { overridden.get(e.uid.as_str()) } else { None };
        for (s, end) in e.occurrences(from, to, tz) {
            if skip.is_some_and(|v| v.contains(&s.timestamp())) {
                continue;
            }
            if e.cancelled() {
                continue;
            }
            out.push(Instance { event: e, start: s, end, all_day: e.all_day() });
        }
    }
    out.sort_by(|a, b| {
        a.start.date().cmp(&b.start.date()).then((!a.all_day).cmp(&!b.all_day)).then(a.start.cmp(&b.start))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const ICS: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//Test//EN\r
BEGIN:VTIMEZONE\r
TZID:Custom Zone\r
BEGIN:STANDARD\r
DTSTART:19700101T000000\r
TZOFFSETFROM:+0100\r
TZOFFSETTO:+0100\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
UID:standup@example.org\r
DTSTART;TZID=America/New_York:20260302T090000\r
DTEND;TZID=America/New_York:20260302T091500\r
RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r
EXDATE;TZID=America/New_York:20260304T090000\r
SUMMARY:Standup\r
ORGANIZER;CN=Grace:mailto:grace@example.org\r
ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED:mailto:ada@example.org\r
BEGIN:VALARM\r
ACTION:DISPLAY\r
TRIGGER:-PT10M\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:standup@example.org\r
RECURRENCE-ID;TZID=America/New_York:20260306T090000\r
DTSTART;TZID=America/New_York:20260306T100000\r
DTEND;TZID=America/New_York:20260306T101500\r
SUMMARY:Standup (moved)\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:holiday@example.org\r
DTSTART;VALUE=DATE:20260309\r
DTEND;VALUE=DATE:20260311\r
SUMMARY:Away\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:custom@example.org\r
DTSTART;TZID=Custom Zone:20260310T120000\r
DURATION:PT1H\r
SUMMARY:Lunch\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn occurrences_with_overrides_across_dst() {
        let cal = Calendar::parse(ICS);
        assert_eq!(cal.events.len(), 4);
        let ny = TimeZone::get("America/New_York").unwrap();
        let from = date(2026, 3, 1).to_zoned(ny.clone()).unwrap();
        let to = date(2026, 3, 14).to_zoned(ny.clone()).unwrap();
        let got: Vec<String> = instances(&cal.events, &from, &to, &ny)
            .iter()
            .map(|i| format!("{} {}", i.start.strftime("%m-%d %H:%M"), i.event.summary))
            .collect();
        assert_eq!(
            got,
            [
                "03-02 09:00 Standup",
                "03-06 10:00 Standup (moved)",
                "03-09 00:00 Away",
                "03-09 09:00 Standup",
                "03-10 07:00 Lunch",
                "03-11 09:00 Standup",
                "03-13 09:00 Standup",
            ]
        );
        // 9:00 stays 9:00 after the clocks change on 8 March.
        let e = &cal.events[0];
        assert_eq!(e.alarms, [-600]);
        assert_eq!(e.organizer.as_ref().unwrap().email, "grace@example.org");
        assert_eq!(e.attendees[0].partstat, "ACCEPTED");
        let after = date(2026, 3, 9).at(8, 0, 0, 0).to_zoned(ny.clone()).unwrap();
        let horizon = after.checked_add(1.day()).unwrap();
        let (at, occ) = e.next_alarm(&after, &horizon, &ny).unwrap();
        assert_eq!(at.strftime("%H:%M").to_string(), "08:50");
        assert_eq!(occ.strftime("%d %H:%M").to_string(), "09 09:00");

        // Writing it back and reading it again keeps the same occurrences.
        let refs: Vec<&Event> = cal.events.iter().collect();
        let again = Calendar::parse(&Calendar::write(&refs, None));
        assert_eq!(instances(&again.events, &from, &to, &ny).len(), 7);
    }

    #[test]
    fn durations_and_windows_zones() {
        assert_eq!(parse_duration("-PT15M"), Some(-900));
        assert_eq!(parse_duration("P1DT2H"), Some(93600));
        assert_eq!(parse_duration("P2W"), Some(1_209_600));
        assert_eq!(format_duration(-900), "-PT15M");
        assert_eq!(format_duration(93600), "P1DT2H");
        let tz = zone("Pacific Standard Time", &ZoneHints::new());
        assert_eq!(tz.iana_name(), Some("America/Los_Angeles"));
        let tz = zone("(UTC-05:00) Eastern Time (US & Canada)", &ZoneHints::new());
        assert_eq!(tz.iana_name(), Some("America/New_York"));
    }
}
