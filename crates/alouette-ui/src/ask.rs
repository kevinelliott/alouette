//! Questions that background work asks the person: a password, a passphrase, whether to
//! trust a host key. The work awaits [`Asker::ask`]; the window draws the question with
//! [`draw`] and feeds keys to [`on_key`].

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::oneshot;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    pub body: Vec<String>,
    pub prompt: String,
    /// Hide what is typed.
    pub secret: bool,
    /// A yes or no question: the answer is "yes" or None.
    pub yes_no: bool,
}

struct Pending {
    question: Question,
    reply: oneshot::Sender<Option<String>>,
}

/// The slot one question at a time goes through. Cheap to clone.
#[derive(Clone)]
pub struct Asker {
    pending: Arc<Mutex<Option<Pending>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Asker {
    /// `wake` asks the UI to redraw when a question arrives.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { pending: Arc::new(Mutex::new(None)), wake: Arc::new(wake) }
    }

    /// Ask and wait. None means the person cancelled (or the window closed).
    pub async fn ask(&self, question: Question) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending { question, reply: tx });
        (self.wake)();
        rx.await.ok().flatten()
    }

    pub fn question(&self) -> Option<Question> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| p.question.clone())
    }

    pub fn answer(&self, value: Option<String>) {
        if let Some(p) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = p.reply.send(value);
        }
    }
}

/// Handle a key while a question is showing. `typed` holds the answer being typed.
pub fn on_key(asker: &Asker, q: &Question, typed: &mut String, key: KeyEvent) {
    if q.yes_no {
        match key.code {
            KeyCode::Char('y' | 'Y') => asker.answer(Some("yes".into())),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => asker.answer(None),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Enter => asker.answer(Some(std::mem::take(typed))),
        KeyCode::Esc => {
            typed.clear();
            asker.answer(None);
        }
        KeyCode::Backspace => {
            typed.pop();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => typed.clear(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => typed.push(c),
        _ => {}
    }
}

/// Draw the question as a dialog centred in `area`.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme, q: &Question, typed: &str) {
    let t = theme;
    let w = area.width.saturating_sub(4).clamp(20, 72).min(area.width);
    let h = (q.body.len() as u16 + 5).min(area.height);
    let r = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", q.title))
        .style(Style::new().fg(t.dialog_fg).bg(t.dialog_bg));
    let inner = block.inner(r);
    frame.render_widget(block, r);
    let mut lines: Vec<Line> = q.body.iter().map(|b| Line::from(b.clone())).collect();
    lines.push(Line::default());
    if q.yes_no {
        lines.push(Line::from(vec![
            Span::raw(format!("{} ", q.prompt)),
            Span::styled("y", t.accent()),
            Span::raw(" yes   "),
            Span::styled("n", t.accent()),
            Span::raw(" no"),
        ]));
    } else {
        let shown = if q.secret { "•".repeat(typed.chars().count()) } else { typed.to_string() };
        let x = inner.x + q.prompt.chars().count() as u16 + 2 + shown.chars().count() as u16;
        lines.push(Line::from(vec![
            Span::raw(format!("{}: ", q.prompt)),
            Span::styled(shown, Style::new().fg(t.input_fg).bg(t.input_bg)),
        ]));
        let y = inner.y + lines.len() as u16 - 1;
        if x < inner.right() && y < inner.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
