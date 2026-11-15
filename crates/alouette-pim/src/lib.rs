//! Contacts and calendars: vCard 3/4 and iCalendar (RFC 5545) with recurrence, local
//! stores, CardDAV and CalDAV, LDAP directory lookups and iTIP invitations.

pub mod contact;
pub mod dav;
pub mod event;
pub mod itip;
pub mod ldap;
pub mod line;
pub mod rrule;
pub mod store;

pub use contact::Contact;
pub use event::{Event, When};
pub use line::{Component, Prop};

/// Sent as a module message to "calendar" (and "contacts") when another module changed
/// the stores, so open windows reload.
#[derive(Debug, Clone, Copy)]
pub struct Changed;
