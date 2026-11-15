//! SSH for Alouette, built on russh: reads `~/.ssh/config`, uses the SSH agent, checks
//! `known_hosts`, hops through jump hosts, forwards ports and opens SFTP.

pub mod agent;
pub mod config;
pub mod connect;
pub mod forward;
#[cfg(any(test, feature = "test-server"))]
pub mod testserver;

pub use config::{HostConfig, SshConfig};
pub use connect::{Ask, Options, Prompter, Session, SshError, connect};
