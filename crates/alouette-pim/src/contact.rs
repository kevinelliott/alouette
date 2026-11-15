//! Contacts as vCard 3.0 and 4.0 (RFC 6350), with the links Alouette follows between
//! modules: mail addresses, fediverse handles and SSH hosts.

use crate::line::{self, Component, Prop, escape, split_structured};

/// A value with its kind, such as a work address or a mobile number.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Typed {
    /// "work", "home", "cell" and so on; empty when the card did not say.
    pub kind: String,
    pub value: String,
    pub pref: bool,
}

impl Typed {
    pub fn new(kind: &str, value: &str) -> Self {
        Typed { kind: kind.to_string(), value: value.to_string(), pref: false }
    }
}

/// One person or organisation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contact {
    pub uid: String,
    /// The name as shown (FN).
    pub name: String,
    pub family: String,
    pub given: String,
    pub nickname: Option<String>,
    pub org: Option<String>,
    pub title: Option<String>,
    pub emails: Vec<Typed>,
    pub phones: Vec<Typed>,
    /// Postal addresses, written out on one line each.
    pub addresses: Vec<Typed>,
    pub urls: Vec<String>,
    pub birthday: Option<String>,
    pub note: Option<String>,
    pub categories: Vec<String>,
    /// Fediverse handles, as `@user@server`.
    pub fediverse: Vec<String>,
    /// SSH hosts: a saved host name, or `user@host[:port]`.
    pub ssh: Vec<String>,
    /// Chat addresses (IMPP) that are not fediverse handles, such as `xmpp:` or `matrix:`.
    pub chat: Vec<String>,
    /// Every property of the card as read, so that writing it back keeps what Alouette
    /// does not show.
    pub raw: Component,
}

const KNOWN: &[&str] = &[
    "VERSION",
    "UID",
    "FN",
    "N",
    "NICKNAME",
    "ORG",
    "TITLE",
    "EMAIL",
    "TEL",
    "ADR",
    "URL",
    "BDAY",
    "NOTE",
    "CATEGORIES",
    "X-SOCIALPROFILE",
    "SOCIALPROFILE",
    "IMPP",
    "X-ALOUETTE-SSH",
    "PRODID",
    "REV",
];

/// `@user@host` from the shapes a fediverse address comes in: `@u@h`, `u@h`,
/// `acct:u@h`, `https://h/@u` and `https://h/users/u`.
pub fn fediverse_handle(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.strip_prefix("acct:").unwrap_or(s);
    let s = s.strip_prefix("x-apple:").unwrap_or(s);
    if let Some(rest) = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")) {
        let (host, path) = rest.split_once('/')?;
        let path = path.trim_end_matches('/');
        let user = path.strip_prefix('@').or_else(|| path.strip_prefix("users/")).filter(|u| !u.contains('/'))?;
        let user = user.split('@').next()?;
        return (!user.is_empty() && host.contains('.')).then(|| format!("@{user}@{host}"));
    }
    let t = s.trim_start_matches('@');
    let (user, host) = t.split_once('@')?;
    (!user.is_empty() && host.contains('.') && !host.contains(['/', '@', ' '])).then(|| format!("@{user}@{host}"))
}

/// The profile URL for a handle, `https://host/@user`.
pub fn fediverse_url(handle: &str) -> Option<String> {
    let t = handle.trim_start_matches('@');
    let (user, host) = t.split_once('@')?;
    Some(format!("https://{host}/@{user}"))
}

fn kind_of(p: &Prop) -> (String, bool) {
    let types = p.param_all("TYPE");
    let pref = types.iter().any(|t| t == "pref") || p.param("PREF").is_some();
    let kind =
        types.into_iter().find(|t| !matches!(t.as_str(), "pref" | "internet" | "voice" | "x400")).unwrap_or_default();
    (kind, pref)
}

fn typed(p: &Prop) -> Typed {
    let (kind, pref) = kind_of(p);
    Typed { kind, value: p.text_value().trim().to_string(), pref }
}

fn strip_scheme(v: &str, scheme: &str) -> String {
    let lower = v.to_ascii_lowercase();
    if lower.starts_with(scheme) { v[scheme.len()..].to_string() } else { v.to_string() }
}

