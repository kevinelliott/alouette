//! The main loop: draw when something changed, then wait for input, an app event or a tick.

use std::time::Duration;

use alouette_core::event::EventReceiver;
use crossterm::event::EventStream;
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::App;

/// How often the clock, spinners and toasts are refreshed.
pub const TICK: Duration = Duration::from_millis(250);

/// Run until the app asks to quit. The caller owns terminal setup and teardown.
pub async fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, events: EventReceiver) -> anyhow::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    run_with(terminal, app, events, EventStream::new()).await
}

/// The same loop with input from any stream of terminal events, such as an SSH channel's.
pub async fn run_with<B, S>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut events: EventReceiver,
    mut input: S,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    S: futures::Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
{
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let size = terminal.size()?;
    app.set_screen(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    app.start_modules();

    while !app.quit {
        if app.dirty {
            app.dirty = false;
            terminal.draw(|f| app.draw(f))?;
        }
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(ev)) => app.handle_input(ev),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            ev = events.recv() => match ev {
                Some(ev) => {
                    app.handle_event(ev);
                    // Drain whatever else is queued so a burst of task updates costs one redraw.
                    while let Ok(ev) = events.try_recv() {
                        app.handle_event(ev);
                    }
                }
                None => break,
            },
            _ = tick.tick() => app.on_tick(),
        }
    }
    Ok(())
}

/// Render one frame into a string, for `--dump-screen` and tests.
pub fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    app.set_screen(ratatui::layout::Rect::new(0, 0, width, height));
    term.draw(|f| app.draw(f)).expect("draw");
    let buf = term.backend().buffer();
    let mut out = String::with_capacity((width as usize + 1) * height as usize);
    for y in 0..height {
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            skip = unicode_width::UnicodeWidthStr::width(sym).saturating_sub(1);
            out.push_str(sym);
        }
        let trimmed = out.trim_end_matches(' ').len();
        out.truncate(trimmed);
        out.push('\n');
    }
    out
}
