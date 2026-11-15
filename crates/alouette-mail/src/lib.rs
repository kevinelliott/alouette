//! Mail accounts: one [`Backend`] trait over IMAP with SMTP, and JMAP, with sign-in by
//! password or OAuth (Gmail, Microsoft 365). Messages are parsed with `mail-parser` and
//! written with `mail-builder`. Nothing is kept on disk unless the user turns the cache on.

pub mod account;
pub mod imap;
pub mod jmap;
pub mod message;
pub mod oauth;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use account::{Account, Login, Server, Tls};
pub use message::{Draft, Parsed, Summary};

pub type Result<T> = std::result::Result<T, String>;
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// What a folder is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    All,
    Flagged,
    Other,
}

impl Role {
    pub fn from_flag(f: &str) -> Role {
        match f.trim_start_matches('\\').to_ascii_lowercase().as_str() {
            "inbox" => Role::Inbox,
            "drafts" => Role::Drafts,
            "sent" => Role::Sent,
            "archive" => Role::Archive,
            "junk" => Role::Junk,
            "trash" => Role::Trash,
            "all" => Role::All,
            "flagged" => Role::Flagged,
            _ => Role::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// The backend's name for it (IMAP mailbox name or JMAP id).
    pub id: String,
    /// For people, with the hierarchy shown by `/`.
    pub name: String,
    pub role: Role,
    pub unread: u32,
    pub total: u32,
}

/// Flags Alouette shows and changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
}

/// Asks the person something (a password, a code to type into a web page).
pub trait Prompt: Send + Sync {
    fn ask(
        &self,
        title: &str,
        body: Vec<String>,
        prompt: &str,
        secret: bool,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
    /// Tell the person something without waiting (such as "open this URL").
    fn note(&self, text: &str);
}

/// One mail account, ready to use.
pub trait Backend: Send + Sync {
    /// `IMAP · TLS 1.3` and the like.
    fn badge(&self) -> String;
    fn folders(&self) -> BoxFut<'_, Vec<Folder>>;
    /// The newest `limit` messages in a folder, newest first.
    fn list<'a>(&'a self, folder: &'a str, limit: usize) -> BoxFut<'a, Vec<Summary>>;
    /// The whole message.
    fn fetch<'a>(&'a self, folder: &'a str, id: &'a str) -> BoxFut<'a, Vec<u8>>;
    fn set_flags<'a>(&'a self, folder: &'a str, ids: &'a [String], flags: Flags, on: bool) -> BoxFut<'a, ()>;
    fn move_to<'a>(&'a self, folder: &'a str, ids: &'a [String], to: &'a str) -> BoxFut<'a, ()>;
    /// Delete for good (callers move to Trash first, as people expect).
    fn expunge<'a>(&'a self, folder: &'a str, ids: &'a [String]) -> BoxFut<'a, ()>;
    /// Send a finished message to these recipients, and file a copy in Sent where the
    /// server does not do that itself.
    fn send<'a>(&'a self, raw: &'a [u8], from: &'a str, to: &'a [String]) -> BoxFut<'a, ()>;
    /// Keep a draft.
    fn save_draft<'a>(&'a self, raw: &'a [u8]) -> BoxFut<'a, ()>;
    /// Wait until something may have changed in `folder` (or up to `max`).
    fn wait<'a>(&'a self, folder: &'a str, max: std::time::Duration) -> BoxFut<'a, bool>;
}

/// Connect an account: sign in and check the login.
pub async fn connect(
    net: alouette_net::Net,
    account: &alouette_core::config::MailAccount,
    secrets: Arc<dyn alouette_store_like::Secrets>,
    prompt: Arc<dyn Prompt>,
) -> Result<Arc<dyn Backend>> {
    let acct = Account::from_config(account)?;
    match acct.kind {
        alouette_core::config::MailKind::Jmap => Ok(Arc::new(jmap::Jmap::connect(net, acct, secrets, prompt).await?)),
        _ => Ok(Arc::new(imap::ImapSmtp::connect(net, acct, secrets, prompt).await?)),
    }
}

/// The secret store, as this crate needs it (so it does not depend on alouette-store).
pub mod alouette_store_like {
    pub trait Secrets: Send + Sync {
        fn get(&self, key: &str) -> Option<String>;
        fn set(&self, key: &str, value: &str);
        fn delete(&self, key: &str);
    }

    /// Keeps secrets in memory only, for tests and for people without a keychain.
    #[derive(Default)]
    pub struct Memory(std::sync::Mutex<std::collections::HashMap<String, String>>);

    impl Secrets for Memory {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(key.into(), value.into());
        }
        fn delete(&self, key: &str) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        }
    }
}
