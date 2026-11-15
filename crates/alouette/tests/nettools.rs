//! NetTools and Inquire end to end against local servers.

use std::sync::Arc;
use std::time::Duration;

use alouette_core::Config;
use alouette_core::event::EventReceiver;
use alouette_store::Store;
use alouette_store::secrets::MemoryStore;
use alouette_ui::builtin::CoreModule;
use alouette_ui::run::render_to_string;
use alouette_ui::theme::{ColorMode, ThemeSet};
use alouette_ui::{App, AppOptions};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;

fn app() -> (App, EventReceiver) {
    let mut config = Config::default();
    config.general.layout = alouette_core::config::LayoutKind::Tabs;
    let (mut app, rx) = App::new(AppOptions {
        config,
        config_path: std::env::temp_dir().join("alouette-nettools-test-never-written.toml"),
        themes: ThemeSet::builtin(),
        store: Store::in_memory().ok(),
        secrets: Box::new(MemoryStore::default()),
        modules: vec![
            Box::new(CoreModule),
            Box::new(mod_nettools::NetToolsModule),
            Box::new(mod_inquire::InquireModule),
        ],
        color_mode: ColorMode::TrueColor,
        show_splash: false,
        local_offset: time::UtcOffset::UTC,
        deterministic: true,
    });
    app.set_screen(ratatui::layout::Rect::new(0, 0, 100, 30));
    (app, rx)
}

/// Feed events until the screen shows `want`.
async fn until(app: &mut App, rx: &mut EventReceiver, want: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let s = render_to_string(app, 100, 30);
        if s.contains(want) {
            return s;
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) => app.handle_event(ev),
            _ => panic!("never saw {want:?}:\n{s}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn port_check_finds_an_open_port() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { while l.accept().await.is_ok() {} });
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (mut app, mut rx) = app();
    app.open_url(&format!("net:ports/127.0.0.1/{port},{closed}"));
    let s = until(&mut app, &mut rx, "1 open, 1 closed").await;
    assert!(s.contains(&format!("{port}/tcp  open")), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_inspector_shows_pq_and_untrusted_chain() {
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
            let a = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut t) = a.accept(s).await {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut t).await;
                }
            });
        }
    });
    let (mut app, mut rx) = app();
    app.open_url(&format!("net:tls/127.0.0.1:{port}"));
    let s = until(&mut app, &mut rx, "Server certificate").await;
    assert!(s.contains("Not trusted"), "{s}");
    assert!(s.contains("X25519MLKEM768 (post-quantum hybrid)"), "{s}");
    assert!(s.contains("Names          localhost"), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_window_explains_commands() {
    let (mut app, mut rx) = app();
    app.open_url("net:");
    let s = until(&mut app, &mut rx, "Commands").await;
    assert!(s.contains("ping HOST") && s.contains("mdns"), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn inquire_opens_with_slots_for_each_source() {
    let (mut app, _rx) = app();
    app.open_url("inquire:?q=AS64496");
    let s = render_to_string(&mut app, 100, 30);
    assert!(s.contains("Inquire ▸ AS64496"), "{s}");
    assert!(s.contains("asking Team Cymru") && s.contains("asking RDAP"), "{s}");
}
