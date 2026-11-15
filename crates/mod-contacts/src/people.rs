//! Looking up the people in the address books from other modules: who sent this mail,
//! who is this fediverse account.

use std::path::Path;

use alouette_pim::Contact;
use alouette_pim::store::Stores;

/// Every contact in every book, with the book's name and the file it lives in.
pub fn all(data_dir: &Path) -> Vec<(String, String, Contact)> {
    let stores = Stores::new(data_dir);
    let mut out = vec![];
    for book in stores.books() {
        for item in stores.book(&book).list() {
            for c in Contact::parse_all(&item.text) {
                out.push((book.clone(), item.file.clone(), c));
            }
        }
    }
    out
}

/// The contact with this mail address, if there is one.
pub fn by_email(data_dir: &Path, addr: &str) -> Option<Contact> {
    all(data_dir).into_iter().map(|(_, _, c)| c).find(|c| c.has_email(addr))
}

/// The contact with this fediverse handle (`@user@host`), if there is one.
pub fn by_handle(data_dir: &Path, handle: &str) -> Option<Contact> {
    let h = alouette_pim::contact::fediverse_handle(handle)?;
    all(data_dir).into_iter().map(|(_, _, c)| c).find(|c| c.fediverse.iter().any(|f| f.eq_ignore_ascii_case(&h)))
}

/// Addresses for completing a To field: "Name <addr>" for every contact whose name or
/// address starts a word with `prefix`.
pub fn complete(data_dir: &Path, prefix: &str) -> Vec<String> {
    let p = prefix.trim().to_lowercase();
    if p.is_empty() {
        return vec![];
    }
    let mut out: Vec<String> = all(data_dir)
        .into_iter()
        .flat_map(|(_, _, c)| {
            let name = c.name.clone();
            c.emails.into_iter().map(move |e| (name.clone(), e.value))
        })
        .filter(|(n, e)| {
            e.to_lowercase().starts_with(&p) || n.to_lowercase().split_whitespace().any(|w| w.starts_with(&p))
        })
        .map(|(n, e)| if n.is_empty() { e } else { format!("{n} <{e}>") })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The URL that adds someone to the address book, as `contacts:new`.
pub fn add_url(name: &str, email: &str) -> String {
    let mut u = url::Url::parse("contacts:new").expect("static URL");
    u.query_pairs_mut().append_pair("name", name).append_pair("email", email);
    u.to_string()
}
