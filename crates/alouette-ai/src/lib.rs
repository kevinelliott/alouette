//! The assistant's engine. It knows how to ask a model something through a local CLI
//! (claude, codex, gemini or any command), a local Ollama, an OpenAI-compatible server, the
//! Anthropic API or the Gemini API. It also redacts personal details and keeps a cost meter.
//!
//! Nothing here runs unless the person has turned the assistant on and picked a provider.

pub mod cli;
pub mod detect;
pub mod http;
pub mod prompt;
pub mod redact;
pub mod search;
pub mod usage;

#[cfg(feature = "test-server")]
pub mod testserver;

use alouette_core::config::Ai;
use alouette_net::Net;

/// Where a request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    ClaudeCli,
    CodexCli,
    GeminiCli,
    Command,
    Ollama,
    OpenAi,
    Anthropic,
    Gemini,
}

impl Kind {
    pub const ALL: &'static [Kind] = &[
        Kind::ClaudeCli,
        Kind::CodexCli,
        Kind::GeminiCli,
        Kind::Command,
        Kind::Ollama,
        Kind::OpenAi,
        Kind::Anthropic,
        Kind::Gemini,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Kind::ClaudeCli => "claude-cli",
            Kind::CodexCli => "codex-cli",
            Kind::GeminiCli => "gemini-cli",
            Kind::Command => "command",
            Kind::Ollama => "ollama",
            Kind::OpenAi => "openai",
            Kind::Anthropic => "anthropic",
            Kind::Gemini => "gemini",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.id() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::ClaudeCli => "Claude Code CLI (claude)",
            Kind::CodexCli => "Codex CLI (codex)",
            Kind::GeminiCli => "Gemini CLI (gemini)",
            Kind::Command => "Any command (reads stdin, writes stdout)",
            Kind::Ollama => "Ollama, on this machine",
            Kind::OpenAi => "OpenAI-compatible API",
            Kind::Anthropic => "Anthropic API",
            Kind::Gemini => "Google Gemini API",
        }
    }

    /// True when content leaves this machine.
    pub fn is_remote(self) -> bool {
        !matches!(self, Kind::Ollama | Kind::Command)
    }

    /// True for providers reached over HTTP rather than a program on this machine.
    pub fn is_http(self) -> bool {
        matches!(self, Kind::Ollama | Kind::OpenAi | Kind::Anthropic | Kind::Gemini)
    }

    /// True when the provider needs an API key from the person.
    pub fn needs_key(self) -> bool {
        matches!(self, Kind::OpenAi | Kind::Anthropic | Kind::Gemini)
    }

    /// The environment variable that may already hold a key.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Kind::OpenAi => Some("OPENAI_API_KEY"),
            Kind::Anthropic => Some("ANTHROPIC_API_KEY"),
            Kind::Gemini => Some("GEMINI_API_KEY"),
            _ => None,
        }
    }

    /// Where the key is kept in the secret store.
    pub fn secret_key(self) -> String {
        format!("ai/{}", self.id())
    }

    /// The model used when the config names none. `None` means the person must choose one.
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Kind::Anthropic => Some("claude-opus-5-5"),
            _ => None,
        }
    }

    pub fn default_base(self) -> Option<&'static str> {
        match self {
            Kind::Ollama => Some("http://127.0.0.1:11434"),
            Kind::OpenAi => Some("https://api.openai.com/v1"),
            Kind::Anthropic => Some("https://api.anthropic.com"),
            Kind::Gemini => Some("https://generativelanguage.googleapis.com"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One request: a system prompt and the conversation so far.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub system: String,
    pub messages: Vec<(Role, String)>,
    pub max_tokens: u32,
}

impl Request {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self { system: system.into(), messages: vec![(Role::User, user.into())], max_tokens: 16000 }
    }

    /// Everything in one text, for CLIs that take a single prompt.
    pub fn flat(&self) -> String {
        let mut s = String::new();
        if !self.system.is_empty() {
            s.push_str(&self.system);
            s.push_str("\n\n");
        }
        let many = self.messages.len() > 1;
        for (role, text) in &self.messages {
            if many {
                s.push_str(match role {
                    Role::User => "User: ",
                    Role::Assistant => "Assistant: ",
                });
            }
            s.push_str(text);
            s.push_str("\n\n");
        }
        s.trim_end().to_string()
    }

    /// Characters that will be sent, for the preview.
    pub fn chars(&self) -> usize {
        self.system.chars().count() + self.messages.iter().map(|(_, t)| t.chars().count()).sum::<usize>()
    }

    /// A rough token count (four characters per token), for the cost estimate.
    pub fn estimate_tokens(&self) -> u64 {
        (self.chars() as u64).div_ceil(4)
    }
}

