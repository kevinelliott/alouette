//! The application: state, input handling, actions and drawing.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use alouette_core::config::{KeymapPreset, LayoutKind};
use alouette_core::event::{self, EventReceiver, EventSender};
use alouette_core::{AppEvent, Config, Notification, Router, TaskManager};
use alouette_store::{SecretStore, Store};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use time::UtcOffset;

use crate::chrome::{self, FrameSpec};
use crate::dialog::{Dialog, DialogOutcome, Purpose};
use crate::keymap::{Action, KeyChord, Keymap, Lookup};
use crate::menu::{self, Menu, MenuCmd, MenuState};
use crate::module::Module;
use crate::palette::{Palette, PaletteCmd, PaletteItem, PaletteOutcome};
use crate::splash::Splash;
use crate::theme::{ColorMode, Theme, ThemeSet};
use crate::window::{Cx, DrawCx, Effect, Window};
use crate::wm::{self, Dir, WinId, Wm};

/// Everything needed to build an [`App`].
pub struct AppOptions {
    pub config: Config,
    pub config_path: PathBuf,
    pub themes: ThemeSet,
    pub store: Option<Store>,
    pub secrets: Box<dyn SecretStore>,
    pub modules: Vec<Box<dyn Module>>,
    pub color_mode: ColorMode,
    pub show_splash: bool,
    pub local_offset: UtcOffset,
    /// Freeze the clock and tips, for snapshot tests and screen dumps.
    pub deterministic: bool,
}

/// An in-progress mouse drag on the desktop.
#[derive(Debug, Clone, Copy)]
enum Drag {
    Move { id: WinId, dx: u16, dy: u16 },
    Resize { id: WinId },
}

pub struct App {
    pub config: Config,
    config_path: PathBuf,
    themes: ThemeSet,
    color_mode: ColorMode,
    pub theme: Theme,
    pub keymap: Keymap,
    pub wm: Wm,
    menus: Vec<Menu>,
    menu: MenuState,
    palette: Option<Palette>,
    dialog: Option<Dialog>,
    notifications: Vec<Notification>,
    pub tasks: TaskManager,
    pub net: alouette_net::Net,
    router: Router,
    modules: Vec<Box<dyn Module>>,
    store: Option<Store>,
    secrets: Box<dyn SecretStore>,
    events: EventSender,
    splash: Option<Splash>,
    pending: Vec<KeyChord>,
    move_mode: bool,
    drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16)>,
    tab_hits: Vec<(Rect, WinId)>,
    screen: Rect,
    local_offset: UtcOffset,
    deterministic: bool,
    last_minute: i64,
    pub dirty: bool,
    pub quit: bool,
}

/// The connector for these settings, or the defaults plus an error to show.
fn build_net(cfg: &alouette_core::config::Network) -> (alouette_net::Net, Option<String>) {
    match alouette_net::Net::new(cfg) {
        Ok(n) => (n, None),
        Err(e) => {
            let fallback = alouette_net::Net::new(&Default::default()).expect("default network settings are valid");
            (fallback, Some(format!("{e}. Using direct connections and system DNS.")))
        }
    }
}

const MENU_ROW: u16 = 1;
const STATUS_ROW: u16 = 1;

impl App {
    /// Build the app and the receiving end of its event channel.
    pub fn new(opts: AppOptions) -> (Self, EventReceiver) {
        let (tx, rx) = event::channel();
        let mut router = Router::new();
        for m in &opts.modules {
            for s in m.schemes() {
                router.register(s, m.id());
            }
        }
        let keymap = Keymap::new(opts.config.general.keymap, &opts.config.keys);
        let theme = opts.themes.resolve(&opts.config.general.theme).degraded(opts.color_mode);
        let splash = (opts.show_splash && opts.config.nostalgia.splash())
            .then(|| Splash::new(Duration::from_millis(opts.config.nostalgia.splash_ms.min(5000))));
        let mut app = Self {
            wm: Wm::new(opts.config.general.layout),
            menus: vec![],
            menu: MenuState::default(),
            palette: None,
            dialog: None,
            notifications: vec![],
            tasks: TaskManager::new(tx.clone()),
            net: build_net(&opts.config.network).0,
            router,
            modules: opts.modules,
            store: opts.store,
            secrets: opts.secrets,
            events: tx,
            splash,
            pending: vec![],
            move_mode: false,
            drag: None,
            last_click: None,
            tab_hits: vec![],
            screen: Rect::new(0, 0, 80, 24),
            local_offset: opts.local_offset,
            deterministic: opts.deterministic,
            last_minute: 0,
            dirty: true,
            quit: false,
            theme,
            keymap,
            themes: opts.themes,
            color_mode: opts.color_mode,
            config: opts.config,
            config_path: opts.config_path,
        };
        if let Some(e) = build_net(&app.config.network).1 {
            app.notify(Notification::error(e));
        }
        for e in app.themes.errors.clone() {
            app.notify(Notification::warning(e));
        }
        for e in app.keymap.errors.clone() {
            app.notify(Notification::warning(e));
        }
        app.rebuild_menus();
        (app, rx)
    }

