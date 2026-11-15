//! The offline cache: message lists and messages on disk, only when the user turns it on
//! (`mail.offline_cache`). Without it nothing about mail is written anywhere.

use std::path::PathBuf;

use alouette_mail::{Flags, Folder, Role, Summary};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct S {
    id: String,
    from: String,
    from_addr: String,
    to: String,
    subject: String,
    date: Option<i64>,
    seen: bool,
    flagged: bool,
    answered: bool,
    draft: bool,
    size: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct F {
    id: String,
    name: String,
    role: String,
    unread: u32,
    total: u32,
}

/// File-name-safe text: letters and digits kept, everything else hex-escaped.
fn safe(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02x}")
            }
        })
        .collect()
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Inbox => "inbox",
        Role::Drafts => "drafts",
        Role::Sent => "sent",
        Role::Archive => "archive",
        Role::Junk => "junk",
        Role::Trash => "trash",
        Role::All => "all",
        Role::Flagged => "flagged",
        Role::Other => "other",
    }
}

impl Cache {
    pub fn new(dir: PathBuf, account: &str) -> Self {
        Self { root: dir.join("mail").join(safe(account)) }
    }

    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root.join(rel);
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }

    pub fn put_folders(&self, f: &[Folder]) {
        let v: Vec<F> = f
            .iter()
            .map(|f| F {
                id: f.id.clone(),
                name: f.name.clone(),
                role: role_name(f.role).into(),
                unread: f.unread,
                total: f.total,
            })
            .collect();
        self.write("folders.json", serde_json::to_string(&v).unwrap_or_default().as_bytes());
    }

    pub fn folders(&self) -> Option<Vec<Folder>> {
        let v: Vec<F> = serde_json::from_slice(&std::fs::read(self.root.join("folders.json")).ok()?).ok()?;
        Some(
            v.into_iter()
                .map(|f| Folder {
                    role: Role::from_flag(&f.role),
                    id: f.id,
                    name: f.name,
                    unread: f.unread,
                    total: f.total,
                })
                .collect(),
        )
    }

    pub fn put_list(&self, folder: &str, l: &[Summary]) {
        let v: Vec<S> = l
            .iter()
            .map(|s| S {
                id: s.id.clone(),
                from: s.from.clone(),
                from_addr: s.from_addr.clone(),
                to: s.to.clone(),
                subject: s.subject.clone(),
                date: s.date,
                seen: s.flags.seen,
                flagged: s.flags.flagged,
                answered: s.flags.answered,
                draft: s.flags.draft,
                size: s.size,
            })
            .collect();
        self.write(&format!("{}/list.json", safe(folder)), serde_json::to_string(&v).unwrap_or_default().as_bytes());
    }

    pub fn list(&self, folder: &str) -> Option<Vec<Summary>> {
        let v: Vec<S> =
            serde_json::from_slice(&std::fs::read(self.root.join(safe(folder)).join("list.json")).ok()?).ok()?;
        Some(
            v.into_iter()
                .map(|s| Summary {
                    id: s.id,
                    from: s.from,
                    from_addr: s.from_addr,
                    to: s.to,
                    subject: s.subject,
                    date: s.date,
                    flags: Flags { seen: s.seen, flagged: s.flagged, answered: s.answered, draft: s.draft },
                    size: s.size,
                })
                .collect(),
        )
    }

    pub fn put_message(&self, folder: &str, id: &str, raw: &[u8]) {
        self.write(&format!("{}/{}.eml", safe(folder), safe(id)), raw);
    }

    pub fn message(&self, folder: &str, id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(safe(folder)).join(format!("{}.eml", safe(id)))).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let d = std::env::temp_dir().join(format!("alouette-mail-cache-{}", std::process::id()));
        let c = Cache::new(d.clone(), "me@example.org");
        assert!(c.list("INBOX").is_none());
        let s = Summary {
            id: "7".into(),
            subject: "Hi".into(),
            flags: Flags { seen: true, ..Default::default() },
            ..Default::default()
        };
        c.put_list("[Gmail]/All Mail", std::slice::from_ref(&s));
        assert_eq!(c.list("[Gmail]/All Mail").unwrap(), vec![s]);
        c.put_message("INBOX", "7", b"raw");
        assert_eq!(c.message("INBOX", "7").unwrap(), b"raw");
        let f = Folder { id: "INBOX".into(), name: "INBOX".into(), role: Role::Inbox, unread: 1, total: 2 };
        c.put_folders(std::slice::from_ref(&f));
        assert_eq!(c.folders().unwrap(), vec![f]);
        let _ = std::fs::remove_dir_all(d);
    }
}
