//! Alouette: a modern, single-binary internet suite for the terminal.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use alouette_core::config::{KeymapPreset, LayoutKind};
use alouette_core::{Config, ConfigWatcher, Paths};
use alouette_store::Store;
use alouette_ui::builtin::CoreModule;
use alouette_ui::theme::{ColorMode, ThemeSet};
use alouette_ui::{App, AppOptions, Keymap, Module};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};

#[derive(Parser, Debug)]
#[command(name = "alouette", version, about = "A modern internet suite for the terminal, in the spirit of Minuet")]
struct Cli {
    /// URLs to open at startup, such as about:tasks, gopher://… or gemini://…
    urls: Vec<String>,

    /// Window layout: desktop, tiled or tabs.
    #[arg(long, value_parser = parse_layout)]
    layout: Option<LayoutKind>,

    /// Theme name, such as minuet, turbo, amber, green, modern-dark or mono.
    #[arg(long)]
    theme: Option<String>,

    /// Key bindings: cua, vim or emacs.
    #[arg(long, value_parser = parse_keymap)]
    keymap: Option<KeymapPreset>,

    /// Skip the splash screen.
    #[arg(long)]
    no_splash: bool,

    /// Use this config file instead of the default.
    #[arg(long, env = "ALOUETTE_CONFIG")]
    config: Option<PathBuf>,

    /// Render one frame of the given size (such as 100x30) to stdout and exit.
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    dump_screen: Option<(u16, u16)>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Work with the config file.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// List the available themes.
    Themes,
    /// Print the key bindings for a keymap.
    Keys {
        /// cua, vim or emacs (default: the configured one).
        #[arg(value_parser = parse_keymap)]
        keymap: Option<KeymapPreset>,
    },
    /// Show where Alouette keeps its files.
    Paths,
    /// Connect to a server and show how: address family, proxy, TLS version, key exchange.
    Probe {
        /// host:port, such as example.org:443 or [2001:db8::1]:1965
        target: String,
        /// Plain TCP only, no TLS.
        #[arg(long)]
        plain: bool,
        /// Accept any certificate and print its fingerprint (as Gemini does).
        #[arg(long)]
        tofu: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Write the default config file if there is none.
    Init,
    /// Check the config file for errors.
    Check,
    /// Print the default config.
    Default,
}

fn parse_layout(s: &str) -> Result<LayoutKind, String> {
    s.parse()
}
fn parse_keymap(s: &str) -> Result<KeymapPreset, String> {
    s.parse()
}
fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WxH, such as 100x30")?;
    let w: u16 = w.trim().parse().map_err(|_| "bad width")?;
    let h: u16 = h.trim().parse().map_err(|_| "bad height")?;
    if w < 20 || h < 8 {
        return Err("the screen must be at least 20x8".into());
    }
    Ok((w, h))
}

fn modules() -> Vec<Box<dyn Module>> {
    vec![Box::new(CoreModule), Box::new(mod_demo::DemoModule)]
}

fn main() -> std::process::ExitCode {
    match real_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("alouette: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover();
    let config_path = cli.config.clone().unwrap_or_else(|| paths.config_file());

    if let Some(cmd) = &cli.command {
        if let Command::Probe { target, plain, tofu } = cmd {
            let cfg = Config::load(&config_path).unwrap_or_default();
            return probe(&cfg, target, *plain, *tofu);
        }
        return subcommand(cmd, &paths, &config_path);
    }

    // The local UTC offset must be read before any threads start (a `time` crate rule).
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);

    paths.ensure().context("creating Alouette's folders")?;
    let _log_guard = init_logging(&paths);
    let wrote_default = Config::write_default_if_missing(&config_path).unwrap_or(false);
    let (mut config, config_err) = match Config::load(&config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    if let Some(l) = cli.layout {
        config.general.layout = l;
    }
    if let Some(t) = &cli.theme {
        config.general.theme = t.clone();
    }
    if let Some(k) = cli.keymap {
        config.general.keymap = k;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let store = match Store::open(&paths.state_db()) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("state database unavailable: {e}");
                None
            }
        };
        let dumping = cli.dump_screen.is_some();
        let opts = AppOptions {
            config,
            config_path: config_path.clone(),
            themes: ThemeSet::load(&paths.themes_dir()),
            store: if dumping { Store::in_memory().ok() } else { store },
            secrets: alouette_store::secrets::default_store(),
            modules: modules(),
            color_mode: if dumping { ColorMode::TrueColor } else { ColorMode::detect() },
            show_splash: !cli.no_splash && !dumping,
            local_offset,
            deterministic: dumping,
        };
        let (mut app, events) = App::new(opts);
        if let Some(e) = config_err {
            app.notify(alouette_core::Notification::error(format!("config: {e}. Using defaults.")));
        }
        if wrote_default {
            app.notify(alouette_core::Notification::info(format!(
                "Wrote a default config to {}",
                config_path.display()
            )));
        }
        // Size the desktop before opening windows so they are placed for the real screen.
        let (w, h) = cli.dump_screen.or_else(|| terminal::size().ok()).unwrap_or((80, 24));
        app.set_screen(ratatui::layout::Rect::new(0, 0, w, h));
        let urls = if cli.urls.is_empty() { app.config.general.startup.clone() } else { cli.urls.clone() };
        app.open_all(&urls);

        if let Some((w, h)) = cli.dump_screen {
            print!("{}", alouette_ui::run::render_to_string(&mut app, w, h));
            return Ok(());
        }
        if !std::io::stdout().is_terminal() {
            anyhow::bail!("Alouette needs a terminal. Try --dump-screen 100x30 to render one frame.");
        }

        let _watcher = ConfigWatcher::start(config_path.clone(), app.sender())
            .map_err(|e| tracing::warn!("config watching is off: {e}"))
            .ok();
        let mut term = setup_terminal(app.config.general.mouse)?;
        let result = alouette_ui::run::run(&mut term, &mut app, events).await;
        restore_terminal();
        result
    })
}

