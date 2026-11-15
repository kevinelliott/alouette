//! Snapshot tests: render whole screens in each layout and compare with saved text.
//! Run `cargo insta review` after an intended change to the look.

use alouette_core::Config;
use alouette_core::config::{KeymapPreset, LayoutKind};
use alouette_store::Store;
use alouette_store::secrets::MemoryStore;
use alouette_ui::builtin::CoreModule;
use alouette_ui::run::render_to_string;
use alouette_ui::theme::{ColorMode, ThemeSet};
use alouette_ui::{App, AppOptions};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app(layout: LayoutKind, keymap: KeymapPreset) -> App {
    let mut config = Config::default();
    config.general.layout = layout;
    config.general.keymap = keymap;
    let (mut app, _rx) = App::new(AppOptions {
        config,
        config_path: std::env::temp_dir().join("alouette-snapshot-never-written.toml"),
        themes: ThemeSet::builtin(),
        store: Store::in_memory().ok(),
        secrets: Box::new(MemoryStore::default()),
        modules: vec![Box::new(CoreModule), Box::new(mod_demo::DemoModule)],
        color_mode: ColorMode::TrueColor,
        show_splash: false,
        local_offset: time::UtcOffset::UTC,
        deterministic: true,
    });
    app.set_screen(ratatui::layout::Rect::new(0, 0, 100, 30));
    app.open_all(&["about:welcome".into(), "demo:hello".into(), "demo:jobs".into()]);
    app
}

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

#[test]
fn desktop() {
    insta::assert_snapshot!(render_to_string(&mut app(LayoutKind::Desktop, KeymapPreset::Cua), 100, 30));
}

#[test]
fn tiled() {
    insta::assert_snapshot!(render_to_string(&mut app(LayoutKind::Tiled, KeymapPreset::Cua), 100, 30));
}

#[test]
fn tabs() {
    insta::assert_snapshot!(render_to_string(&mut app(LayoutKind::Tabs, KeymapPreset::Cua), 100, 30));
}

#[test]
fn palette_open() {
    let mut a = app(LayoutKind::Desktop, KeymapPreset::Cua);
    key(&mut a, KeyCode::Char('k'), KeyModifiers::CONTROL);
    for c in "about".chars() {
        key(&mut a, KeyCode::Char(c), KeyModifiers::NONE);
    }
    insta::assert_snapshot!(render_to_string(&mut a, 100, 30));
}

#[test]
fn file_menu_open() {
    let mut a = app(LayoutKind::Desktop, KeymapPreset::Cua);
    key(&mut a, KeyCode::F(10), KeyModifiers::NONE);
    insta::assert_snapshot!(render_to_string(&mut a, 100, 30));
}

#[test]
fn layout_cycles_without_losing_windows() {
    let mut a = app(LayoutKind::Desktop, KeymapPreset::Cua);
    let before = a.wm.ordered().count();
    for _ in 0..3 {
        key(&mut a, KeyCode::F(6), KeyModifiers::CONTROL);
    }
    assert_eq!(a.wm.layout, LayoutKind::Desktop);
    assert_eq!(a.wm.ordered().count(), before);
}

#[test]
fn opening_the_same_url_focuses_instead_of_duplicating() {
    let mut a = app(LayoutKind::Tiled, KeymapPreset::Cua);
    let before = a.wm.ordered().count();
    a.open_url("about:welcome");
    assert_eq!(a.wm.ordered().count(), before);
}

#[test]
fn unknown_scheme_says_when_it_arrives() {
    let mut a = app(LayoutKind::Tabs, KeymapPreset::Cua);
    a.open_url("gopher://gopher.floodgap.com/");
    let screen = render_to_string(&mut a, 100, 30);
    assert!(screen.contains("M1"), "{screen}");
}

#[test]
fn small_screen_does_not_panic() {
    for layout in LayoutKind::ALL {
        let mut a = app(layout, KeymapPreset::Vim);
        for (w, h) in [(20, 8), (40, 10), (1, 1), (0, 0), (300, 100)] {
            let _ = render_to_string(&mut a, w, h);
        }
    }
}
