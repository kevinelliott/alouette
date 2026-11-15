//! The unified message model.
//!
//! Minuet showed Usenet articles "just like mail messages". Alouette goes further: mail,
//! news articles, feed items, fediverse posts and chat lines all become a [`Message`],
//! so one reader, one search and one set of AI features work on all of them.
//! Modules arriving in later milestones fill these types; M0 only defines them.

use time::OffsetDateTime;

/// Where a message came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Mail,
    Usenet,
    Feed,
    Mastodon,
    Bluesky,
    Irc,
    Matrix,
}

/// A person or account that sent or received a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    /// An email address, a fediverse handle, a Matrix ID, a nick.
    pub address: String,
}

/// One part of a message body.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Html(String),
    Attachment { name: String, mime: String, size: Option<u64> },
}

/// One item in the unified reader.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Stable id within its source, such as an IMAP UID or a Message-ID.
    pub id: String,
    pub source: SourceKind,
    /// The account, server or feed it belongs to.
    pub account: String,
    /// The folder, newsgroup, feed or room.
    pub container: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub subject: Option<String>,
    pub date: Option<OffsetDateTime>,
    /// Ids this message replies to, oldest first. Used to build threads.
    pub references: Vec<String>,
    pub parts: Vec<Part>,
    pub unread: bool,
    pub flagged: bool,
    /// A link back to the original, such as a web URL for a post.
    pub url: Option<String>,
}

/// A conversation: a root message and its replies, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub messages: Vec<Message>,
}

impl Thread {
    pub fn unread(&self) -> usize {
        self.messages.iter().filter(|m| m.unread).count()
    }
}
