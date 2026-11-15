//! The emulator state a backend feeds and a window draws.
//!
//! A backend (local shell, SSH, Telnet) owns the connection. It pushes the bytes it reads into
//! [`Shared::feed`], writes back the replies the emulator produces (answers to "what terminal
//! are you" and "where is the cursor"), and reads keys and resizes from an [`Input`] channel.

use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::mpsc;

/// What the window sends to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Where the session is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting(String),
    Running,
    Exited(String),
}

pub use alouette_ui::ask::Question;

/// Collects what the parser asks the host to do.
#[derive(Default)]
pub struct Hooks {
    pub replies: Vec<u8>,
    pub title: Option<String>,
    pub bell: bool,
    pub clipboard: Option<Vec<u8>>,
}

impl vt100::Callbacks for Hooks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).chars().filter(|c| !c.is_control()).take(120).collect());
    }
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        self.clipboard = Some(data.to_vec());
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            // Primary device attributes: a VT220 with ANSI colour, like xterm says.
            (None, 'c') if first == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            // Secondary device attributes.
            (Some(b'>'), 'c') => self.replies.extend_from_slice(b"\x1b[>1;10;0c"),
            // Device status: OK, and the cursor position.
            (None, 'n') if first == 5 => self.replies.extend_from_slice(b"\x1b[0n"),
            (None, 'n') if first == 6 => {
                let (r, c) = screen.cursor_position();
                self.replies.extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
            }
            // Window size in characters.
            (None, 't') if first == 18 => {
                let (rows, cols) = screen.size();
                self.replies.extend_from_slice(format!("\x1b[8;{rows};{cols}t").as_bytes());
            }
            _ => {}
        }
    }
}

/// The state shared by the window and its backend.
pub struct Shared {
    parser: Mutex<vt100::Parser<Hooks>>,
    status: Mutex<Status>,
    asker: alouette_ui::ask::Asker,
    badge: Mutex<Option<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    /// `wake` asks the UI to redraw; it is called after output arrives.
    pub fn new(rows: u16, cols: u16, scrollback: usize, wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let w2 = wake.clone();
        Arc::new(Self {
            parser: Mutex::new(vt100::Parser::new_with_callbacks(nz(rows), nz(cols), scrollback, Hooks::default())),
            status: Mutex::new(Status::Connecting("Starting".into())),
            asker: alouette_ui::ask::Asker::new(move || w2()),
            badge: Mutex::new(None),
            wake,
        })
    }

    /// Process output from the remote side. Returns bytes the backend must send back.
    pub fn feed(&self, bytes: &[u8]) -> Vec<u8> {
        let replies = {
            let mut p = self.parser();
            p.process(bytes);
            std::mem::take(&mut p.callbacks_mut().replies)
        };
        (self.wake)();
        replies
    }

    /// Write a line of Alouette's own text into the terminal, dimmed.
    pub fn note(&self, text: &str) {
        let line = format!("\r\n\x1b[2m{}\x1b[0m\r\n", text.replace('\n', "\r\n"));
        self.parser().process(line.as_bytes());
        (self.wake)();
    }

    pub fn parser(&self) -> MutexGuard<'_, vt100::Parser<Hooks>> {
        self.parser.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_status(&self, s: Status) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = s;
        (self.wake)();
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Describe the connection for the status line, such as `IPv6 · SSH · curve25519`.
    pub fn set_badge(&self, b: impl Into<String>) {
        *self.badge.lock().unwrap_or_else(|e| e.into_inner()) = Some(b.into());
    }

    pub fn badge(&self) -> Option<String> {
        self.badge.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Ask the person something and wait for the answer. None means they cancelled.
    pub async fn ask(&self, question: Question) -> Option<String> {
        self.asker.ask(question).await
    }

    pub fn asker(&self) -> &alouette_ui::ask::Asker {
        &self.asker
    }

    pub fn question(&self) -> Option<Question> {
        self.asker.question()
    }

    pub fn answer(&self, value: Option<String>) {
        self.asker.answer(value)
    }
}

fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).unwrap_or(std::num::NonZeroU16::MIN)
}

/// The two ends a backend works with.
pub struct Wire {
    pub shared: Arc<Shared>,
    pub input: mpsc::UnboundedReceiver<Input>,
}

/// Resize the emulator grid. Called by the window when its area changes.
pub fn resize(shared: &Shared, rows: u16, cols: u16) {
    shared.parser().set_size(nz(rows), nz(cols));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_device_queries() {
        let s = Shared::new(24, 80, 100, || {});
        assert_eq!(s.feed(b"\x1b[c"), b"\x1b[?62;22c");
        assert_eq!(s.feed(b"\x1b[3;5H\x1b[6n"), b"\x1b[3;5R");
        assert_eq!(s.feed(b"\x1b[18t"), b"\x1b[8;24;80t");
        assert!(s.feed(b"plain text").is_empty());
    }

    #[test]
    fn title_and_bell() {
        let s = Shared::new(24, 80, 100, || {});
        s.feed(b"\x1b]0;hello\x07\x07");
        let mut p = s.parser();
        assert_eq!(p.callbacks().title.as_deref(), Some("hello"));
        assert!(p.callbacks_mut().bell);
    }

    #[tokio::test]
    async fn questions_round_trip() {
        let s = Shared::new(24, 80, 100, || {});
        let s2 = s.clone();
        let h = tokio::spawn(async move {
            s2.ask(Question { title: "t".into(), body: vec![], prompt: "Password".into(), secret: true, yes_no: false })
                .await
        });
        while s.question().is_none() {
            tokio::task::yield_now().await;
        }
        s.answer(Some("hunter2".into()));
        assert_eq!(h.await.unwrap().as_deref(), Some("hunter2"));
    }
}
