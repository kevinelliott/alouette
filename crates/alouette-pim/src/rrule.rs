//! Recurrence rules (RFC 5545 section 3.3.10), expanded in wall-clock time so that a
//! 9:00 meeting stays at 9:00 across daylight saving changes.

use jiff::ToSpan;
use jiff::civil::{Date, DateTime, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: i32,
    pub count: Option<u32>,
    /// The raw UNTIL value; the event turns it into its own time.
    pub until: Option<String>,
    /// Weekdays, each with an ordinal (0 for every one, 1 for the first, -1 for the last).
    pub byday: Vec<(i32, Weekday)>,
    pub bymonthday: Vec<i32>,
    pub bymonth: Vec<i8>,
    pub byyearday: Vec<i32>,
    pub bysetpos: Vec<i32>,
    pub wkst: Weekday,
}

pub fn weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

pub fn weekday_code(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

fn ints<T: std::str::FromStr>(v: &str) -> Vec<T> {
    v.split(',').filter_map(|x| x.trim().trim_start_matches('+').parse().ok()).collect()
}

impl RRule {
    pub fn new(freq: Freq) -> Self {
        RRule {
            freq,
            interval: 1,
            count: None,
            until: None,
            byday: vec![],
            bymonthday: vec![],
            bymonth: vec![],
            byyearday: vec![],
            bysetpos: vec![],
            wkst: Weekday::Monday,
        }
    }

    /// Parse `FREQ=WEEKLY;BYDAY=MO,WE`. Frequencies below a day are not expanded.
    pub fn parse(s: &str) -> Option<RRule> {
        let mut r = RRule::new(Freq::Daily);
        let mut freq = None;
        for part in s.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match v.trim().to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => r.interval = v.trim().parse().unwrap_or(1).max(1),
                "COUNT" => r.count = v.trim().parse().ok(),
                "UNTIL" => r.until = Some(v.trim().to_string()),
                "BYDAY" => {
                    r.byday = v
                        .split(',')
                        .filter_map(|d| {
                            let d = d.trim();
                            if d.len() < 2 {
                                return None;
                            }
                            let (n, w) = d.split_at(d.len() - 2);
                            let n =
                                if n.is_empty() || n == "+" { 0 } else { n.trim_start_matches('+').parse().ok()? };
                            Some((n, weekday(w)?))
                        })
                        .collect()
                }
                "BYMONTHDAY" => r.bymonthday = ints(v),
                "BYMONTH" => r.bymonth = ints(v),
                "BYYEARDAY" => r.byyearday = ints(v),
                "BYSETPOS" => r.bysetpos = ints(v),
                "WKST" => r.wkst = weekday(v.trim()).unwrap_or(Weekday::Monday),
                _ => {}
            }
        }
        r.freq = freq?;
        Some(r)
    }

    pub fn to_value(&self) -> String {
        let mut out = vec![format!(
            "FREQ={}",
            match self.freq {
                Freq::Daily => "DAILY",
                Freq::Weekly => "WEEKLY",
                Freq::Monthly => "MONTHLY",
                Freq::Yearly => "YEARLY",
            }
        )];
        if self.interval > 1 {
            out.push(format!("INTERVAL={}", self.interval));
        }
        if let Some(c) = self.count {
            out.push(format!("COUNT={c}"));
        }
        if let Some(u) = &self.until {
            out.push(format!("UNTIL={u}"));
        }
        let join = |v: &[i32]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        if !self.byday.is_empty() {
            let d: Vec<String> = self
                .byday
                .iter()
                .map(|(n, w)| if *n == 0 { weekday_code(*w).to_string() } else { format!("{n}{}", weekday_code(*w)) })
                .collect();
            out.push(format!("BYDAY={}", d.join(",")));
        }
        if !self.bymonthday.is_empty() {
            out.push(format!("BYMONTHDAY={}", join(&self.bymonthday)));
        }
        if !self.bymonth.is_empty() {
            let m: Vec<i32> = self.bymonth.iter().map(|m| *m as i32).collect();
            out.push(format!("BYMONTH={}", join(&m)));
        }
        if !self.byyearday.is_empty() {
            out.push(format!("BYYEARDAY={}", join(&self.byyearday)));
        }
        if !self.bysetpos.is_empty() {
            out.push(format!("BYSETPOS={}", join(&self.bysetpos)));
        }
        if self.wkst != Weekday::Monday {
            out.push(format!("WKST={}", weekday_code(self.wkst)));
        }
        out.join(";")
    }

    /// A plain description, such as "every 2 weeks on Mon, Wed".
    pub fn describe(&self) -> String {
        let unit = match self.freq {
            Freq::Daily => "day",
            Freq::Weekly => "week",
            Freq::Monthly => "month",
            Freq::Yearly => "year",
        };
        let mut s =
            if self.interval > 1 { format!("every {} {unit}s", self.interval) } else { format!("every {unit}") };
        let weekdays = [Weekday::Monday, Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday];
        if self.freq == Freq::Weekly
            && self.interval == 1
            && self.byday.len() == 5
            && weekdays.iter().all(|w| self.byday.contains(&(0, *w)))
        {
            s = "every weekday".into();
        } else if !self.byday.is_empty() {
            let days: Vec<String> = self
                .byday
                .iter()
                .map(|(n, w)| {
                    let name = &format!("{w:?}")[..3];
                    match n {
                        0 => name.to_string(),
                        -1 => format!("last {name}"),
                        n => format!("{} {name}", ordinal(*n)),
                    }
                })
                .collect();
            s.push_str(&format!(" on {}", days.join(", ")));
        }
        if let Some(c) = self.count {
            s.push_str(&format!(", {c} times"));
        }
        s
    }

    /// Start times from `start`, in order, up to and including `end`, stopping at COUNT or
    /// `until` (already in the same wall-clock time as `start`).
    pub fn expand(&self, start: DateTime, until: Option<DateTime>, end: DateTime) -> Vec<DateTime> {
        let mut out = vec![];
        let mut emitted = 0u32;
        let time = start.time();
        let stop = until.map_or(end, |u| u.min(end));
        let mut push = |dt: DateTime, out: &mut Vec<DateTime>| -> bool {
            if self.count.is_some_and(|c| emitted >= c) || dt > stop {
                return false;
            }
            if until.is_some_and(|u| dt > u) {
                return false;
            }
            emitted += 1;
            out.push(dt);
            true
        };
        // The first occurrence is always the start itself.
        if !push(start, &mut out) {
            return out;
        }
        let d0 = start.date();
        let mut empty_run = 0;
        for k in 1..200_000i64 {
            let Some(period) = self.period(d0, (k - 1) * self.interval as i64) else { break };
            if period > stop.date() {
                break;
            }
            let mut days = self.candidates(period, d0);
            days.sort();
            days.dedup();
            if !self.bysetpos.is_empty() {
                let n = days.len() as i32;
                let mut picked: Vec<Date> = self
                    .bysetpos
                    .iter()
                    .filter_map(|p| {
                        let i = if *p > 0 { p - 1 } else { n + p };
                        (0..n).contains(&i).then(|| days[i as usize])
                    })
                    .collect();
                picked.sort();
                picked.dedup();
                days = picked;
            }
            if days.is_empty() {
                empty_run += 1;
                // A rule that can never match (BYMONTHDAY=31 with BYMONTH=2) ends here.
                if empty_run > 2000 {
                    break;
                }
                continue;
            }
            empty_run = 0;
            for d in days {
                let dt = d.to_datetime(time);
                if dt <= start {
                    continue;
                }
                if !push(dt, &mut out) {
                    return out;
                }
            }
        }
        out
    }

    /// The first day of the period `steps` units after the one holding `d0`.
    fn period(&self, d0: Date, steps: i64) -> Option<Date> {
        match self.freq {
            Freq::Daily => d0.checked_add(steps.days()).ok(),
            Freq::Weekly => {
                let back = (d0.weekday().to_monday_zero_offset() - self.wkst.to_monday_zero_offset()).rem_euclid(7);
                d0.checked_sub((back as i64).days()).ok()?.checked_add((steps * 7).days()).ok()
            }
            Freq::Monthly => d0.first_of_month().checked_add(steps.months()).ok(),
            Freq::Yearly => d0.first_of_year().checked_add(steps.years()).ok(),
        }
    }

    fn month_ok(&self, d: Date) -> bool {
        self.bymonth.is_empty() || self.bymonth.contains(&d.month())
    }

    fn monthday_ok(&self, d: Date) -> bool {
        self.bymonthday.is_empty()
            || self.bymonthday.iter().any(|m| {
                let n = d.days_in_month() as i32;
                let want = if *m > 0 { *m } else { n + m + 1 };
                want == d.day() as i32
            })
    }

    fn weekday_ok(&self, d: Date) -> bool {
        self.byday.is_empty() || self.byday.iter().any(|(_, w)| *w == d.weekday())
    }

    /// The days of one month that the BYMONTHDAY and BYDAY parts pick.
    fn month_days(&self, first: Date, d0: Date) -> Vec<Date> {
        let n = first.days_in_month() as i32;
        let by_md: Vec<Date> = self
            .bymonthday
            .iter()
            .filter_map(|m| {
                let day = if *m > 0 { *m } else { n + m + 1 };
                (1..=n).contains(&day).then(|| first.with().day(day as i8).build().ok()).flatten()
            })
            .collect();
        let by_wd: Vec<Date> =
            self.byday.iter().flat_map(|(ord, w)| nth_in(first, first.last_of_month(), *ord, *w)).collect();
        match (self.bymonthday.is_empty(), self.byday.is_empty()) {
            (false, false) => by_md.into_iter().filter(|d| by_wd.contains(d)).collect(),
            (false, true) => by_md,
            (true, false) => by_wd,
            (true, true) => {
                first.with().day(d0.day()).build().ok().filter(|d| d.month() == first.month()).into_iter().collect()
            }
        }
    }

    fn candidates(&self, period: Date, d0: Date) -> Vec<Date> {
        match self.freq {
            Freq::Daily => [period]
                .into_iter()
                .filter(|d| self.month_ok(*d) && self.monthday_ok(*d) && self.weekday_ok(*d))
                .collect(),
            Freq::Weekly => {
                let wds: Vec<Weekday> = if self.byday.is_empty() {
                    vec![d0.weekday()]
                } else {
                    self.byday.iter().map(|(_, w)| *w).collect()
                };
                wds.into_iter()
                    .filter_map(|w| {
                        let off = (w.to_monday_zero_offset() - self.wkst.to_monday_zero_offset()).rem_euclid(7);
                        period.checked_add((off as i64).days()).ok()
                    })
                    .filter(|d| self.month_ok(*d))
                    .collect()
            }
            Freq::Monthly => {
                if !self.month_ok(period) {
                    return vec![];
                }
                self.month_days(period, d0)
            }
            Freq::Yearly => {
                let year_end = period.last_of_year();
                if !self.byyearday.is_empty() {
                    let n = period.days_in_year() as i32;
                    return self
                        .byyearday
                        .iter()
                        .filter_map(|y| {
                            let day = if *y > 0 { *y } else { n + y + 1 };
                            (1..=n).contains(&day).then(|| period.checked_add(((day - 1) as i64).days()).ok()).flatten()
                        })
                        .filter(|d| self.month_ok(*d) && self.weekday_ok(*d) && self.monthday_ok(*d))
                        .collect();
                }
                if !self.bymonth.is_empty() {
                    let mut out = vec![];
                    for m in &self.bymonth {
                        if let Ok(first) = Date::new(period.year(), *m, 1) {
                            out.extend(self.month_days(first, d0));
                        }
                    }
                    return out;
                }
                if !self.byday.is_empty() {
                    return self
                        .byday
                        .iter()
                        .flat_map(|(ord, w)| nth_in(period, year_end, *ord, *w))
                        .filter(|d| self.monthday_ok(*d))
                        .collect();
                }
                if !self.bymonthday.is_empty() {
                    let mut out = vec![];
                    for m in 1..=12 {
                        if let Ok(first) = Date::new(period.year(), m, 1) {
                            out.extend(self.month_days(first, d0));
                        }
                    }
                    return out;
                }
                Date::new(period.year(), d0.month(), d0.day()).ok().into_iter().collect()
            }
        }
    }
}

