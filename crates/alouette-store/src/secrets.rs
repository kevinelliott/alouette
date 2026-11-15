//! Secrets: passwords, OAuth tokens and API keys.
//!
//! Secrets go to the operating system's keychain (macOS Keychain, Windows Credential
//! Manager, the Linux kernel keyring). They never touch the config file, SQLite or logs.
//! Where no keychain works (some headless hosts, SSH serve mode), Alouette falls back to an
//! in-memory store and says so; an encrypted file vault replaces that fallback in a later
//! milestone.

use std::collections::HashMap;
use std::sync::Mutex;

/// The service name every keychain entry is filed under.
pub const SERVICE: &str = "alouette";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Backend(String),
}

/// Where secrets are kept.
pub trait SecretStore: Send + Sync {
    /// A short name for the backend, shown in Settings.
    fn backend(&self) -> &'static str;
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
    /// Whether secrets survive a restart.
    fn persistent(&self) -> bool;
}

/// The OS keychain.
pub struct KeychainStore;

impl KeychainStore {
    fn entry(key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, key).map_err(|e| SecretError::Backend(e.to_string()))
    }

    /// Check the keychain works by writing, reading and deleting a probe entry.
    pub fn probe() -> bool {
        let key = "alouette-probe";
        let ok = Self.set(key, "ok").is_ok() && matches!(Self.get(key), Ok(Some(v)) if v == "ok");
        let _ = Self.delete(key);
        ok
    }
}

impl SecretStore for KeychainStore {
    fn backend(&self) -> &'static str {
        "OS keychain"
    }
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        Self::entry(key)?.set_password(value).map_err(|e| SecretError::Backend(e.to_string()))
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
    fn persistent(&self) -> bool {
        // The Linux kernel keyring is cleared on reboot; the others persist.
        !cfg!(target_os = "linux")
    }
}

/// Secrets held only for this session.
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn backend(&self) -> &'static str {
        "memory (not saved)"
    }
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.map.lock().expect("secret lock").get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.map.lock().expect("secret lock").insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.map.lock().expect("secret lock").remove(key);
        Ok(())
    }
    fn persistent(&self) -> bool {
        false
    }
}

/// Pick the best working backend: the OS keychain, or memory if the keychain fails.
pub fn default_store() -> Box<dyn SecretStore> {
    if KeychainStore::probe() {
        Box::new(KeychainStore)
    } else {
        tracing::warn!("OS keychain unavailable; secrets will only last for this session");
        Box::new(MemoryStore::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips() {
        let s = MemoryStore::default();
        assert_eq!(s.get("imap:me").unwrap(), None);
        s.set("imap:me", "hunter2").unwrap();
        assert_eq!(s.get("imap:me").unwrap().as_deref(), Some("hunter2"));
        s.delete("imap:me").unwrap();
        assert_eq!(s.get("imap:me").unwrap(), None);
        assert!(!s.persistent());
    }
}