impl Contact {
    pub fn new(name: &str) -> Self {
        let mut c = Contact { uid: new_uid(), name: name.to_string(), ..Default::default() };
        let (given, family) = match name.rsplit_once(' ') {
            Some((g, f)) => (g.to_string(), f.to_string()),
            None => (name.to_string(), String::new()),
        };
        c.given = given;
        c.family = family;
        c
    }

    /// Every contact in a .vcf file.
    pub fn parse_all(text: &str) -> Vec<Contact> {
        line::parse(text).iter().filter(|c| c.name == "VCARD").map(Contact::from_component).collect()
    }

    pub fn from_component(v: &Component) -> Contact {
        let mut c = Contact { raw: v.clone(), ..Default::default() };
        c.uid = v.text("UID").map(|u| strip_scheme(&u, "urn:uuid:")).unwrap_or_default();
        if let Some(n) = v.get("N") {
            let parts = split_structured(&n.value, ';');
            c.family = parts.first().cloned().unwrap_or_default();
            c.given = parts.get(1).cloned().unwrap_or_default();
        }
        c.name = v.text("FN").unwrap_or_else(|| {
            let n = format!("{} {}", c.given, c.family).trim().to_string();
            if n.is_empty() { v.text("ORG").unwrap_or_default() } else { n }
        });
        c.nickname = v.text("NICKNAME");
        c.org = v.get("ORG").map(|o| {
            split_structured(&o.value, ';').into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
        });
        c.org = c.org.filter(|o| !o.is_empty());
        c.title = v.text("TITLE");
        c.emails = v.all("EMAIL").map(typed).filter(|t| !t.value.is_empty()).collect();
        c.phones = v
            .all("TEL")
            .map(|p| {
                let mut t = typed(p);
                t.value = strip_scheme(&t.value, "tel:");
                t
            })
            .filter(|t| !t.value.is_empty())
            .collect();
        c.addresses = v
            .all("ADR")
            .map(|p| {
                let (kind, pref) = kind_of(p);
                let parts: Vec<String> =
                    split_structured(&p.value, ';').into_iter().map(|s| s.trim().to_string()).collect();
                // PO box; extended; street; locality; region; code; country
                let mut out: Vec<String> = vec![];
                for i in [2, 1, 0, 3, 4, 5, 6] {
                    if let Some(s) = parts.get(i).filter(|s| !s.is_empty()) {
                        out.push(s.replace('\n', ", "));
                    }
                }
                Typed { kind, value: out.join(", "), pref }
            })
            .filter(|t| !t.value.is_empty())
            .collect();
        c.urls = v.all("URL").map(|p| p.text_value()).filter(|s| !s.is_empty()).collect();
        c.birthday = v.text("BDAY");
        c.note = v.text("NOTE");
        c.categories = v
            .all("CATEGORIES")
            .flat_map(|p| split_structured(&p.value, ','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for p in v.all("X-SOCIALPROFILE").chain(v.all("SOCIALPROFILE")) {
            let val = p.text_value();
            let user = p.param("X-USER").or(p.param("USERNAME"));
            let h = fediverse_handle(&val).or_else(|| {
                let host = url::Url::parse(&val).ok()?.host_str()?.to_string();
                Some(format!("@{}@{host}", user?.trim_start_matches('@')))
            });
            if let Some(h) = h
                && !c.fediverse.contains(&h)
            {
                c.fediverse.push(h);
            }
        }
        for p in v.all("IMPP") {
            let val = p.text_value();
            let lower = val.to_ascii_lowercase();
            let service = p.param("X-SERVICE-TYPE").unwrap_or("").to_ascii_lowercase();
            let fedi = lower.starts_with("acct:")
                || service.contains("mastodon")
                || service.contains("fediverse")
                || p.param_all("TYPE").iter().any(|t| t == "fediverse");
            match fedi.then(|| fediverse_handle(&val)).flatten() {
                Some(h) => {
                    if !c.fediverse.contains(&h) {
                        c.fediverse.push(h);
                    }
                }
                None => c.chat.push(val),
            }
        }
        c.ssh = v.all("X-ALOUETTE-SSH").map(|p| p.text_value()).filter(|s| !s.is_empty()).collect();
        c
    }

    /// The card as vCard 4.0, keeping the properties Alouette does not edit.
    pub fn to_component(&self) -> Component {
        let mut v = Component::new("VCARD");
        v.props.push(Prop::new("VERSION", "4.0"));
        v.props.push(Prop::new("UID", format!("urn:uuid:{}", self.uid)));
        v.props.push(Prop::text("FN", &self.name));
        v.props.push(Prop::new("N", format!("{};{};;;", escape(&self.family), escape(&self.given))));
        if let Some(n) = &self.nickname {
            v.props.push(Prop::text("NICKNAME", n));
        }
        if let Some(o) = &self.org {
            v.props.push(Prop::text("ORG", o));
        }
        if let Some(t) = &self.title {
            v.props.push(Prop::text("TITLE", t));
        }
        let with_type = |name: &str, t: &Typed, value: String| {
            let mut p = Prop::new(name, value);
            if !t.kind.is_empty() {
                p = p.with_param("TYPE", &t.kind);
            }
            if t.pref {
                p = p.with_param("PREF", "1");
            }
            p
        };
        for e in &self.emails {
            v.props.push(with_type("EMAIL", e, escape(&e.value)));
        }
        for t in &self.phones {
            v.props.push(with_type("TEL", t, escape(&t.value)));
        }
        for a in &self.addresses {
            // Written whole into the street part; readers show it as typed.
            v.props.push(with_type("ADR", a, format!(";;{};;;;", escape(&a.value))));
        }
        for u in &self.urls {
            v.props.push(Prop::new("URL", u.clone()));
        }
        if let Some(b) = &self.birthday {
            v.props.push(Prop::new("BDAY", b.clone()));
        }
        if let Some(n) = &self.note {
            v.props.push(Prop::text("NOTE", n));
        }
        if !self.categories.is_empty() {
            let cats: Vec<String> = self.categories.iter().map(|c| escape(c)).collect();
            v.props.push(Prop::new("CATEGORIES", cats.join(",")));
        }
        for h in &self.fediverse {
            let url = fediverse_url(h).unwrap_or_else(|| h.clone());
            v.props.push(Prop::new("X-SOCIALPROFILE", url).with_param("TYPE", "fediverse"));
        }
        for c in &self.chat {
            v.props.push(Prop::new("IMPP", c.clone()));
        }
        for s in &self.ssh {
            v.props.push(Prop::text("X-ALOUETTE-SSH", s));
        }
        for p in &self.raw.props {
            if !KNOWN.contains(&p.name.as_str()) {
                v.props.push(p.clone());
            }
        }
        v.props.push(Prop::new("PRODID", "-//Alouette//Contacts//EN"));
        v.props.push(Prop::new("REV", jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string()));
        v.children = self.raw.children.clone();
        v
    }

    pub fn to_vcard(&self) -> String {
        self.to_component().write()
    }

    /// The address to write to: the preferred one, else the first.
    pub fn email(&self) -> Option<&str> {
        self.emails.iter().find(|e| e.pref).or(self.emails.first()).map(|e| e.value.as_str())
    }

    /// Whether a mail address belongs to this contact, ignoring case.
    pub fn has_email(&self, addr: &str) -> bool {
        self.emails.iter().any(|e| e.value.eq_ignore_ascii_case(addr.trim()))
    }

    /// Whether any field matches every word of `q`, ignoring case.
    pub fn matches(&self, q: &str) -> bool {
        let hay = [
            self.name.as_str(),
            self.nickname.as_deref().unwrap_or(""),
            self.org.as_deref().unwrap_or(""),
            self.title.as_deref().unwrap_or(""),
            self.note.as_deref().unwrap_or(""),
        ]
        .join(" ")
            + " "
            + &self.emails.iter().map(|e| e.value.as_str()).collect::<Vec<_>>().join(" ")
            + " "
            + &self.phones.iter().map(|e| e.value.as_str()).collect::<Vec<_>>().join(" ")
            + " "
            + &self.fediverse.join(" ")
            + " "
            + &self.ssh.join(" ")
            + " "
            + &self.categories.join(" ");
        let hay = hay.to_lowercase();
        q.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
    }

    /// The name to sort by: family name first.
    pub fn sort_key(&self) -> String {
        let k = if self.family.is_empty() { self.name.clone() } else { format!("{} {}", self.family, self.given) };
        k.to_lowercase()
    }

    /// A short one-line summary: name, then the first address or org.
    pub fn summary(&self) -> String {
        let extra = self.email().map(str::to_string).or_else(|| self.org.clone()).unwrap_or_default();
        if extra.is_empty() { self.name.clone() } else { format!("{} <{extra}>", self.name) }
    }
}

/// A random UUID (version 4) for a new card or event.
pub fn new_uid() -> String {
    let mut b = [0u8; 16];
    fill_random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn fill_random(b: &mut [u8]) {
    // Time, a counter and the address of a local, mixed: unique enough for UIDs, which
    // only need to not collide.
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    for (i, chunk) in b.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.write_u64(N.fetch_add(1, Ordering::Relaxed));
        h.write_usize(i);
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "BEGIN:VCARD\r
VERSION:3.0\r
UID:urn:uuid:4fbe8971-0bc3-424c-9c26-36c3e1eff6b1\r
FN:Ada Lovelace\r
N:Lovelace;Ada;;Countess;\r
ORG:Analytical Engines;Research\r
EMAIL;TYPE=INTERNET,WORK,pref:ada@example.org\r
EMAIL;TYPE=home:ada@home.example\r
TEL;TYPE=CELL:+44 20 7946 0000\r
item1.ADR;TYPE=home:;;12 St James's Square;London;;SW1Y 4JH;UK\r
X-SOCIALPROFILE;TYPE=mastodon:https://mastodon.social/@ada\r
IMPP;X-SERVICE-TYPE=Matrix:matrix:u/ada:example.org\r
X-ALOUETTE-SSH:ada@engine.example.org\r
X-CUSTOM:kept\r
PHOTO;ENCODING=b;TYPE=JPEG:AAAA\r
END:VCARD\r
";

    #[test]
    fn reads_and_writes_a_card() {
        let c = &Contact::parse_all(CARD)[0];
        assert_eq!(c.uid, "4fbe8971-0bc3-424c-9c26-36c3e1eff6b1");
        assert_eq!(c.name, "Ada Lovelace");
        assert_eq!((c.family.as_str(), c.given.as_str()), ("Lovelace", "Ada"));
        assert_eq!(c.org.as_deref(), Some("Analytical Engines, Research"));
        assert_eq!(c.email(), Some("ada@example.org"));
        assert_eq!(c.emails[0].kind, "work");
        assert_eq!(c.phones[0], Typed::new("cell", "+44 20 7946 0000"));
        assert_eq!(c.addresses[0].value, "12 St James's Square, London, SW1Y 4JH, UK");
        assert_eq!(c.fediverse, ["@ada@mastodon.social"]);
        assert_eq!(c.chat, ["matrix:u/ada:example.org"]);
        assert_eq!(c.ssh, ["ada@engine.example.org"]);
        assert!(c.matches("ada mastodon"));
        assert!(c.has_email("ADA@example.org"));

        let out = c.to_vcard();
        assert!(out.contains("X-CUSTOM:kept\r\n"));
        assert!(out.contains("PHOTO;ENCODING=b;TYPE=JPEG:AAAA"));
        let back = &Contact::parse_all(&out)[0];
        assert_eq!(back.emails, c.emails);
        assert_eq!(back.fediverse, c.fediverse);
        assert_eq!(back.ssh, c.ssh);
        assert_eq!(back.uid, c.uid);
        assert_eq!(back.addresses[0].value, c.addresses[0].value);
    }

    #[test]
    fn handles() {
        assert_eq!(fediverse_handle("@ada@mastodon.social").as_deref(), Some("@ada@mastodon.social"));
        assert_eq!(fediverse_handle("acct:ada@social.example").as_deref(), Some("@ada@social.example"));
        assert_eq!(fediverse_handle("https://hachyderm.io/@bob").as_deref(), Some("@bob@hachyderm.io"));
        assert_eq!(fediverse_handle("https://gts.example/users/bob").as_deref(), Some("@bob@gts.example"));
        assert_eq!(fediverse_handle("https://example.org/about"), None);
        assert_eq!(fediverse_handle("nope"), None);
        assert_eq!(new_uid().len(), 36);
        assert_ne!(new_uid(), new_uid());
    }

    #[test]
    fn vcard_21() {
        let c = &Contact::parse_all("BEGIN:VCARD\nVERSION:2.1\nN:Smith;John\nTEL;WORK;VOICE:555-1234\nEND:VCARD\n")[0];
        assert_eq!(c.name, "John Smith");
        assert_eq!(c.phones[0].kind, "work");
    }
}
