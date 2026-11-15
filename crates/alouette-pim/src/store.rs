//! Local storage: a folder of .vcf or .ics files, one per contact or event, plus an index
//! that remembers where each file lives on a CardDAV or CalDAV server.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// A file's place on the server, and whether it changed here since the last sync.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncEntry {
    pub href: String,
    pub etag: String,
    /// Changed here; to be sent.
    pub dirty: bool,
    /// Deleted here; to be deleted there.
    pub deleted: bool,
}

#[derive(Debug, Clone)]
pub struct Item {
    /// The file name, without the folder.
    pub file: String,
    pub text: String,
}

/// One folder of items.
#[derive(Debug, Clone)]
pub struct Folder {
    pub dir: PathBuf,
    pub ext: &'static str,
}

const INDEX: &str = ".sync";

/// A safe file name for a UID.
pub fn file_for(uid: &str, ext: &str) -> String {
    let mut s: String = uid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') { c } else { '_' })
        .take(120)
        .collect();
    s = s.trim_start_matches('.').to_string();
    if s.is_empty() {
        s = crate::contact::new_uid();
    }
    format!("{s}.{ext}")
}

impl Folder {
    pub fn new(dir: impl Into<PathBuf>, ext: &'static str) -> Self {
        Folder { dir: dir.into(), ext }
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    /// Every item, sorted by file name. A missing folder is empty.
    pub fn list(&self) -> Vec<Item> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return vec![] };
        let mut out: Vec<Item> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || !name.to_ascii_lowercase().ends_with(&format!(".{}", self.ext)) {
                    return None;
                }
                let bytes = std::fs::read(e.path()).ok()?;
                Some(Item { file: name, text: String::from_utf8_lossy(&bytes).into_owned() })
            })
            .collect();
        out.sort_by(|a, b| a.file.cmp(&b.file));
        out
    }

    pub fn read(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.path(file)).ok()
    }

    /// Write a file, safely: to a temporary name first, then renamed over.
    pub fn write(&self, file: &str, text: &str) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{file}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, self.path(file))
    }

    pub fn remove_file(&self, file: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(file)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The sync index: file name to its server entry.
    pub fn index(&self) -> BTreeMap<String, SyncEntry> {
        let mut out = BTreeMap::new();
        let Ok(text) = std::fs::read_to_string(self.dir.join(INDEX)) else { return out };
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 3 {
                continue;
            }
            let flags = f.get(3).copied().unwrap_or("");
            out.insert(
                f[0].to_string(),
                SyncEntry {
                    href: f[1].to_string(),
                    etag: f[2].to_string(),
                    dirty: flags.contains('d'),
                    deleted: flags.contains('x'),
                },
            );
        }
        out
    }

    pub fn save_index(&self, idx: &BTreeMap<String, SyncEntry>) -> io::Result<()> {
        let mut text = String::new();
        for (file, e) in idx {
            let flags = format!("{}{}", if e.dirty { "d" } else { "" }, if e.deleted { "x" } else { "" });
            let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
            text.push_str(&format!("{}\t{}\t{}\t{flags}\n", clean(file), clean(&e.href), clean(&e.etag)));
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(INDEX), text)
    }

    /// Save an item changed here. In a synced folder it is marked to be sent.
    pub fn put_local(&self, file: &str, text: &str) -> io::Result<()> {
        self.write(file, text)?;
        if self.synced() {
            let mut idx = self.index();
            let e = idx.entry(file.to_string()).or_default();
            e.dirty = true;
            e.deleted = false;
            self.save_index(&idx)?;
        }
        Ok(())
    }

    /// Delete an item here. In a synced folder the server copy goes at the next sync.
    pub fn delete_local(&self, file: &str) -> io::Result<()> {
        self.remove_file(file)?;
        if self.synced() {
            let mut idx = self.index();
            match idx.get_mut(file) {
                Some(e) if !e.href.is_empty() => {
                    e.deleted = true;
                    e.dirty = false;
                }
                _ => {
                    idx.remove(file);
                }
            }
            self.save_index(&idx)?;
        }
        Ok(())
    }

    /// Whether this folder mirrors a server collection.
    pub fn synced(&self) -> bool {
        self.dir.join(INDEX).exists()
    }

    /// Start mirroring a server collection (an empty index).
    pub fn mark_synced(&self) -> io::Result<()> {
        if !self.synced() {
            self.save_index(&BTreeMap::new())?;
        }
        Ok(())
    }
}

/// Where contacts and calendars live under Alouette's data folder.
#[derive(Debug, Clone)]
pub struct Stores {
    pub root: PathBuf,
}

impl Stores {
    pub fn new(data_dir: &Path) -> Self {
        Stores { root: data_dir.to_path_buf() }
    }
    /// An address book: "local", or the name of a CardDAV book.
    pub fn book(&self, name: &str) -> Folder {
        Folder::new(self.root.join("contacts").join(safe(name)), "vcf")
    }
    /// A calendar: "local", the name of a CalDAV calendar or an ICS subscription.
    pub fn calendar(&self, name: &str) -> Folder {
        Folder::new(self.root.join("calendars").join(safe(name)), "ics")
    }
    pub fn books(&self) -> Vec<String> {
        subdirs(&self.root.join("contacts"))
    }
    pub fn calendars(&self) -> Vec<String> {
        subdirs(&self.root.join("calendars"))
    }
}

fn safe(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '_' }).collect();
    if s.trim().is_empty() { "local".into() } else { s }
}

fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<String> =
        rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_and_index() {
        let root = std::env::temp_dir().join(format!("alouette-pim-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Stores::new(&root);
        let book = s.book("local");
        assert!(book.list().is_empty());
        book.put_local(&file_for("a/b", "vcf"), "BEGIN:VCARD\r\nEND:VCARD\r\n").unwrap();
        assert_eq!(book.list()[0].file, "a_b.vcf");
        assert!(!book.synced());

        let remote = s.book("work");
        remote.mark_synced().unwrap();
        remote.put_local("x.vcf", "one").unwrap();
        assert!(remote.index()["x.vcf"].dirty);
        // A file never sent is simply forgotten when deleted.
        remote.delete_local("x.vcf").unwrap();
        assert!(remote.index().is_empty());
        let mut idx = remote.index();
        idx.insert("y.vcf".into(), SyncEntry { href: "/dav/y.vcf".into(), etag: "\"1\"".into(), ..Default::default() });
        remote.save_index(&idx).unwrap();
        remote.delete_local("y.vcf").unwrap();
        assert!(remote.index()["y.vcf"].deleted);
        assert_eq!(s.books(), ["local", "work"]);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(file_for("..", "ics").len(), 36 + 4);
    }
}
