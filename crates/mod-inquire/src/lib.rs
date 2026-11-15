//! Inquire: one box for the whole internet. A domain, an IP address, an AS number, a
//! `user@host`, a URL or plain words: every source that applies answers at once, and the
//! results stream in as they arrive.

pub use alouette_ui::input;
pub mod inquire;
pub mod report;

use alouette_ui::window::{Cx, Window};
use alouette_ui::{Launcher, Module};
use url::Url;

pub use inquire::InquireWindow;

pub struct InquireModule;

/// The text after `inquire:`, percent-decoded.
pub fn query_of(url: &Url) -> String {
    let raw = url.path().trim_start_matches('/');
    let raw = url.query().map(|q| q.strip_prefix("q=").unwrap_or(q)).unwrap_or(raw);
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| if v.is_empty() { k.into_owned() } else { format!("{k}={v}") })
        .collect()
}

impl Module for InquireModule {
    fn id(&self) -> &'static str {
        inquire::MODULE
    }
    fn name(&self) -> &'static str {
        "Inquire"
    }
    fn description(&self) -> &'static str {
        "Search and look up anything: names, addresses, people, words"
    }
    fn schemes(&self) -> &'static [&'static str] {
        &["inquire"]
    }
    fn launchers(&self) -> Vec<Launcher> {
        vec![
            Launcher::new("New inquiry", "inquire:", "Look up a domain, IP, AS, person, URL or words"),
            Launcher::new("Example: a domain", "inquire:umn.edu", "DNS, RDAP, TLS, certificates and WHOIS"),
            Launcher::new("Example: words", "inquire:minuet", "Dictionaries, Gopherspace and Geminispace"),
        ]
    }
    fn open(&mut self, url: &Url, cx: &mut Cx) -> Result<Box<dyn Window>, String> {
        Ok(Box::new(InquireWindow::new(&query_of(url), cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_queries_from_urls() {
        assert_eq!(query_of(&Url::parse("inquire:example.org").unwrap()), "example.org");
        assert_eq!(query_of(&Url::parse("inquire:?q=university%20of%20minnesota").unwrap()), "university of minnesota");
        assert_eq!(query_of(&Url::parse("inquire:").unwrap()), "");
    }
}
