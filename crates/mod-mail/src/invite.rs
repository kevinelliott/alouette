//! Invitations in mail: what an attached iCalendar asks for, and putting the answer in
//! the calendar.

use std::path::Path;

use alouette_core::Config;
use alouette_pim::event::{Calendar, Event, Person};
use alouette_pim::itip::{self, Method};
use alouette_pim::store::Stores;
use jiff::tz::TimeZone;

/// An invitation (or a reply, cancellation or plain event) found in a message.
#[derive(Debug, Clone)]
pub struct Invite {
    pub method: Method,
    pub events: Vec<Event>,
    /// Lines to show above the message.
    pub lines: Vec<String>,
    /// What we already answered, from the copy in our calendar.
    pub answered: Option<String>,
}

/// Every address that is us: all the mail accounts.
pub fn mine(cfg: &Config) -> Vec<String> {
    cfg.mail.accounts.iter().map(|a| a.email.clone()).collect()
}

/// The calendar new events go in: the first CalDAV calendar, else the local one.
pub fn target(cfg: &Config) -> String {
    cfg.calendar.accounts.first().map(|a| a.name.clone()).unwrap_or_else(|| "local".into())
}

pub fn read(text: &str, data_dir: &Path, cfg: &Config) -> Option<Invite> {
    let cal = Calendar::parse(text);
    let first = cal.events.first()?.clone();
    let method = itip::method(&cal);
    let tz = TimeZone::system();
    let mut lines = vec![match method {
        Method::Request => format!("Invitation: {}", first.summary),
        Method::Reply => format!("Answer about: {}", first.summary),
        Method::Cancel => format!("Cancelled: {}", first.summary),
        Method::Publish => format!("Event: {}", first.summary),
    }];
    lines.extend(itip::describe(&first, &tz));
    let stores = Stores::new(data_dir);
    let mine = mine(cfg);
    let answered = itip::find(&stores, &first.uid).map(|(_, _, evs)| {
        evs.iter()
            .find(|e| e.recurrence_id.is_none())
            .and_then(|e| itip::me(e, &mine))
            .map(|p| p.partstat.clone())
            .unwrap_or_else(|| "IN-CALENDAR".into())
    });
    let events: Vec<Event> = cal.events.iter().filter(|e| e.uid == first.uid).cloned().collect();
    Some(Invite { method, events, lines, answered })
}

/// What Y, N and T do here, for the hint line.
pub fn hint(inv: &Invite) -> String {
    let status = match inv.answered.as_deref() {
        Some("ACCEPTED") => "You accepted. ",
        Some("DECLINED") => "You declined. ",
        Some("TENTATIVE") => "You said maybe. ",
        Some(_) => "It is in your calendar. ",
        None => "",
    };
    let keys = match inv.method {
        Method::Request => "Y accept, N decline, T maybe",
        Method::Cancel if inv.answered.is_some() => "Y takes it out of your calendar",
        Method::Cancel => "",
        Method::Publish => "Y adds it to your calendar",
        Method::Reply => "",
    };
    format!("{status}{keys}")
}

/// The outcome of answering.
pub struct Answer {
    pub note: String,
    /// A reply to send the organiser: to, subject, body and the REPLY calendar.
    pub reply: Option<(String, String, String, String)>,
}

