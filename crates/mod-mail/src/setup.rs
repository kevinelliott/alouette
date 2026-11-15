//! A form to add a mail account. It writes `[[mail.accounts]]` to the config; passwords
//! are asked for on first connect and kept in the secret store.

use alouette_core::config::{MailAccount, MailKind};
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::MODULE;

const KINDS: [(MailKind, &str); 4] =
    [(MailKind::Imap, "IMAP"), (MailKind::Gmail, "Gmail"), (MailKind::M365, "Microsoft 365"), (MailKind::Jmap, "JMAP")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum F {
    Kind,
    Name,
    Email,
    Display,
    User,
    Imap,
    Smtp,
    Jmap,
    ClientId,
    ClientSecret,
}

impl F {
    fn label(self) -> &'static str {
        match self {
            F::Kind => "Kind",
            F::Name => "Name",
            F::Email => "Address",
            F::Display => "Your name",
            F::User => "Login",
            F::Imap => "IMAP server",
            F::Smtp => "SMTP server",
            F::Jmap => "JMAP URL",
            F::ClientId => "Client ID",
            F::ClientSecret => "Client secret",
        }
    }
    fn help(self) -> &'static str {
        match self {
            F::Kind => "Left and Right choose. Microsoft 365 also covers Outlook.com; JMAP suits Fastmail.",
            F::Name => "A short name for the account, such as work. Opens as mail:<name>.",
            F::Email => "Your email address. Messages are sent from it.",
            F::Display => "Optional. Shown to the people you write to.",
            F::User => "Optional. The login, if it is not your address.",
            F::Imap => "host[:port]. Port 993 uses TLS from the start; 143 uses STARTTLS.",
            F::Smtp => "host[:port]. Port 465 uses TLS from the start; 587 uses STARTTLS.",
            F::Jmap => "The session URL, or just the host (/.well-known/jmap is tried).",
            F::ClientId => "Your own OAuth app's client ID. Google and Microsoft need one; see Help.",
            F::ClientSecret => "Google only, if your app has one.",
        }
    }
}

pub struct SetupWindow {
    kind: usize,
    inputs: Vec<(F, LineInput)>,
    focus: usize,
    note: Option<String>,
}

impl Default for SetupWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupWindow {
    pub fn new() -> Self {
        let all = [F::Name, F::Email, F::Display, F::User, F::Imap, F::Smtp, F::Jmap, F::ClientId, F::ClientSecret];
        Self { kind: 0, inputs: all.into_iter().map(|f| (f, LineInput::default())).collect(), focus: 0, note: None }
    }

    fn kind(&self) -> MailKind {
        KINDS[self.kind].0
    }

    /// The rows shown for the chosen kind, Kind first.
    fn rows(&self) -> Vec<F> {
        let k = self.kind();
        let mut v = vec![F::Kind, F::Name, F::Email, F::Display];
        match k {
            MailKind::Imap => v.extend([F::User, F::Imap, F::Smtp]),
            MailKind::Jmap => v.extend([F::User, F::Jmap]),
            MailKind::Gmail => v.extend([F::ClientId, F::ClientSecret]),
            MailKind::M365 => v.extend([F::ClientId]),
        }
        v
    }

    fn value(&self, f: F) -> Option<String> {
        self.inputs.iter().find(|(g, _)| *g == f).map(|(_, i)| i.value.trim().to_string()).filter(|v| !v.is_empty())
    }

    fn input(&mut self, f: F) -> Option<&mut LineInput> {
        self.inputs.iter_mut().find(|(g, _)| *g == f).map(|(_, i)| i)
    }

    /// The account the form describes, or what is missing.
    pub fn account(&self, existing: &[MailAccount]) -> Result<MailAccount, String> {
        let email = self.value(F::Email).ok_or("Type your address")?;
        if !email.contains('@') {
            return Err("The address needs an @".into());
        }
        let name = self.value(F::Name).unwrap_or_else(|| email.split('@').next().unwrap_or("mail").to_string());
        if name.contains(['/', ' ']) {
            return Err("Keep the name to one word".into());
        }
        if existing.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) {
            return Err(format!("There is already an account called {name}"));
        }
        let kind = self.kind();
        let keep = |f: F| if self.rows().contains(&f) { self.value(f) } else { None };
        let a = MailAccount {
            name,
            email,
            display_name: keep(F::Display),
            kind,
            user: keep(F::User),
            imap: keep(F::Imap),
            smtp: keep(F::Smtp),
            jmap: keep(F::Jmap),
            client_id: keep(F::ClientId),
            client_secret: keep(F::ClientSecret),
            signature: None,
        };
        match kind {
            MailKind::Imap if a.imap.is_none() || a.smtp.is_none() => Err("Fill in the IMAP and SMTP servers".into()),
            MailKind::Jmap if a.jmap.is_none() => Err("Fill in the JMAP URL".into()),
            MailKind::Gmail | MailKind::M365 if a.client_id.is_none() => {
                Err("Fill in your OAuth client ID (see Help: Mail)".into())
            }
            _ => Ok(a),
        }
    }

    fn save(&mut self, cx: &mut Cx) {
        match self.account(&cx.config.mail.accounts) {
            Ok(a) => {
                let name = a.name.clone();
                cx.edit_config(move |c| c.mail.accounts.push(a));
                cx.open_url(format!("mail:{name}"));
                cx.close();
            }
            Err(e) => self.note = Some(e),
        }
    }
}

