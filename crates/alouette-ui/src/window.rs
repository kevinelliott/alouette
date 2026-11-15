//! The window contract every module implements.

use std::time::Instant;

use alouette_core::{AppEvent, Config, Notification, TaskManager};
use alouette_store::Store;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::keymap::{Action, Keymap};
use crate::theme::Theme;

/// Whether a window used an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    Yes,
    No,
}

impl Handled {
    pub fn yes(self) -> bool {
        self == Handled::Yes
    }
}

impl From<bool> for Handled {
    fn from(b: bool) -> Self {
        if b { Handled::Yes } else { Handled::No }
    }
}

/// Something a window asks the shell to do.
pub enum Effect {
    /// Route a URL through the core router and open it.
    OpenUrl(String),
    /// Open a window that the caller built.
    OpenWindow(Box<dyn Window>),
    /// Close the window that produced this effect.
    CloseSelf,
    /// Show a toast.
    Notify(Notification),
    /// Run a shell action, as if its key had been pressed.
    Action(Action),
    /// Change the config, save it and apply it.
    EditConfig(Box<dyn FnOnce(&mut Config) + Send>),
    /// Open the command palette with this text already typed.
    PaletteWith(String),
    /// Exit Alouette.
    Quit,
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::OpenUrl(u) => write!(f, "OpenUrl({u})"),
            Effect::OpenWindow(w) => write!(f, "OpenWindow({})", w.title()),
            Effect::CloseSelf => f.write_str("CloseSelf"),
            Effect::Notify(n) => write!(f, "Notify({})", n.text),
            Effect::Action(a) => write!(f, "Action({a:?})"),
            Effect::EditConfig(_) => f.write_str("EditConfig(..)"),
            Effect::PaletteWith(s) => write!(f, "PaletteWith({s})"),
            Effect::Quit => f.write_str("Quit"),
        }
    }
}

/// What a window can read while drawing.
pub struct DrawCx<'a> {
    pub theme: &'a Theme,
    pub config: &'a Config,
    pub keymap: &'a Keymap,
    pub tasks: &'a TaskManager,
    pub focused: bool,
    pub now: Instant,
}

/// What a window can use while handling input or events.
pub struct Cx<'a> {
    pub theme: &'a Theme,
    pub config: &'a Config,
    pub keymap: &'a Keymap,
    pub tasks: &'a TaskManager,
    /// The shared network connector. Clone it into background tasks.
    pub net: &'a alouette_net::Net,
    pub store: Option<&'a Store>,
    pub secrets_backend: &'a str,
    pub config_path: &'a std::path::Path,
    pub effects: Vec<Effect>,
}

impl Cx<'_> {
    pub fn open_url(&mut self, url: impl Into<String>) {
        self.effects.push(Effect::OpenUrl(url.into()));
    }
    pub fn notify(&mut self, n: Notification) {
        self.effects.push(Effect::Notify(n));
    }
    pub fn close(&mut self) {
        self.effects.push(Effect::CloseSelf);
    }
    pub fn action(&mut self, a: Action) {
        self.effects.push(Effect::Action(a));
    }
    pub fn edit_config(&mut self, f: impl FnOnce(&mut Config) + Send + 'static) {
        self.effects.push(Effect::EditConfig(Box::new(f)));
    }
}

/// A window inside the shell. Modules implement this for each kind of view.
///
/// The shell draws the frame (border, title, close and zoom boxes); `draw` only fills the
/// inside. Windows get keys before the shell for plain keys, and after the shell for keys
/// with Ctrl, Alt or function keys, so global shortcuts keep working everywhere.
pub trait Window: Send {
    /// The title shown in the frame, tab bar and palette.
    fn title(&self) -> String;

    /// The id of the module that owns this window.
    fn module(&self) -> &'static str;

    /// The URL this window shows, if any. Used for history and to avoid duplicates.
    fn url(&self) -> Option<String> {
        None
    }

    /// Fill the inside of the window.
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx);

    /// Handle a key. Return [`Handled::No`] to let the shell try it.
    fn on_key(&mut self, _key: KeyEvent, _cx: &mut Cx) -> Handled {
        Handled::No
    }

    /// Handle a mouse event inside the window. `area` is the inside of the window.
    fn on_mouse(&mut self, _ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        Handled::No
    }

    /// React to an app event, such as a task update.
    fn on_event(&mut self, _ev: &AppEvent, _cx: &mut Cx) {}

    /// Short key hints for the status line while this window has focus.
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    /// How the window's current connection was made, such as `IPv6 · TLS 1.3 · PQ`.
    /// Shown in the status line while the window has focus.
    fn badge(&self) -> Option<String> {
        None
    }

    /// True when the window wants every key first, including Ctrl and function keys, as a
    /// terminal does. The window returns [`Handled::No`] for keys it hands back to the shell.
    fn captures_keys(&self) -> bool {
        false
    }

    /// Handle pasted text. Return [`Handled::No`] to have it typed key by key.
    fn on_paste(&mut self, _text: &str, _cx: &mut Cx) -> Handled {
        Handled::No
    }

    /// True while the window is typing text, so the shell leaves plain keys alone.
    fn wants_text(&self) -> bool {
        false
    }

    /// Whether the window changes over time without input (spinners, clocks).
    fn animates(&self) -> bool {
        false
    }
}
