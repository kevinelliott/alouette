//! Each serve-mode user's passwords and tokens, in one file sealed with ChaCha20-Poly1305
//! under a key only the server holds. The system keychain cannot be used: it belongs to the
//! account the server runs as, and every user would share it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alouette_store::secrets::{SecretError, SecretStore};
use aws_lc_rs::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};

pub struct SealedStore {
    path: PathBuf,
    key: [u8; 32],
    map: Mutex<BTreeMap<String, String>>,
}

fn other(e: impl std::fmt::Display) -> SecretError {
    SecretError::Backend(e.to_string())
}

/// Write a file only its owner can read.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        use std::io::Write;
        let mut f = o.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// The server's sealing key, made on first use.
pub fn master_key(root: &Path) -> std::io::Result<[u8; 32]> {
    let p = root.join("secrets.key");
    if let Ok(b) = std::fs::read(&p)
        && b.len() == 32
    {
        let mut k = [0u8; 32];
        k.copy_from_slice(&b);
        return Ok(k);
    }
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).map_err(|e| std::io::Error::other(e.to_string()))?;
    write_private(&p, &k)?;
    Ok(k)
}

impl SealedStore {
    pub fn open(path: PathBuf, key: [u8; 32]) -> Result<Self, SecretError> {
        let map = match std::fs::read(&path) {
            Ok(b) => {
                let plain = open_sealed(&key, &b)?;
                serde_json::from_slice(&plain).map_err(other)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(other(e)),
        };
        Ok(Self { path, key, map: Mutex::new(map) })
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        let plain = serde_json::to_vec(map).map_err(other)?;
        let sealed = seal(&self.key, &plain)?;
        write_private(&self.path, &sealed).map_err(other)
    }
}

fn seal(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, SecretError> {
    let k = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(other)?);
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(other)?;
    let mut data = plain.to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data).map_err(other)?;
    let mut out = nonce.to_vec();
    out.extend(data);
    Ok(out)
}

fn open_sealed(key: &[u8; 32], b: &[u8]) -> Result<Vec<u8>, SecretError> {
    if b.len() < NONCE_LEN {
        return Err(other("the secrets file is damaged"));
    }
    let k = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(other)?);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&b[..NONCE_LEN]);
    let mut data = b[NONCE_LEN..].to_vec();
    let plain = k
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .map_err(|_| other("the secrets file does not open with this server's key"))?;
    Ok(plain.to_vec())
}

impl SecretStore for SealedStore {
    fn backend(&self) -> &'static str {
        "sealed file (serve mode)"
    }
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.map.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(key.to_string(), value.to_string());
        self.save(&m)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(key);
        self.save(&m)
    }
    fn persistent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("alouette-sealed-{}", std::process::id()));
        let key = master_key(&dir).unwrap();
        assert_eq!(master_key(&dir).unwrap(), key);
        let p = dir.join("u/secrets.sealed");
        let s = SealedStore::open(p.clone(), key).unwrap();
        s.set("imap:me", "hunter2").unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(!raw.windows(7).any(|w| w == b"hunter2"));
        let again = SealedStore::open(p.clone(), key).unwrap();
        assert_eq!(again.get("imap:me").unwrap().as_deref(), Some("hunter2"));
        assert!(SealedStore::open(p, [9; 32]).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
