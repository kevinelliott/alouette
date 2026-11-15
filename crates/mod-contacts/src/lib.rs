//! Contacts: the address book. Cards live as .vcf files in Alouette's data folder, sync
//! with CardDAV servers, and link each person to the other modules: mail, the fediverse,
//! SSH hosts and chat. LDAP directories can be searched and their entries saved.
//!
//! URLs:
//! - `contacts:` the address book
//! - `contacts:Ada` the address book, searching for "Ada"
//! - `contacts:new?name=Ada%20Lovelace&email=ada@example.org` a new card, filled in

pub mod account;
pub mod people;
pub mod window;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub const MODULE: &str = "contacts";

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct ContactsModule {
    data_dir: PathBuf,
}

impl ContactsModule {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

/// What a `contacts:` URL asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Open {
    pub query: String,
    /// A new card, with a name and address to start from.
    pub new: Option<(String, String)>,
}

pub fn parse_url(url: &Url) -> Open {
    let path = url.path().trim_matches('/');
    let path = url::form_urlencoded::parse(format!("x={path}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let q = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned()).unwrap_or_default();
    if path == "new" {
        return Open { query: String::new(), new: Some((q("name"), q("email"))) };
    }
    let query = if path.is_empty() { q("q") } else { path };
    Open { query, new: None }
}

impl Module for ContactsModule {
    fn id(&self) -> &'static str {
        MODULE
    }
    fn name(&self) -> &'static str {
        "Contacts"
    }
    fn description(&self) -> &'static str {
        "The address book: CardDAV, LDAP, and links to mail, fediverse and SSH"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["contacts"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![Launcher::new("Contacts", "contacts:", "Your address book; a adds, Enter opens a card")]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        Ok(Box::new(window::ContactsWindow::new(self.data_dir.clone(), parse_url(url), cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = |s: &str| parse_url(&Url::parse(s).unwrap());
        assert_eq!(u("contacts:"), Open::default());
        assert_eq!(u("contacts:Ada%20L").query, "Ada L");
        assert_eq!(u("contacts:?q=bob").query, "bob");
        assert_eq!(
            u(&people::add_url("Ada Lovelace", "ada@example.org")).new,
            Some(("Ada Lovelace".into(), "ada@example.org".into()))
        );
    }
}