fn subcommand(cmd: &Command, paths: &Paths, config_path: &std::path::Path) -> Result<()> {
    match cmd {
        Command::Config { action } => match action {
            ConfigCmd::Path => println!("{}", config_path.display()),
            ConfigCmd::Init => {
                if Config::write_default_if_missing(config_path)? {
                    println!("wrote {}", config_path.display());
                } else {
                    println!("{} already exists", config_path.display());
                }
            }
            ConfigCmd::Check => match Config::load(config_path) {
                Ok(_) => println!("{}: ok", config_path.display()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            },
            ConfigCmd::Default => print!("{}", alouette_core::config::DEFAULT_CONFIG),
        },
        Command::Themes => {
            let set = ThemeSet::load(&paths.themes_dir());
            for n in set.names() {
                println!("{n}");
            }
            for e in &set.errors {
                eprintln!("warning: {e}");
            }
        }
        Command::Keys { keymap } => {
            let cfg = Config::load(config_path).unwrap_or_default();
            let km = Keymap::new(keymap.unwrap_or(cfg.general.keymap), &cfg.keys);
            let mut out = std::io::stdout().lock();
            for (action, keys) in km.all() {
                writeln!(
                    out,
                    "{:<28} {}",
                    action.label(),
                    if keys.is_empty() { "(unbound)".into() } else { keys.join(", ") }
                )?;
            }
        }
        Command::Probe { .. } => unreachable!("handled before"),
        Command::Paths => {
            println!("config   {}", config_path.display());
            println!("themes   {}", paths.themes_dir().display());
            println!("data     {}", paths.data_dir.display());
            println!("cache    {}", paths.cache_dir.display());
            println!("logs     {}", paths.log_dir.display());
        }
    }
    Ok(())
}

fn probe(cfg: &Config, target: &str, plain: bool, tofu: bool) -> Result<()> {
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() && (!h.contains(':') || h.ends_with(']')) => {
            (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse::<u16>()?)
        }
        _ => (target.trim_start_matches('[').trim_end_matches(']').to_string(), 443),
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let net = alouette_net::Net::new(&cfg.network)?;
        println!("dns      {}", net.dns().label());
        if let Ok(addrs) = net.resolve(&host, port).await {
            for a in addrs {
                println!("address  {}", a.ip());
            }
        }
        let info = if plain {
            net.tcp(&host, port).await?.info
        } else {
            let verify = if tofu { alouette_net::Verify::Tofu } else { alouette_net::Verify::System };
            let opts = alouette_net::TlsOptions { verify, ..Default::default() };
            net.tls(&host, port, &opts).await?.info
        };
        println!("via      {}", info.via);
        if let Some(p) = info.peer {
            println!("peer     {p}");
        }
        if let Some(t) = &info.tls {
            println!("tls      {} · {} · {}", t.version, t.cipher, t.key_exchange);
            println!("pq       {}", if t.post_quantum { "yes (hybrid ML-KEM)" } else { "no" });
            if let Some(a) = &t.alpn {
                println!("alpn     {a}");
            }
            if let Some(f) = &t.cert_sha256 {
                println!("cert     sha256:{f}{}", if t.verified { " (verified)" } else { " (not verified)" });
            }
        }
        println!("badge    {}", info.badge());
        Ok(())
    })
}

fn init_logging(paths: &Paths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::EnvFilter;
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "alouette.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("ALOUETTE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(writer).with_ansi(false).try_init().ok()?;
    Some(guard)
}

type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

fn setup_terminal(mouse: bool) -> Result<Term> {
    // Put the terminal back even if we panic, so the shell is usable afterwards.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        hook(info);
    }));
    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, terminal::EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;
    term.clear()?;
    Ok(term)
}

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}
