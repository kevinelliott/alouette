//! Burrow end to end: real servers on localhost, the real app, rendered screens.

use std::sync::Arc;
use std::time::Duration;

use alouette_core::event::EventReceiver;
use alouette_core::{AppEvent, Config};
use alouette_store::Store;
use alouette_store::secrets::MemoryStore;
use alouette_ui::builtin::CoreModule;
use alouette_ui::run::render_to_string;
use alouette_ui::theme::{ColorMode, ThemeSet};
use alouette_ui::{App, AppOptions};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn app(downloads: std::path::PathBuf) -> (App, EventReceiver) {
    let mut config = Config::default();
    config.general.layout = alouette_core::config::LayoutKind::Tabs;
    let (mut app, rx) = App::new(AppOptions {
        config,
        config_path: std::env::temp_dir().join("alouette-burrow-test-never-written.toml"),
        themes: ThemeSet::builtin(),
        store: Store::in_memory().ok(),
        secrets: Box::new(MemoryStore::default()),
        modules: vec![Box::new(CoreModule), Box::new(mod_burrow::BurrowModule::new(downloads))],
        color_mode: ColorMode::TrueColor,
        show_splash: false,
        local_offset: time::UtcOffset::UTC,
        deterministic: true,
        max_windows: None,
        window_count: None,
    });
    app.set_screen(ratatui::layout::Rect::new(0, 0, 90, 24));
    (app, rx)
}

/// Feed events to the app until a Burrow page arrives.
async fn settle(app: &mut App, rx: &mut EventReceiver) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv()).await.expect("timed out").expect("channel");
        let done = matches!(&ev, AppEvent::Module { module: "burrow", .. });
        app.handle_event(ev);
        if done {
            return;
        }
    }
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// A Gopher server that answers by selector.
async fn gopher_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 512];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = match req.trim_end() {
                    "" => format!(
                        "iWelcome to the test hole\t\t\t0\r\n0About this server\t/about\t127.0.0.1\t{port}\r\n7Search\t/search\t127.0.0.1\t{port}\r\n.\r\n"
                    ),
                    "/about" => "Alouette was here.\r\n..And dots survive.\r\n.\r\n".to_string(),
                    r if r.starts_with("/search\t") => {
                        format!("iResults for {}\t\t\t0\r\n.\r\n", r.split('\t').nth(1).unwrap_or(""))
                    }
                    _ => "3Not found\t\terror.host\t1\r\n.\r\n".to_string(),
                };
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    port
}

async fn gemini_server() -> u16 {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let cfg = rustls::ServerConfig::builder_with_provider(alouette_net::tls::provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![ck.cert.der().clone()], key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(s).await else { return };
                let mut buf = vec![0u8; 1100];
                let n = tls.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).trim_end().to_string();
                let path = req.split_once("://").and_then(|(_, r)| r.split_once('/')).map(|(_, p)| p).unwrap_or("");
                let reply = match path {
                    "" => {
                        "20 text/gemini\r\n# Test capsule\n\nHello from Gemini.\n=> /next Next page\n=> /ask Ask me\n"
                            .into()
                    }
                    "next" => "20 text/gemini\r\n# Next\n> A quote\n* An item\n".into(),
                    "ask" => "10 Your name\r\n".into(),
                    p if p.starts_with("ask?") => format!("20 text/gemini\r\n# Hi {}\n", &p[4..]),
                    "old" => "31 /next\r\n".into(),
                    _ => "51 Not here\r\n".to_string(),
                };
                let _ = tls.write_all(reply.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn gopher_menu_text_and_search() {
    let port = gopher_server().await;
    let dir = std::env::temp_dir().join(format!("alouette-dl-{port}"));
    let (mut app, mut rx) = app(dir);
    app.open_url(&format!("gopher://127.0.0.1:{port}/"));
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("Welcome to the test hole"), "{s}");
    assert!(s.contains("TXT  About this server"), "{s}");
    assert!(s.contains("IPv4 · plain"), "{s}");

    // The first link is selected; Enter opens the text item.
    key(&mut app, KeyCode::Enter);
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("Alouette was here.") && s.contains(".And dots survive."), "{s}");

    // Back, down to the search item, and search.
    key(&mut app, KeyCode::Char('b'));
    settle(&mut app, &mut rx).await;
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    settle(&mut app, &mut rx).await;
    for c in "minuet".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("Results for minuet"), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn gemini_pages_input_redirects_and_tofu() {
    let port = gemini_server().await;
    let dir = std::env::temp_dir().join(format!("alouette-dl-{port}"));
    let (mut app, mut rx) = app(dir);
    app.open_url(&format!("gemini://127.0.0.1:{port}/"));
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("Test capsule") && s.contains("→ Next page"), "{s}");
    assert!(s.contains("TLS 1.3 · PQ · TOFU"), "{s}");

    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("│ A quote") && s.contains("• An item"), "{s}");

    // Input prompt.
    app.open_url(&format!("gemini://127.0.0.1:{port}/ask"));
    settle(&mut app, &mut rx).await;
    let s = render_to_string(&mut app, 90, 24);
    assert!(s.contains("Your name:"), "{s}");
    for c in "Kev".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    settle(&mut app, &mut rx).await;
    assert!(render_to_string(&mut app, 90, 24).contains("Hi Kev"));

    // Redirects are followed.
    app.open_url(&format!("gemini://127.0.0.1:{port}/old"));
    settle(&mut app, &mut rx).await;
    assert!(render_to_string(&mut app, 90, 24).contains("A quote"));
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_certificate_is_caught() {
    // Two servers on different ports have different certificates; pretend they are one
    // host by pinning the first fingerprint under the second's key.
    let a = gemini_server().await;
    let b = gemini_server().await;
    let dir = std::env::temp_dir().join(format!("alouette-dl-{a}"));
    let (mut app, mut rx) = app(dir);
    app.open_url(&format!("gemini://127.0.0.1:{a}/"));
    settle(&mut app, &mut rx).await;
    // Copy a's pin onto b in the saved pins, then open b in a fresh app with that store.
    let pins = app.store().unwrap().kv_get(mod_burrow::PINS_KEY).unwrap().unwrap();
    let fp_a = pins.lines().next().unwrap().split(' ').nth(1).unwrap().to_string();
    let (mut app2, mut rx2) = self::app(std::env::temp_dir());
    app2.store().unwrap().kv_set(mod_burrow::PINS_KEY, &format!("127.0.0.1:{b} {fp_a}")).unwrap();
    app2.open_url(&format!("gemini://127.0.0.1:{b}/"));
    settle(&mut app2, &mut rx2).await;
    let s = render_to_string(&mut app2, 90, 24);
    assert!(s.contains("Certificate changed"), "{s}");
    key(&mut app2, KeyCode::Char('t'));
    settle(&mut app2, &mut rx2).await;
    assert!(render_to_string(&mut app2, 90, 24).contains("Test capsule"));
}
