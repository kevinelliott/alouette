//! A ratatui backend that draws into an SSH channel. It is crossterm's backend writing into a
//! buffer, with the screen size taken from the client's window-change messages instead of
//! the server's own terminal (which a server usually does not have).

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tokio::sync::mpsc::UnboundedSender;

/// Bytes written here go to the channel on flush.
pub struct ChannelWriter {
    buf: Vec<u8>,
    tx: UnboundedSender<Vec<u8>>,
}

impl ChannelWriter {
    pub fn new(tx: UnboundedSender<Vec<u8>>) -> Self {
        Self { buf: Vec::with_capacity(16 * 1024), tx }
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let out = std::mem::take(&mut self.buf);
            // Once the person has gone there is nobody to draw for; the session ends on its own.
            let _ = self.tx.send(out);
        }
        Ok(())
    }
}

/// The size the client last reported.
pub type SharedSize = Arc<Mutex<(u16, u16)>>;

pub struct SshBackend {
    inner: CrosstermBackend<ChannelWriter>,
    size: SharedSize,
}

impl SshBackend {
    pub fn new(w: ChannelWriter, size: SharedSize) -> Self {
        Self { inner: CrosstermBackend::new(w), size }
    }

    fn cols_rows(&self) -> (u16, u16) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Backend for SshBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // Asking the client would mean reading its answer from the input; the fullscreen
        // viewport never needs it.
        Ok(Position::ORIGIN)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        let (w, h) = self.cols_rows();
        Ok(Size::new(w, h))
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        let (w, h) = self.cols_rows();
        Ok(WindowSize { columns_rows: Size::new(w, h), pixels: Size::new(0, 0) })
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}