/// What came back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The cost the provider reported, when it reports one (the claude CLI does).
    pub cost: Option<f64>,
}

/// A configured provider, ready to ask.
#[derive(Debug, Clone)]
pub struct Provider {
    pub kind: Kind,
    pub model: String,
    pub base: String,
    pub key: Option<String>,
    /// For `Command`: the program and its arguments.
    pub command: Vec<String>,
}

impl Provider {
    /// Build from the config. `key` is the API key, when the provider needs one.
    pub fn from_config(cfg: &Ai, key: Option<String>) -> Result<Self, String> {
        // On a shared server the environment's keys are the operator's, not this person's.
        Self::build(cfg, key, |v| if alouette_core::shared::restricted() { None } else { std::env::var(v).ok() })
    }

    fn build(cfg: &Ai, key: Option<String>, env: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let id = cfg.provider.as_deref().filter(|s| !s.is_empty()).ok_or("no provider chosen yet")?;
        let kind = Kind::parse(id).ok_or_else(|| format!("unknown AI provider `{id}`"))?;
        let model = cfg
            .model
            .clone()
            .filter(|m| !m.is_empty())
            .or_else(|| kind.default_model().map(str::to_string))
            .unwrap_or_default();
        if model.is_empty() && matches!(kind, Kind::Ollama | Kind::OpenAi | Kind::Gemini) {
            return Err(format!("choose a model for {} (ai.model)", kind.label()));
        }
        let key = key.or_else(|| kind.key_env().and_then(&env)).filter(|k| !k.is_empty());
        if kind.needs_key() && key.is_none() && !is_local_base(cfg.base_url.as_deref()) {
            return Err(format!("{} needs an API key", kind.label()));
        }
        let base = cfg
            .base_url
            .clone()
            .filter(|b| !b.is_empty())
            .or_else(|| kind.default_base().map(str::to_string))
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        let command = match kind {
            Kind::Command => {
                let c = cfg.command.as_deref().unwrap_or("").trim();
                if c.is_empty() {
                    return Err("set ai.command to a program that reads stdin".into());
                }
                split_command(c)
            }
            _ => Vec::new(),
        };
        Ok(Self { kind, model, base, key, command })
    }

    /// Ask. Returns the answer, or why there is none.
    pub async fn ask(&self, net: &Net, req: &Request) -> Result<Reply, String> {
        if alouette_core::shared::restricted() && !self.kind.is_http() {
            return Err(format!("{} is {}", self.kind.label(), alouette_core::shared::REFUSED));
        }
        match self.kind {
            Kind::ClaudeCli | Kind::CodexCli | Kind::GeminiCli | Kind::Command => cli::ask(self, req).await,
            Kind::Ollama | Kind::OpenAi | Kind::Anthropic | Kind::Gemini => http::ask(self, net, req).await,
        }
    }

    /// A short name for the status line, such as `anthropic · claude-opus-5-5`.
    pub fn describe(&self) -> String {
        if self.model.is_empty() { self.kind.id().to_string() } else { format!("{} · {}", self.kind.id(), self.model) }
    }
}

fn is_local_base(base: Option<&str>) -> bool {
    base.and_then(|b| url::Url::parse(b).ok())
        .and_then(|u| u.host_str().map(|h| h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "[::1]"))
        .unwrap_or(false)
}

/// Split a command line on spaces, honouring double and single quotes.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert_eq!(split_command(r#"llm -m "my model" 'a b'"#), vec!["llm", "-m", "my model", "a b"]);
        assert_eq!(split_command("  x  "), vec!["x"]);
    }

    #[test]
    fn provider_needs() {
        let mut cfg = Ai { provider: Some("ollama".into()), ..Ai::default() };
        assert!(Provider::from_config(&cfg, None).unwrap_err().contains("model"));
        cfg.model = Some("llama3".into());
        let p = Provider::from_config(&cfg, None).unwrap();
        assert_eq!(p.base, "http://127.0.0.1:11434");
        cfg.provider = Some("anthropic".into());
        cfg.model = None;
        assert!(Provider::build(&cfg, None, |_| None).unwrap_err().contains("key"));
        let p = Provider::build(&cfg, Some("k".into()), |_| None).unwrap();
        assert_eq!(p.model, "claude-opus-5-5");
        cfg.base_url = Some("http://127.0.0.1:9/".into());
        assert_eq!(Provider::build(&cfg, None, |_| None).unwrap().base, "http://127.0.0.1:9");
    }

    #[test]
    fn flat() {
        let mut r = Request::new("sys", "hi");
        assert_eq!(r.flat(), "sys\n\nhi");
        r.messages.push((Role::Assistant, "hello".into()));
        assert!(r.flat().contains("Assistant: hello"));
    }
}
