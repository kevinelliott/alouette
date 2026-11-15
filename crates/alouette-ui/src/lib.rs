//! The Alouette shell.
//!
//! One window manager drives three layouts over the same windows: **Desktop** (Minuet
//! style, overlapping windows with a menu bar), **Tiled** and **Tabs**. Ratatui redraws the
//! whole window tree each frame, so switching layouts at runtime loses no state.
//!
//! Modules plug in through the [`Module`] trait and open [`Window`]s. Everything that opens
//! a URL goes through the core router, so a link in one module can open another.

pub mod app;
pub mod builtin;
pub mod chrome;
pub mod dialog;
pub mod input;
pub mod keymap;
pub mod menu;
pub mod module;
pub mod palette;
pub mod run;
pub mod splash;
pub mod text;
pub mod theme;
pub mod window;
pub mod wm;

pub use app::{App, AppOptions};
pub use keymap::{Action, KeyChord, Keymap};
pub use module::{Launcher, Module};
pub use theme::{ColorMode, Theme};
pub use window::{Cx, DrawCx, Effect, Handled, Window};
