//! Invitations (iTIP, RFC 5546): answering a REQUEST, and applying REPLY and CANCEL
//! messages to events already in a calendar.

use crate::event::{Calendar, Event, Person};
use crate::store::{Stores, file_for};

/// Every event with this UID in any calendar: the calendar's name, the file, and the
/// events in that file.
pub fn find(stores: &Stores, uid: &str) -> Option<(String, String, Vec<Event>)> {
    for cal in stores.calendars() {
        for item in stores.calendar(&cal).list() {
            let parsed = Calendar::parse(&item.text);
            if parsed.events.iter().any(|e| e.uid == uid) {
                return Some((cal, item.file, parsed.events));
            }
        }
    }
    None
}

/// Put events (a master and its overrides) in a calendar, replacing any copy of them
/// in that calendar or another.
pub fn save(stores: &Stores, cal: &str, events: &[Event]) -> std::io::Result<()> {
    let Some(uid) = events.first().map(|e| e.uid.clone()) else { return Ok(()) };
    let mut file = file_for(&uid, "ics");
    if let Some((old_cal, old_file, _)) = find(stores, &uid) {
        if old_cal == cal {
            file = old_file;
        } else {
            stores.calendar(&old_cal).delete_local(&old_file)?;
        }
    }
    let refs: Vec<&Event> = events.iter().collect();
    stores.calendar(cal).put_local(&file, &Calendar::write(&refs, None))
}

/// Take an event out of whichever calendar has it.
pub fn remove(stores: &Stores, uid: &str) -> std::io::Result<bool> {
    match find(stores, uid) {
        Some((cal, file, _)) => stores.calendar(&cal).delete_local(&file).map(|_| true),
        None => Ok(false),
    }
}

/// What an invitation file asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Request,
    Reply,
    Cancel,
    /// PUBLISH, or a plain file with no METHOD: just events to add.
    Publish,
}

pub fn method(cal: &Calendar) -> Method {
    match cal.method.as_deref() {
        Some("REQUEST") | Some("ADD") | Some("COUNTER") => Method::Request,
        Some("REPLY") => Method::Reply,
        Some("CANCEL") => Method::Cancel,
        _ => Method::Publish,
    }
}

/// The attendee entry that is one of `mine`.
pub fn me<'a>(event: &'a Event, mine: &[String]) -> Option<&'a Person> {
    event.attendees.iter().find(|a| mine.iter().any(|m| m.eq_ignore_ascii_case(&a.email)))
}

/// A copy of the event with our own answer recorded, ready for the calendar.
pub fn answered(event: &Event, me: &str, partstat: &str) -> Event {
    let mut e = event.clone();
    let mut found = false;
    for a in &mut e.attendees {
        if a.email.eq_ignore_ascii_case(me) {
            a.partstat = partstat.to_string();
            a.rsvp = false;
            found = true;
        }
    }
    if !found {
        e.attendees.push(Person {
            email: me.to_string(),
            partstat: partstat.to_string(),
            role: "REQ-PARTICIPANT".into(),
            ..Default::default()
        });
    }
    if partstat == "TENTATIVE" {
        e.status.get_or_insert_with(|| "TENTATIVE".into());
    }
    e
}

/// The METHOD:REPLY file to send the organiser.
pub fn reply(event: &Event, me: &Person, partstat: &str) -> String {
    let mut r = Event {
        uid: event.uid.clone(),
        summary: event.summary.clone(),
        start: event.start.clone(),
        end: event.end.clone(),
        duration: event.duration,
        recurrence_id: event.recurrence_id.clone(),
        organizer: event.organizer.clone(),
        sequence: event.sequence,
        ..Default::default()
    };
    r.attendees.push(Person { partstat: partstat.to_string(), rsvp: false, ..me.clone() });
    Calendar::write(&[&r], Some("REPLY"))
}

/// The subject line for a reply: "Accepted: Standup".
pub fn reply_subject(event: &Event, partstat: &str) -> String {
    let word = match partstat {
        "ACCEPTED" => "Accepted",
        "DECLINED" => "Declined",
        "TENTATIVE" => "Tentative",
        _ => "Reply",
    };
    format!("{word}: {}", event.summary)
}