fn ordinal(n: i32) -> String {
    match n {
        1 => "1st".into(),
        2 => "2nd".into(),
        3 => "3rd".into(),
        n => format!("{n}th"),
    }
}

/// Weekday `w` between `first` and `last`: every one when `ord` is 0, else the nth from
/// the start (or from the end when negative).
fn nth_in(first: Date, last: Date, ord: i32, w: Weekday) -> Vec<Date> {
    let fwd = (w.to_monday_zero_offset() - first.weekday().to_monday_zero_offset()).rem_euclid(7);
    let Ok(first_w) = first.checked_add((fwd as i64).days()) else { return vec![] };
    let back = (last.weekday().to_monday_zero_offset() - w.to_monday_zero_offset()).rem_euclid(7);
    let Ok(last_w) = last.checked_sub((back as i64).days()) else { return vec![] };
    match ord {
        0 => {
            let mut out = vec![];
            let mut d = first_w;
            while d <= last {
                out.push(d);
                match d.checked_add(7.days()) {
                    Ok(n) => d = n,
                    Err(_) => break,
                }
            }
            out
        }
        n if n > 0 => {
            first_w.checked_add(((n - 1) as i64 * 7).days()).ok().filter(|d| *d <= last).into_iter().collect()
        }
        n => last_w.checked_sub(((-n - 1) as i64 * 7).days()).ok().filter(|d| *d >= first).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn run(rule: &str, start: DateTime, n: usize) -> Vec<String> {
        let r = RRule::parse(rule).unwrap();
        r.expand(start, None, date(2100, 1, 1).at(0, 0, 0, 0))
            .into_iter()
            .take(n)
            .map(|d| d.date().to_string())
            .collect()
    }

    #[test]
    fn rules() {
        let s = date(2026, 1, 5).at(9, 0, 0, 0); // a Monday
        assert_eq!(run("FREQ=DAILY;COUNT=3", s, 10), ["2026-01-05", "2026-01-06", "2026-01-07"]);
        assert_eq!(
            run("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4", s, 10),
            ["2026-01-05", "2026-01-07", "2026-01-12", "2026-01-14"]
        );
        assert_eq!(run("FREQ=WEEKLY;INTERVAL=2;COUNT=3", s, 10), ["2026-01-05", "2026-01-19", "2026-02-02"]);
        // The last Friday of each month.
        assert_eq!(run("FREQ=MONTHLY;BYDAY=-1FR", s, 4), ["2026-01-05", "2026-01-30", "2026-02-27", "2026-03-27"]);
        // The 31st only falls in long months.
        assert_eq!(
            run("FREQ=MONTHLY;BYMONTHDAY=31", date(2026, 1, 31).at(0, 0, 0, 0), 3),
            ["2026-01-31", "2026-03-31", "2026-05-31"]
        );
        // US Thanksgiving.
        assert_eq!(
            run("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH", date(2026, 11, 26).at(0, 0, 0, 0), 3),
            ["2026-11-26", "2027-11-25", "2028-11-23"]
        );
        // The last weekday of the month.
        assert_eq!(
            run("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1", date(2026, 1, 30).at(0, 0, 0, 0), 3),
            ["2026-01-30", "2026-02-27", "2026-03-31"]
        );
        // Birthdays on 29 February.
        assert_eq!(run("FREQ=YEARLY", date(2024, 2, 29).at(0, 0, 0, 0), 3), ["2024-02-29", "2028-02-29", "2032-02-29"]);
        let r = RRule::parse("FREQ=DAILY;UNTIL=20260107T090000Z").unwrap();
        assert_eq!(r.expand(s, Some(date(2026, 1, 7).at(9, 0, 0, 0)), date(2030, 1, 1).at(0, 0, 0, 0)).len(), 3);
        assert_eq!(
            RRule::parse("FREQ=WEEKLY;BYDAY=MO,-1FR;INTERVAL=2").unwrap().to_value(),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,-1FR"
        );
        assert_eq!(RRule::parse("FREQ=WEEKLY;BYDAY=MO,WE").unwrap().describe(), "every week on Mon, Wed");
        assert!(run("FREQ=MONTHLY;BYMONTH=2;BYMONTHDAY=31", s, 5).len() == 1);
    }
}