impl Window for SetupWindow {
    fn title(&self) -> String {
        "Add a mail account".into()
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some("mail:setup".into())
    }
    fn wants_text(&self) -> bool {
        self.rows().get(self.focus).is_some_and(|f| *f != F::Kind)
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Tab", "Next"), ("Ctrl+S", "Save"), ("Esc", "Close")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let rows = self.rows();
        self.focus = self.focus.min(rows.len() - 1);
        let lw = 15u16;
        let mut lines = vec![
            Line::from(Span::styled(" Add a mail account", t.accent())),
            Line::from(Span::styled(
                " Passwords are asked for when you first connect, and kept in your keychain.",
                t.dim(),
            )),
            Line::default(),
        ];
        let top = area.y + lines.len() as u16;
        let mut cursor = None;
        for (i, f) in rows.iter().enumerate() {
            let focused = i == self.focus;
            let label = Span::styled(
                format!(" {:>w$}: ", f.label(), w = lw as usize - 3),
                if focused { t.accent() } else { t.dim() },
            );
            let field = Style::new().fg(t.input_fg).bg(t.input_bg);
            let value = if *f == F::Kind {
                let s = KINDS
                    .iter()
                    .enumerate()
                    .map(|(k, (_, n))| if k == self.kind { format!("[{n}]") } else { format!(" {n} ") })
                    .collect::<Vec<_>>()
                    .join(" ");
                Span::styled(s, if focused { field } else { Style::new() })
            } else {
                let inp = self.inputs.iter().find(|(g, _)| g == f).map(|(_, i)| i).cloned().unwrap_or_default();
                if focused {
                    cursor = Some((area.x + lw + inp.cursor_col() as u16, top + i as u16));
                }
                let shown =
                    if *f == F::ClientSecret { "•".repeat(inp.value.chars().count()) } else { inp.value.clone() };
                let w = area.width.saturating_sub(lw + 1) as usize;
                let pad = w.saturating_sub(unicode_width::UnicodeWidthStr::width(shown.as_str()));
                Span::styled(format!("{shown}{}", " ".repeat(pad)), if focused { field } else { Style::new() })
            };
            lines.push(Line::from(vec![label, value]));
        }
        lines.push(Line::default());
        if let Some(f) = rows.get(self.focus) {
            lines.push(Line::from(Span::styled(format!(" {}", f.help()), t.dim())));
        }
        if let Some(n) = &self.note {
            lines.push(Line::from(Span::styled(format!(" {n}"), Style::new().fg(t.error))));
        }
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("Ctrl+S", t.accent()),
            Span::raw(" or Enter on the last field saves and connects."),
        ]));
        frame.render_widget(Paragraph::new(lines), area);
        if let Some(c) = cursor
            && cx.focused
        {
            frame.set_cursor_position(c);
        }
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let rows = self.rows();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let f = rows[self.focus.min(rows.len() - 1)];
        match key.code {
            KeyCode::Char('s') if ctrl => self.save(cx),
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1).min(rows.len() - 1),
            KeyCode::BackTab | KeyCode::Up => self.focus = self.focus.saturating_sub(1),
            KeyCode::Enter if self.focus + 1 == rows.len() => self.save(cx),
            KeyCode::Enter => self.focus += 1,
            KeyCode::Esc => cx.close(),
            KeyCode::Left if f == F::Kind => self.kind = (self.kind + KINDS.len() - 1) % KINDS.len(),
            KeyCode::Right | KeyCode::Char(' ') if f == F::Kind => self.kind = (self.kind + 1) % KINDS.len(),
            _ if f == F::Kind => return Handled::No,
            _ => {
                let Some(inp) = self.input(f) else { return Handled::No };
                match inp.on_key(key) {
                    InputResult::Ignored => return Handled::No,
                    _ => self.note = None,
                }
                // Fill in the name from the address as it is typed.
                if f == F::Email
                    && let Some(e) = self.value(F::Email)
                    && let Some(n) = self.input(F::Name)
                    && (n.value.is_empty() || e.starts_with(&n.value))
                {
                    let local = e.split('@').next().unwrap_or("").to_string();
                    n.set(&local);
                }
            }
        }
        Handled::Yes
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        let rows = self.rows();
        let f = rows[self.focus.min(rows.len() - 1)];
        if let Some(i) = self.input(f) {
            let v = format!("{}{}", i.value, text.trim());
            i.set(&v);
            return Handled::Yes;
        }
        Handled::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(w: &mut SetupWindow, f: F, v: &str) {
        w.input(f).unwrap().set(v);
    }

    #[test]
    fn builds_accounts() {
        let mut w = SetupWindow::new();
        assert!(w.account(&[]).is_err());
        set(&mut w, F::Email, "me@example.com");
        assert_eq!(w.account(&[]).unwrap_err(), "Fill in the IMAP and SMTP servers");
        set(&mut w, F::Imap, "imap.example.com");
        set(&mut w, F::Smtp, "smtp.example.com:587");
        let a = w.account(&[]).unwrap();
        assert_eq!(a.name, "me");
        assert_eq!(a.kind, MailKind::Imap);
        assert_eq!(a.smtp.as_deref(), Some("smtp.example.com:587"));
        assert!(w.account(std::slice::from_ref(&a)).unwrap_err().contains("already"));
        // Gmail drops the server fields and wants a client ID.
        w.kind = 1;
        assert!(w.account(&[]).unwrap_err().contains("client ID"));
        set(&mut w, F::ClientId, "abc.apps.googleusercontent.com");
        let a = w.account(&[]).unwrap();
        assert_eq!(a.kind, MailKind::Gmail);
        assert_eq!(a.imap, None);
    }
}