/// Act on Y (`ACCEPTED`), N (`DECLINED`) or T (`TENTATIVE`).
pub fn answer(
    inv: &Invite,
    partstat: &str,
    me_addr: &str,
    me_name: Option<&str>,
    data_dir: &Path,
    cfg: &Config,
) -> Result<Answer, String> {
    let stores = Stores::new(data_dir);
    let first = inv.events.first().ok_or("the invitation has no event")?;
    let io = |e: std::io::Error| format!("could not save to the calendar: {e}");
    match inv.method {
        Method::Cancel => {
            itip::remove(&stores, &first.uid).map_err(io)?;
            Ok(Answer { note: format!("Took {} out of your calendar", first.summary), reply: None })
        }
        Method::Publish => {
            itip::save(&stores, &target(cfg), &inv.events).map_err(io)?;
            Ok(Answer { note: format!("Added {} to your calendar", first.summary), reply: None })
        }
        Method::Reply => Ok(Answer { note: "This is an answer; it is already in your calendar".into(), reply: None }),
        Method::Request => {
            let mine = mine(cfg);
            let me = itip::me(first, &mine).cloned().unwrap_or(Person {
                email: me_addr.to_string(),
                name: me_name.map(str::to_string),
                partstat: "NEEDS-ACTION".into(),
                role: "REQ-PARTICIPANT".into(),
                rsvp: true,
            });
            let ours: Vec<Event> = inv.events.iter().map(|e| itip::answered(e, &me.email, partstat)).collect();
            if partstat == "DECLINED" {
                itip::remove(&stores, &first.uid).map_err(io)?;
            } else {
                itip::save(&stores, &target(cfg), &ours).map_err(io)?;
            }
            let word = match partstat {
                "ACCEPTED" => "accepted",
                "DECLINED" => "declined",
                _ => "said maybe to",
            };
            let reply =
                first.organizer.as_ref().filter(|o| !mine.iter().any(|m| m.eq_ignore_ascii_case(&o.email))).map(|o| {
                    let who = me.name.clone().unwrap_or_else(|| me.email.clone());
                    (
                        o.label(),
                        itip::reply_subject(first, partstat),
                        format!("{who} has {word} \"{}\".\n", first.summary),
                        itip::reply(first, &me, partstat),
                    )
                });
            let place = if partstat == "DECLINED" {
                "It is not in your calendar.".to_string()
            } else {
                format!("It is in your {} calendar.", target(cfg))
            };
            let tell = match &reply {
                Some((to, ..)) => format!(" Press Ctrl+S in the new window to tell {to}."),
                None => String::new(),
            };
            Ok(Answer { note: format!("You {word} {}. {place}{tell}", first.summary), reply })
        }
    }
}

/// Record the answers in a REPLY on our copy of the event. Returns a note.
pub fn apply_reply(inv: &Invite, data_dir: &Path) -> Option<String> {
    let stores = Stores::new(data_dir);
    let reply = inv.events.first()?;
    let (cal, _, mut evs) = itip::find(&stores, &reply.uid)?;
    let target = evs.iter_mut().find(|e| e.recurrence_id == reply.recurrence_id)?;
    let changed = itip::apply_reply(target, reply);
    if changed.is_empty() {
        return None;
    }
    itip::save(&stores, &cal, &evs).ok()?;
    let who: Vec<String> = changed
        .iter()
        .map(|(w, s)| {
            let s = match s.as_str() {
                "ACCEPTED" => "accepted",
                "DECLINED" => "declined",
                "TENTATIVE" => "said maybe",
                _ => "answered",
            };
            format!("{w} {s}")
        })
        .collect();
    Some(format!("{}; your calendar is up to date", who.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alouette_core::config::MailAccount;

    const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:review-1@example.org\r\nDTSTART:20260401T150000Z\r\nDTEND:20260401T160000Z\r\nSUMMARY:Design review\r\nORGANIZER;CN=Grace:mailto:grace@example.org\r\nATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn an_invite_in_mail_lands_in_the_calendar() {
        let dir = std::env::temp_dir().join(format!("alouette-mail-invite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.mail.accounts.push(MailAccount {
            name: "me".into(),
            email: "ada@example.org".into(),
            ..Default::default()
        });
        let inv = read(INVITE, &dir, &cfg).unwrap();
        assert_eq!(inv.method, Method::Request);
        assert_eq!(inv.lines[0], "Invitation: Design review");
        assert!(hint(&inv).contains("Y accept"));
        let a = answer(&inv, "ACCEPTED", "ada@example.org", Some("Ada"), &dir, &cfg).unwrap();
        let (to, subject, _, ics) = a.reply.unwrap();
        assert_eq!(to, "Grace <grace@example.org>");
        assert_eq!(subject, "Accepted: Design review");
        assert!(ics.contains("METHOD:REPLY") && ics.contains("PARTSTAT=ACCEPTED"));
        // It is in the calendar now, as accepted.
        let stores = Stores::new(&dir);
        let (cal, _, evs) = itip::find(&stores, "review-1@example.org").unwrap();
        assert_eq!(cal, "local");
        assert_eq!(evs[0].attendees[0].partstat, "ACCEPTED");
        assert_eq!(read(INVITE, &dir, &cfg).unwrap().answered.as_deref(), Some("ACCEPTED"));
        // Declining takes it out again.
        answer(&inv, "DECLINED", "ada@example.org", None, &dir, &cfg).unwrap();
        assert!(itip::find(&stores, "review-1@example.org").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
