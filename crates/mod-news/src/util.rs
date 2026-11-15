//! Small drawing helpers shared by the News and Feeds windows.

pub use alouette_ui::chrome::fit;

pub fn short_date(t: Option<i64>, offset: time::UtcOffset) -> String {
    let Some(d) = t.and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok()) else { return String::new() };
    let d = d.to_offset(offset);
    let now = time::OffsetDateTime::now_utc().to_offset(offset);
    if d.date() == now.date() {
        format!("{:02}:{:02}", d.hour(), d.minute())
    } else if d.year() == now.year() {
        format!("{} {:>2}", &format!("{:?}", d.month())[..3], d.day())
    } else {
        format!("{:04}-{:02}", d.year(), d.month() as u8)
    }
}

pub fn long_date(t: Option<i64>, offset: time::UtcOffset) -> String {
    let Some(d) = t.and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok()) else { return String::new() };
    let d = d.to_offset(offset);
    format!("{:?}, {} {:?} {} {:02}:{:02}", d.weekday(), d.day(), d.month(), d.year(), d.hour(), d.minute())
}

/// The display name from `Ada Lovelace <ada@example.org>` or `ada@example.org (Ada)`.
pub fn who(from: &str) -> String {
    let f = from.trim();
    if let Some(i) = f.find('<') {
        let name = f[..i].trim().trim_matches('"');
        if !name.is_empty() {
            return name.to_string();
        }
        return f[i + 1..].trim_end_matches('>').to_string();
    }
    if let (Some(a), Some(b)) = (f.find('('), f.rfind(')'))
        && a < b
    {
        return f[a + 1..b].to_string();
    }
    f.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_widths() {
        assert_eq!(who("\"Ada L\" <a@x>"), "Ada L");
        assert_eq!(who("a@x (Ada)"), "Ada");
        assert_eq!(who("<a@x>"), "a@x");
        assert_eq!(fit("hello", 3), "he…");
        assert_eq!(fit("hi", 4), "hi  ");
    }
}
