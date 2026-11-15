//! The sessions running now, shared by the server, every session and the admin console.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alouette_core::config::Serve;
use tokio::sync::Notify;

use crate::backend::SharedSize;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct Session {
    pub id: u64,
    pub user: String,
    pub guest: bool,
    pub addr: Option<SocketAddr>,
    started: u64,
    last_input: AtomicU64,
    windows: Arc<AtomicUsize>,
    size: SharedSize,
    kill: Notify,
    reason: Mutex<Option<String>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("id", &self.id).field("user", &self.user).field("guest", &self.guest).finish()
    }
}

impl Session {
    pub fn on_for(&self) -> u64 {
        now().saturating_sub(self.started)
    }
    pub fn idle(&self) -> u64 {
        now().saturating_sub(self.last_input.load(Ordering::Relaxed))
    }
    pub fn touch(&self) {
        self.last_input.store(now(), Ordering::Relaxed);
    }
    pub fn windows(&self) -> usize {
        self.windows.load(Ordering::Relaxed)
    }
    /// The counter the session's app keeps up to date.
    pub fn window_counter(&self) -> Arc<AtomicUsize> {
        self.windows.clone()
    }
    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// End the session, saying why.
    pub fn kill(&self, why: &str) {
        *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
        self.kill.notify_one();
    }
    pub async fn killed(&self) {
        self.kill.notified().await
    }
    pub fn reason(&self) -> Option<String> {
        self.reason.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub struct Registry {
    pub limits: Serve,
    sessions: Mutex<Vec<Arc<Session>>>,
    next: AtomicU64,
}

/// Why a new session is refused.
pub fn refusal(limits: &Serve, sessions: &[Arc<Session>], user: &str, guest: bool) -> Option<String> {
    if sessions.len() >= limits.max_sessions {
        return Some(format!("This server is full ({} sessions). Try again later.", limits.max_sessions));
    }
    if guest && sessions.iter().filter(|s| s.guest).count() >= limits.max_guests {
        return Some(format!("All {} guest places are taken. Try again later.", limits.max_guests));
    }
    if !guest && sessions.iter().filter(|s| !s.guest && s.user == user).count() >= limits.sessions_per_user {
        return Some(format!(
            "You already have {} sessions here, the most allowed. Close one first.",
            limits.sessions_per_user
        ));
    }
    None
}

impl Registry {
    pub fn new(limits: Serve) -> Self {
        Self { limits, sessions: Mutex::new(Vec::new()), next: AtomicU64::new(1) }
    }

    /// Add a session, or say why not.
    pub fn join(
        &self,
        user: &str,
        guest: bool,
        addr: Option<SocketAddr>,
        size: SharedSize,
    ) -> Result<Arc<Session>, String> {
        let mut v = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(why) = refusal(&self.limits, &v, user, guest) {
            return Err(why);
        }
        let t = now();
        let s = Arc::new(Session {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            user: user.to_string(),
            guest,
            addr,
            started: t,
            last_input: AtomicU64::new(t),
            windows: Arc::new(AtomicUsize::new(0)),
            size,
            kill: Notify::new(),
            reason: Mutex::new(None),
        });
        v.push(s.clone());
        Ok(s)
    }

    pub fn leave(&self, id: u64) {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.id != id);
    }

    pub fn snapshot(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn count(&self) -> usize {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        let lim = Serve { max_sessions: 4, sessions_per_user: 2, max_guests: 1, ..Serve::default() };
        let r = Registry::new(lim);
        let sz = || Arc::new(Mutex::new((80, 24)));
        let a1 = r.join("ann", false, None, sz()).unwrap();
        r.join("ann", false, None, sz()).unwrap();
        assert!(r.join("ann", false, None, sz()).unwrap_err().contains("already have 2"));
        r.join("guest", true, None, sz()).unwrap();
        assert!(r.join("guest", true, None, sz()).unwrap_err().contains("guest places"));
        r.join("bo", false, None, sz()).unwrap();
        assert!(r.join("cy", false, None, sz()).unwrap_err().contains("full"));
        r.leave(a1.id);
        assert!(r.join("ann", false, None, sz()).is_ok());
    }
}