    /// A sender other code can use to wake the app.
    pub fn sender(&self) -> EventSender {
        self.events.clone()
    }

    pub fn secrets_backend(&self) -> &'static str {
        self.secrets.backend()
    }

    pub fn set_screen(&mut self, area: Rect) {
        self.screen = area;
    }

    fn desktop_area(&self) -> Rect {
        let s = self.screen;
        Rect { x: s.x, y: s.y + MENU_ROW, width: s.width, height: s.height.saturating_sub(MENU_ROW + STATUS_ROW) }
    }

    fn rebuild_menus(&mut self) {
        let names = self.config.nostalgia.minuet_names();
        let mods: Vec<(String, Vec<crate::module::Launcher>)> = self
            .modules
            .iter()
            .filter(|m| m.id() != "core")
            .map(|m| {
                let name = if names { alouette_nostalgia::minuet_name(m.id()).unwrap_or(m.name()) } else { m.name() };
                (name.to_string(), m.launchers())
            })
            .collect();
        self.menus = menu::build(&self.keymap, self.wm.layout, &mods);
    }

    pub fn notify(&mut self, n: Notification) {
        self.notifications.push(n);
        if self.notifications.len() > 20 {
            self.notifications.remove(0);
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------ config

    /// Apply a new config: theme, keymap, layout and menus follow it at once.
    pub fn apply_config(&mut self, cfg: Config) {
        if cfg == self.config {
            return;
        }
        let keymap_changed = cfg.general.keymap != self.config.general.keymap || cfg.keys != self.config.keys;
        self.theme = self.themes.resolve(&cfg.general.theme).degraded(self.color_mode);
        if self.themes.get(&cfg.general.theme).is_none() {
            self.notify(Notification::warning(format!("Unknown theme `{}`; using minuet", cfg.general.theme)));
        }
        if keymap_changed {
            self.keymap = Keymap::new(cfg.general.keymap, &cfg.keys);
            for e in self.keymap.errors.clone() {
                self.notify(Notification::warning(e));
            }
        }
        if cfg.network != self.config.network {
            let (net, err) = build_net(&cfg.network);
            self.net = net;
            if let Some(e) = err {
                self.notify(Notification::error(e));
            }
        }
        self.wm.layout = cfg.general.layout;
        self.config = cfg;
        self.rebuild_menus();
        self.dirty = true;
    }

    /// Change the config, save it, and apply it.
    pub fn edit_config(&mut self, f: impl FnOnce(&mut Config)) {
        let mut cfg = self.config.clone();
        f(&mut cfg);
        if let Err(e) = cfg.save(&self.config_path) {
            self.notify(Notification::error(e.to_string()));
        }
        self.apply_config(cfg);
    }

    // ------------------------------------------------------------------ opening

    /// Open each URL, as at startup.
    pub fn open_all(&mut self, urls: &[String]) {
        for u in urls {
            self.open_url(u);
        }
    }

    /// Route a URL to its module and open (or focus) its window.
    pub fn open_url(&mut self, input: &str) {
        let route = match self.router.resolve(input) {
            Ok(r) => r,
            Err(e) => {
                let hint = match &e {
                    alouette_core::router::RouteError::NoHandler(s) => {
                        coming_in(s).map(|m| format!(" It arrives in {m}."))
                    }
                    _ => None,
                };
                self.notify(Notification::warning(format!("{e}.{}", hint.unwrap_or_default())));
                return;
            }
        };
        let url_s = route.url.to_string();
        if let Some(id) = self.wm.find_url(&url_s) {
            self.wm.focus(id);
            self.dirty = true;
            return;
        }
        let Some(idx) = self.modules.iter().position(|m| m.id() == route.module) else { return };
        let (result, effects) = {
            let App { modules, theme, config, keymap, tasks, net, store, secrets, config_path, .. } = self;
            let mut cx = Cx {
                theme,
                config,
                keymap,
                tasks,
                net,
                store: store.as_ref(),
                secrets_backend: secrets.backend(),
                config_path,
                effects: vec![],
            };
            let r = modules[idx].open(&route.url, &mut cx);
            (r, cx.effects)
        };
        match result {
            Ok(win) => {
                let title = win.title();
                let area = self.desktop_area();
                self.wm.add(win, area);
                if let Some(store) = &self.store
                    && let Err(e) = store.add_history(&url_s, &title, &route.module)
                {
                    tracing::warn!("could not record history: {e}");
                }
            }
            Err(e) => self.notify(Notification::warning(e)),
        }
        self.apply_effects(effects, None);
        self.dirty = true;
    }

    pub fn open_window(&mut self, win: Box<dyn Window>) {
        let area = self.desktop_area();
        self.wm.add(win, area);
        self.dirty = true;
    }

    // ------------------------------------------------------------------ effects

    fn apply_effects(&mut self, effects: Vec<Effect>, source: Option<WinId>) {
        for e in effects {
            match e {
                Effect::OpenUrl(u) => self.open_url(&u),
                Effect::OpenWindow(w) => self.open_window(w),
                Effect::CloseSelf => {
                    if let Some(id) = source {
                        self.wm.remove(id);
                    }
                }
                Effect::Notify(n) => self.notify(n),
                Effect::Action(a) => self.do_action(a),
                Effect::EditConfig(f) => self.edit_config(f),
                Effect::PaletteWith(q) => self.open_palette(&q),
                Effect::Quit => self.request_quit(),
            }
        }
        self.dirty = true;
    }

    /// Run a closure on one window with a fresh context, then apply its effects.
    fn with_window<R>(&mut self, id: WinId, f: impl FnOnce(&mut dyn Window, &mut Cx) -> R) -> Option<R> {
        let (r, effects) = {
            let App { wm, theme, config, keymap, tasks, net, store, secrets, config_path, .. } = self;
            let m = wm.get_mut(id)?;
            let mut cx = Cx {
                theme,
                config,
                keymap,
                tasks,
                net,
                store: store.as_ref(),
                secrets_backend: secrets.backend(),
                config_path,
                effects: vec![],
            };
            let r = f(m.win.as_mut(), &mut cx);
            (r, cx.effects)
        };
        self.apply_effects(effects, Some(id));
        Some(r)
    }

    // ------------------------------------------------------------------ events

    pub fn handle_event(&mut self, ev: AppEvent) {
        match &ev {
            AppEvent::ConfigReloaded(cfg) => {
                self.apply_config((**cfg).clone());
                return;
            }
            AppEvent::ConfigError(e) => {
                self.notify(Notification::error(e.clone()));
                return;
            }
            AppEvent::OpenUrl(u) => {
                let u = u.clone();
                self.open_url(&u);
                return;
            }
            AppEvent::Notify(n) => {
                self.notify(n.clone());
                return;
            }
            AppEvent::Quit => {
                self.quit = true;
                return;
            }
            AppEvent::TaskUpdated(_) | AppEvent::Module { .. } => {}
        }
        let ids: Vec<WinId> = self.wm.ordered().map(|m| m.id).collect();
        for id in ids {
            self.with_window(id, |w, cx| w.on_event(&ev, cx));
        }
        self.dirty = true;
    }

    /// Called a few times a second for clocks, spinners and toasts.
    pub fn on_tick(&mut self) {
        let now = Instant::now();
        if let Some(s) = &self.splash {
            if s.done(now) {
                self.splash = None;
            }
            self.dirty = true;
        }
        let before = self.notifications.len();
        self.notifications.retain(|n| !n.expired(now));
        if self.notifications.len() != before {
            self.dirty = true;
        }
        if self.tasks.running() > 0 || self.wm.ordered().any(|m| m.win.animates()) {
            self.dirty = true;
        }
        let minute = time::OffsetDateTime::now_utc().unix_timestamp() / 60;
        if minute != self.last_minute {
            self.last_minute = minute;
            self.dirty = true;
        }
    }

    pub fn handle_input(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Resize(w, h) => {
                self.screen = Rect::new(0, 0, w, h);
                self.dirty = true;
            }
            Event::Paste(text) => self.handle_paste(&text),
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    // ------------------------------------------------------------------ keys

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.dirty = true;
        if self.splash.take().is_some() {
            return;
        }
        if let Some(d) = &mut self.dialog {
            match d.on_key(key) {
                DialogOutcome::Continue => {}
                DialogOutcome::Cancel | DialogOutcome::No => self.dialog = None,
                DialogOutcome::Submit(v) => {
                    let purpose = self.dialog.take().map(|d| d.purpose);
                    if purpose == Some(Purpose::OpenUrl) && !v.trim().is_empty() {
                        self.open_url(v.trim());
                    }
                }
                DialogOutcome::Yes => {
                    if self.dialog.take().map(|d| d.purpose) == Some(Purpose::ConfirmQuit) {
                        self.quit = true;
                    }
                }
            }
            return;
        }
        if let Some(p) = &mut self.palette {
            match p.on_key(key) {
                PaletteOutcome::Continue => {}
                PaletteOutcome::Close => self.palette = None,
                PaletteOutcome::Run(cmd) => {
                    self.palette = None;
                    self.run_palette(cmd);
                }
            }
            return;
        }
        if self.menu.is_open() {
            self.menu_key(key);
            return;
        }
        if self.move_mode {
            self.move_key(key);
            return;
        }

        let chord = KeyChord::from_event(&key);
        let special =
            key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) || matches!(key.code, KeyCode::F(_));
        let wants_text = self.wm.focused().and_then(|id| self.wm.get(id)).map(|m| m.win.wants_text()).unwrap_or(false);

        if !self.pending.is_empty() || special {
            self.pending.push(chord);
            match self.keymap.lookup(&self.pending) {
                Lookup::Action(a) => {
                    self.pending.clear();
                    self.do_action(a);
                    return;
                }
                Lookup::Pending => return,
                Lookup::None => {
                    let was_sequence = self.pending.len() > 1;
                    self.pending.clear();
                    if was_sequence {
                        return;
                    }
                }
            }
            // Alt plus a letter opens the matching menu.
            if key.modifiers == KeyModifiers::ALT
                && let KeyCode::Char(c) = key.code
                && let Some(i) = self.menus.iter().position(|m| m.hotkey() == Some(c.to_ascii_lowercase()))
            {
                self.menu.open_menu(&self.menus, i);
                return;
            }
            if let Some(id) = self.wm.focused() {
                self.with_window(id, |w, cx| w.on_key(key, cx));
            }
            return;
        }

        // Plain keys: the window first, then the keymap.
        let handled = match self.wm.focused() {
            Some(id) => self.with_window(id, |w, cx| w.on_key(key, cx)).map(|h| h.yes()).unwrap_or(false),
            None => false,
        };
        if handled || wants_text {
            return;
        }
        match self.keymap.lookup(&[chord]) {
            Lookup::Action(a) => self.do_action(a),
            Lookup::Pending => self.pending.push(chord),
            Lookup::None => {
                if key.code == KeyCode::Esc && self.wm.layout == LayoutKind::Tiled && self.wm.tiled_zoom {
                    self.wm.tiled_zoom = false;
                }
            }
        }
    }

    fn menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => self.menu.close(),
            KeyCode::Left => self.menu.move_menu(&self.menus, -1),
            KeyCode::Right => self.menu.move_menu(&self.menus, 1),
            KeyCode::Up => self.menu.move_item(&self.menus, false),
            KeyCode::Down | KeyCode::Tab => self.menu.move_item(&self.menus, true),
            KeyCode::Enter => {
                if let Some(item) = self.menu.current(&self.menus).cloned() {
                    self.menu.close();
                    if let Some(cmd) = item.cmd {
                        self.run_menu(cmd);
                    }
                }
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(i) = self.menus.iter().position(|m| m.hotkey() == Some(c.to_ascii_lowercase())) {
                    self.menu.open_menu(&self.menus, i);
                }
            }
            KeyCode::Char(c) => {
                let Some(open) = self.menu.open else { return };
                let item =
                    self.menus[open].items.iter().find(|it| it.hotkey() == Some(c.to_ascii_lowercase())).cloned();
                if let Some(cmd) = item.and_then(|it| it.cmd) {
                    self.menu.close();
                    self.run_menu(cmd);
                }
            }
            _ => {}
        }
    }

    fn move_key(&mut self, key: KeyEvent) {
        let area = self.desktop_area();
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match (key.code, shift) {
            (KeyCode::Left, false) => self.wm.nudge(area, -1, 0),
            (KeyCode::Right, false) => self.wm.nudge(area, 1, 0),
            (KeyCode::Up, false) => self.wm.nudge(area, 0, -1),
            (KeyCode::Down, false) => self.wm.nudge(area, 0, 1),
            (KeyCode::Left, true) => self.wm.grow(area, -1, 0),
            (KeyCode::Right, true) => self.wm.grow(area, 1, 0),
            (KeyCode::Up, true) => self.wm.grow(area, 0, -1),
            (KeyCode::Down, true) => self.wm.grow(area, 0, 1),
            (KeyCode::Enter | KeyCode::Esc, _) => self.move_mode = false,
            _ => {}
        }
    }

    fn run_menu(&mut self, cmd: MenuCmd) {
        match cmd {
            MenuCmd::Action(a) => self.do_action(a),
            MenuCmd::Open(u) => self.open_url(&u),
            MenuCmd::Palette(q) => self.open_palette(&q),
        }
    }

    fn run_palette(&mut self, cmd: PaletteCmd) {
        match cmd {
            PaletteCmd::Action(a) => self.do_action(a),
            PaletteCmd::Open(u) => self.open_url(&u),
            PaletteCmd::Focus(id) => {
                self.wm.focus(id);
            }
            PaletteCmd::Theme(name) => self.edit_config(|c| c.general.theme = name),
            PaletteCmd::Keymap(k) => {
                self.edit_config(|c| c.general.keymap = k);
                self.notify(Notification::info(format!("Keymap: {}", k.name())));
            }
            PaletteCmd::Layout(l) => self.edit_config(|c| c.general.layout = l),
        }
    }

    fn request_quit(&mut self) {
        let running = self.tasks.running();
        if running > 0 {
            self.dialog = Some(Dialog::confirm(
                Purpose::ConfirmQuit,
                "Exit Alouette",
                &format!("{running} task{} still running. Exit anyway?", if running == 1 { " is" } else { "s are" }),
            ));
        } else {
            self.quit = true;
        }
    }

    pub fn open_palette(&mut self, query: &str) {
        self.menu.close();
        self.palette = Some(Palette::new(self.palette_items(), query));
        self.dirty = true;
    }

    /// Perform a shell action.
    pub fn do_action(&mut self, a: Action) {
        self.dirty = true;
        let area = self.desktop_area();
        if let Some(n) = a.window_number() {
            if let Some(id) = self.wm.nth(n) {
                self.wm.focus(id);
            }
            return;
        }
        match a {
            Action::Quit => self.request_quit(),
            Action::Palette => self.open_palette(""),
            Action::OpenUrl => {
                self.dialog = Some(Dialog::input(
                    Purpose::OpenUrl,
                    "Open URL",
                    "Type a URL or an address. Examples: about:tasks, demo:jobs, gemini://geminiprotocol.net/",
                    "",
                    "about:welcome",
                ));
            }
            Action::Menu => {
                if self.menu.is_open() {
                    self.menu.close();
                } else {
                    self.menu.open_menu(&self.menus, 1);
                }
            }
            Action::NextWindow => self.wm.cycle(true),
            Action::PrevWindow => self.wm.cycle(false),
            Action::CloseWindow => {
                if let Some(id) = self.wm.focused() {
                    self.wm.remove(id);
                }
            }
            Action::ZoomWindow => self.wm.toggle_zoom(area),
            Action::MoveWindow => {
                if self.wm.layout == LayoutKind::Desktop && self.wm.focused().is_some() {
                    self.move_mode = true;
                } else {
                    self.notify(Notification::info("Moving windows works in the Desktop layout"));
                }
            }
            Action::CycleLayout => {
                let next = self.wm.layout.next();
                self.set_layout(next);
            }
            Action::LayoutDesktop => self.set_layout(LayoutKind::Desktop),
            Action::LayoutTiled => self.set_layout(LayoutKind::Tiled),
            Action::LayoutTabs => self.set_layout(LayoutKind::Tabs),
            Action::FocusLeft => self.wm.focus_dir(area, Dir::Left),
            Action::FocusRight => self.wm.focus_dir(area, Dir::Right),
            Action::FocusUp => self.wm.focus_dir(area, Dir::Up),
            Action::FocusDown => self.wm.focus_dir(area, Dir::Down),
            Action::GrowMaster => self.wm.adjust_master(0.05),
            Action::ShrinkMaster => self.wm.adjust_master(-0.05),
            Action::Cascade => {
                self.wm.cascade_all(area);
                if self.wm.layout != LayoutKind::Desktop {
                    self.set_layout(LayoutKind::Desktop);
                }
            }
            Action::Tasks => self.open_url("about:tasks"),
            Action::Help => self.open_url("about:help"),
            Action::Keys => self.open_url("about:keys"),
            Action::Settings => self.open_url("about:settings"),
            Action::History => self.open_url("about:history"),
            Action::About => self.open_url("about:alouette"),
            Action::CycleTheme => {
                let next = self.themes.next_after(&self.config.general.theme).name.clone();
                self.notify(Notification::info(format!("Theme: {next}")));
                self.edit_config(|c| c.general.theme = next);
            }
            Action::Redraw => {}
            _ => {}
        }
    }

    fn set_layout(&mut self, layout: LayoutKind) {
        self.wm.layout = layout;
        self.wm.tiled_zoom = false;
        self.move_mode = false;
        self.edit_config(|c| c.general.layout = layout);
        self.rebuild_menus();
    }

    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = vec![];
        for (n, m) in self.wm.ordered().enumerate() {
            items.push(PaletteItem::new("Window", m.win.title(), format!("window {}", n + 1), PaletteCmd::Focus(m.id)));
        }
        for a in Action::ALL.iter().filter(|a| a.window_number().is_none()) {
            items.push(PaletteItem::new(
                "Command",
                a.label(),
                self.keymap.key_for(*a).unwrap_or_default(),
                PaletteCmd::Action(*a),
            ));
        }
        let names = self.config.nostalgia.minuet_names();
        for m in &self.modules {
            let name = if names { alouette_nostalgia::minuet_name(m.id()).unwrap_or(m.name()) } else { m.name() };
            for l in m.launchers() {
                items.push(PaletteItem::new(
                    "Module",
                    format!("{name}: {}", l.label),
                    l.url.clone(),
                    PaletteCmd::Open(l.url),
                ));
            }
        }
        for t in self.themes.names() {
            let mark = if t == self.config.general.theme { " (current)" } else { "" };
            items.push(PaletteItem::new("Theme", format!("theme {t}"), mark.trim(), PaletteCmd::Theme(t.to_string())));
        }
        for k in KeymapPreset::ALL {
            items.push(PaletteItem::new("Keymap", format!("keymap {}", k.name()), "", PaletteCmd::Keymap(k)));
        }
        for l in LayoutKind::ALL {
            items.push(PaletteItem::new("Layout", format!("layout {}", l.name()), "", PaletteCmd::Layout(l)));
        }
        if self.config.nostalgia.easter_eggs() {
            items.push(PaletteItem::new(
                "Retro",
                "minuet",
                "The story of Minuet",
                PaletteCmd::Open("about:minuet".into()),
            ));
            for name in ["archie", "veronica", "jughead", "wais"] {
                items.push(PaletteItem::new(
                    "Retro",
                    name,
                    "Search arrives with Inquire in M1",
                    PaletteCmd::Open("about:minuet".into()),
                ));
            }
        }
        if let Some(store) = &self.store {
            for h in store.recent_history(30).unwrap_or_default() {
                items.push(PaletteItem::new("History", h.url.clone(), h.title, PaletteCmd::Open(h.url)));
            }
        }
        items
    }

    // ------------------------------------------------------------------ mouse

    fn handle_mouse(&mut self, ev: MouseEvent) {
        self.dirty = true;
        if self.splash.take().is_some() || self.dialog.is_some() || self.palette.is_some() {
            if self.palette.is_some() && matches!(ev.kind, MouseEventKind::Down(_)) {
                self.palette = None;
            }
            return;
        }
        let (x, y) = (ev.column, ev.row);
        let bar = Rect { height: MENU_ROW, ..self.screen };

        if self.menu.is_open() {
            if let MouseEventKind::Down(MouseButton::Left) = ev.kind {
                let open = self.menu.open.unwrap_or(0);
                let drop = menu::dropdown_rect(&self.menus, open, bar, self.screen);
                if wm::contains(drop, x, y) {
                    let row = y.saturating_sub(drop.y + 1) as usize;
                    if let Some(item) = self.menus[open].items.get(row).cloned()
                        && let Some(cmd) = item.cmd
                    {
                        self.menu.close();
                        self.run_menu(cmd);
                    }
                    return;
                }
                if let Some(i) = menu::title_positions(&self.menus, bar).iter().position(|r| wm::contains(*r, x, y)) {
                    if i == open {
                        self.menu.close()
                    } else {
                        self.menu.open_menu(&self.menus, i)
                    }
                    return;
                }
                self.menu.close();
            }
            return;
        }

        if y < self.screen.y + MENU_ROW {
            if let MouseEventKind::Down(MouseButton::Left) = ev.kind
                && let Some(i) = menu::title_positions(&self.menus, bar).iter().position(|r| wm::contains(*r, x, y))
            {
                self.menu.open_menu(&self.menus, i);
            }
            return;
        }

        let area = self.desktop_area();
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.wm.layout == LayoutKind::Tabs && y == area.y {
                    if let Some((_, id)) = self.tab_hits.iter().find(|(r, _)| wm::contains(*r, x, y)) {
                        self.wm.focus(*id);
                    }
                    return;
                }
                let Some((id, rect)) = self.wm.hit(area, x, y) else { return };
                self.wm.focus(id);
                let double = self
                    .last_click
                    .map(|(t, lx, ly)| t.elapsed() < Duration::from_millis(400) && lx == x && ly == y)
                    .unwrap_or(false);
                self.last_click = Some((Instant::now(), x, y));
                if self.wm.layout == LayoutKind::Desktop && y == rect.y {
                    // Title bar: close box, zoom box, or start a drag.
                    if rect.width >= 20 && x >= rect.x + 2 && x < rect.x + 5 {
                        self.wm.remove(id);
                        return;
                    }
                    if rect.width >= 20 && x >= rect.right().saturating_sub(5) && x < rect.right().saturating_sub(2) {
                        self.wm.toggle_zoom(area);
                        return;
                    }
                    if double {
                        self.wm.toggle_zoom(area);
                        return;
                    }
                    self.drag = Some(Drag::Move { id, dx: x - rect.x, dy: y - rect.y });
                    return;
                }
                if self.wm.layout == LayoutKind::Desktop
                    && y == rect.bottom() - 1
                    && x >= rect.right().saturating_sub(2)
                {
                    self.drag = Some(Drag::Resize { id });
                    return;
                }
                self.forward_mouse(id, rect, ev);
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Move { id, dx, dy }) => {
                    if let Some(m) = self.wm.get_mut(id) {
                        m.restore = None;
                        let nx = x.saturating_sub(dx).max(area.x).min(area.right().saturating_sub(4));
                        let ny = y.saturating_sub(dy).max(area.y).min(area.bottom().saturating_sub(1));
                        m.float.x = nx;
                        m.float.y = ny;
                    }
                }
                Some(Drag::Resize { id }) => {
                    if let Some(m) = self.wm.get_mut(id) {
                        m.restore = None;
                        m.float.width = (x + 1).saturating_sub(m.float.x).max(wm::MIN_W).min(area.right() - m.float.x);
                        m.float.height =
                            (y + 1).saturating_sub(m.float.y).max(wm::MIN_H).min(area.bottom() - m.float.y);
                    }
                }
                None => {}
            },
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::Down(_) => {
                if let Some((id, rect)) = self.wm.hit(area, x, y) {
                    self.forward_mouse(id, rect, ev);
                }
            }
            _ => {}
        }
    }

    fn forward_mouse(&mut self, id: WinId, rect: Rect, ev: MouseEvent) {
        let inner = Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        };
        if wm::contains(inner, ev.column, ev.row) {
            self.with_window(id, |w, cx| w.on_mouse(ev, inner, cx));
        }
    }

    // ------------------------------------------------------------------ drawing

    fn clock(&self) -> String {
        if self.deterministic {
            return "12:00".into();
        }
        let now = time::OffsetDateTime::now_utc().to_offset(self.local_offset);
        format!("{:02}:{:02}", now.hour(), now.minute())
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let screen = frame.area();
        self.screen = screen;
        let now = Instant::now();
        let area = self.desktop_area();
        let desktop_layout = self.wm.layout == LayoutKind::Desktop;
        chrome::desktop(
            frame.buffer_mut(),
            area,
            &self.theme,
            desktop_layout && self.config.nostalgia.desktop_pattern(),
        );

        // Tab bar.
        self.tab_hits.clear();
        if self.wm.layout == LayoutKind::Tabs {
            let focused = self.wm.focused();
            let tabs: Vec<(String, bool)> = self.wm.ordered().map(|m| (m.win.title(), Some(m.id) == focused)).collect();
            let ids: Vec<WinId> = self.wm.ordered().map(|m| m.id).collect();
            let bar = Rect { height: wm::TAB_BAR_HEIGHT, ..area };
            let hits = chrome::tab_bar(frame.buffer_mut(), bar, &tabs, &self.theme);
            self.tab_hits = hits.into_iter().zip(ids).collect();
        }

        // Windows.
        let placed = self.wm.arrange(area);
        let focused = self.wm.focused();
        let shadows = desktop_layout && self.config.nostalgia.shadows();
        let show_numbers = self.wm.len() > 1;
        for (id, rect) in placed {
            if rect.width < 4 || rect.height < 3 {
                continue;
            }
            let number = self.wm.number(id).filter(|_| show_numbers);
            let App { wm, theme, config, keymap, tasks, .. } = self;
            let Some(m) = wm.get_mut(id) else { continue };
            let is_focused = Some(id) == focused;
            if shadows && m.restore.is_none() {
                chrome::shadow(frame.buffer_mut(), rect, area, theme);
            }
            let title = m.win.title();
            let spec = FrameSpec {
                title: &title,
                number,
                focused: is_focused,
                zoomed: m.restore.is_some(),
                boxes: desktop_layout,
                border: config.general.border,
            };
            let (inner, _) = chrome::window_frame(frame, rect, &spec, theme);
            let dcx = DrawCx { theme, config, keymap, tasks, focused: is_focused, now };
            if inner.width > 0 && inner.height > 0 {
                m.win.draw(frame, inner, &dcx);
            }
        }
        if self.wm.is_empty() {
            let msg = format!(
                "No windows open. Press {} for the palette or {} for the menu.",
                self.keymap.key_for(Action::Palette).unwrap_or_default(),
                self.keymap.key_for(Action::Menu).unwrap_or_default()
            );
            let r = chrome::centered(area, msg.len() as u16 + 4, 3);
            frame.buffer_mut().set_string(
                r.x + 2,
                r.y + 1,
                msg,
                ratatui::style::Style::new().fg(self.theme.desktop_fg).bg(self.theme.desktop_bg),
            );
        }

        // Menu bar and status line.
        let bar = Rect { height: MENU_ROW.min(screen.height), ..screen };
        menu::draw_bar(frame.buffer_mut(), bar, &self.menus, &self.menu, &self.theme);
        let status =
            Rect { y: screen.bottom().saturating_sub(STATUS_ROW), height: STATUS_ROW.min(screen.height), ..screen };
        let (hints, right) = self.status_text();
        chrome::status_line(frame.buffer_mut(), status, &hints, &right, &self.theme);

        // Overlays.
        menu::draw_dropdown(frame, &self.menus, &self.menu, bar, &self.theme, self.config.nostalgia.shadows());
        let toast_area = Rect { y: area.y, height: area.height, ..area };
        chrome::toasts(frame, toast_area, &self.notifications, &self.theme, now);
        if let Some(p) = &mut self.palette {
            p.draw(frame, screen, &self.theme, self.config.nostalgia.shadows());
        }
        if let Some(d) = &self.dialog {
            d.draw(frame, screen, &self.theme, self.config.nostalgia.shadows());
        }
        if let Some(s) = &self.splash {
            s.draw(frame, screen, &self.theme, now);
        }
    }

    fn status_text(&self) -> (Vec<(String, String)>, String) {
        let km = &self.keymap;
        let mut hints: Vec<(String, String)> = vec![];
        if self.move_mode {
            hints.push(("←→↑↓".into(), "Move".into()));
            hints.push(("Shift+←→↑↓".into(), "Resize".into()));
            hints.push(("Enter".into(), "Done".into()));
        } else if !self.pending.is_empty() {
            hints.push((crate::keymap::seq_to_string(&self.pending), "…".into()));
        } else {
            for (a, label) in [(Action::Help, "Help"), (Action::Menu, "Menu"), (Action::Palette, "Palette")] {
                if let Some(k) = km.key_for(a) {
                    hints.push((k, label.into()));
                }
            }
            if let Some(m) = self.wm.focused().and_then(|id| self.wm.get(id)) {
                for (k, l) in m.win.hints() {
                    hints.push((k.into(), l.into()));
                }
            }
            for (a, label) in [(Action::NextWindow, "Next"), (Action::CycleLayout, "Layout"), (Action::Quit, "Exit")] {
                if let Some(k) = km.key_for(a) {
                    hints.push((k, label.into()));
                }
            }
        }
        let running = self.tasks.running();
        let mut right = String::new();
        if running > 0 {
            const SPIN: [&str; 4] = ["◐", "◓", "◑", "◒"];
            let i = if self.deterministic {
                0
            } else {
                (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 250_000_000) as usize % 4
            };
            right.push_str(&format!("{} {running} task{}  │ ", SPIN[i], if running == 1 { "" } else { "s" }));
        }
        if let Some(b) = self.wm.focused().and_then(|id| self.wm.get(id)).and_then(|m| m.win.badge()) {
            right.push_str(&format!("{b}  │ "));
        }
        right.push_str(self.wm.layout.name());
        if self.config.general.clock {
            right.push_str("  │ ");
            right.push_str(&self.clock());
        }
        (hints, right)
    }
}

/// Which milestone brings a scheme, for friendlier messages.
fn coming_in(scheme: &str) -> Option<&'static str> {
    Some(match scheme {
        "gopher" | "gemini" | "finger" | "spartan" | "acct" => "M1",
        "ssh" | "telnet" | "mosh" | "sftp" | "ftp" | "ftps" | "s3" | "webdav" | "davs" => "M2",
        "mailto" | "imap" | "imaps" | "jmap" | "pop" => "M3",
        "news" | "nntp" | "snews" | "feed" | "at" => "M4",
        "http" | "https" => "M5",
        "webcal" | "caldav" | "carddav" | "ldap" | "ldaps" => "M6",
        "irc" | "ircs" | "matrix" | "xmpp" => "M7",
        _ => return None,
    })
}
