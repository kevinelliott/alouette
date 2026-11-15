//! Alouette's terminal: a VT220/xterm emulator in a window, fed by a backend.
//!
//! [`open`] wires a [`TermWindow`] to a backend future that runs as a background task, so a
//! session shows in Tasks and stops when its window closes.

pub mod emu;
pub mod keys;
pub mod local;
pub mod telnet;
pub mod window;

use std::future::Future;
use std::sync::{Arc, Mutex};

use alouette_core::AppEvent;
use alouette_core::event::ModulePayload;
use alouette_ui::window::Cx;

pub use emu::{Input, Question, Shared, Status, Wire};
pub use window::TermWindow;

/// Woken after terminal output so the screen redraws.
#[derive(Debug, Clone, Copy)]
pub struct Redraw;

/// Open a terminal window whose backend is `run`. The backend gets a [`Wire`] once; a retry
/// from Tasks reopens nothing (close and reopen the window instead).
pub fn open<F, Fut>(
    cx: &mut Cx,
    module: &'static str,
    title: impl Into<String>,
    url: impl Into<String>,
    run: F,
) -> TermWindow
where
    F: FnOnce(Wire) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let title = title.into();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let events = cx.tasks.events();
    let wake = move || {
        // The app drains queued events before drawing, so a burst costs one frame.
        let _ = events.send(AppEvent::Module { module, payload: ModulePayload::new(Redraw) });
    };
    let scrollback = cx.config.terminal.scrollback;
    let shared = Shared::new(24, 80, scrollback, wake);
    let once = Arc::new(Mutex::new(Some((run, Wire { shared: shared.clone(), input: rx }))));
    cx.tasks.spawn(module, title.clone(), move |_ctx| {
        let taken = once.lock().unwrap_or_else(|e| e.into_inner()).take();
        async move {
            match taken {
                Some((run, wire)) => run(wire).await,
                None => Err("Close this window and open the session again".into()),
            }
        }
    });
    TermWindow::new(module, title, url, shared, tx)
}