/// Record the answers in a REPLY on our copy of the event. Returns who answered what.
pub fn apply_reply(stored: &mut Event, reply: &Event) -> Vec<(String, String)> {
    let mut changed = vec![];
    for r in &reply.attendees {
        if let Some(a) = stored.attendees.iter_mut().find(|a| a.email.eq_ignore_ascii_case(&r.email)) {
            if a.partstat != r.partstat {
                a.partstat = r.partstat.clone();
                changed.push((r.label(), r.partstat.clone()));
            }
        } else {
            stored.attendees.push(r.clone());
            changed.push((r.label(), r.partstat.clone()));
        }
    }
    changed
}

/// A plain description of the invitation for the reader.
pub fn describe(event: &Event, tz: &jiff::tz::TimeZone) -> Vec<String> {
    let mut out = vec![];
    if let Some(s) = &event.start {
        let start = s.in_zone(tz);
        let when = if s.is_date() {
            start.strftime("%A %-d %B %Y").to_string()
        } else {
            start.strftime("%A %-d %B %Y, %H:%M").to_string()
        };
        out.push(format!("When: {when}"));
    }
    if let Some(r) = &event.rrule {
        out.push(format!("Repeats: {}", r.describe()));
    }
    if let Some(l) = &event.location {
        out.push(format!("Where: {l}"));
    }
    if let Some(o) = &event.organizer {
        out.push(format!("Organiser: {}", o.label()));
    }
    if !event.attendees.is_empty() {
        let who: Vec<String> = event
            .attendees
            .iter()
            .map(|a| match a.partstat.as_str() {
                "ACCEPTED" => format!("{} (yes)", a.name.as_deref().unwrap_or(&a.email)),
                "DECLINED" => format!("{} (no)", a.name.as_deref().unwrap_or(&a.email)),
                "TENTATIVE" => format!("{} (maybe)", a.name.as_deref().unwrap_or(&a.email)),
                _ => a.name.clone().unwrap_or_else(|| a.email.clone()),
            })
            .collect();
        out.push(format!("Guests: {}", who.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:42@example.org\r\nSEQUENCE:1\r\nDTSTART:20260401T150000Z\r\nDTEND:20260401T160000Z\r\nSUMMARY:Design review\r\nORGANIZER;CN=Grace:mailto:grace@example.org\r\nATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn answer_an_invitation() {
        let cal = Calendar::parse(INVITE);
        assert_eq!(method(&cal), Method::Request);
        let ev = &cal.events[0];
        let mine = vec!["ADA@example.org".to_string()];
        let me = me(ev, &mine).unwrap().clone();
        assert!(me.rsvp);
        let text = reply(ev, &me, "ACCEPTED");
        let r = Calendar::parse(&text);
        assert_eq!(method(&r), Method::Reply);
        assert_eq!(r.events[0].uid, "42@example.org");
        assert_eq!(r.events[0].sequence, 1);
        assert_eq!(r.events[0].attendees.len(), 1);
        assert_eq!(r.events[0].attendees[0].partstat, "ACCEPTED");
        assert_eq!(reply_subject(ev, "ACCEPTED"), "Accepted: Design review");

        let mut ours = answered(ev, "ada@example.org", "ACCEPTED");
        assert_eq!(ours.attendees[0].partstat, "ACCEPTED");
        // The organiser gets a decline from Bob.
        let mut bob = r.events[0].clone();
        bob.attendees[0].email = "bob@example.org".into();
        bob.attendees[0].partstat = "DECLINED".into();
        bob.attendees[0].name = None;
        assert_eq!(apply_reply(&mut ours, &bob), [("bob@example.org".to_string(), "DECLINED".to_string())]);
        let tz = jiff::tz::TimeZone::UTC;
        let d = describe(&ours, &tz);
        assert_eq!(d[0], "When: Wednesday 1 April 2026, 15:00");
        assert!(d.last().unwrap().contains("Ada (yes)"));
    }
}
