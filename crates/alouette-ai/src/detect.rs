//! What is available on this machine, for the setup screen.

use std::path::{Path, PathBuf};

use alouette_net::Net;

use crate::Kind;

/// One choice on the setup screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub kind: Kind,
    /// Why it is offered, such as `/usr/local/bin/claude` or `2 models`.
    pub detail: String,
    /// True when it can be used without anything more from the person.
    pub ready: bool,
    /// Models found, for Ollama.
    pub models: Vec<String>,
}

/// Find a program on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or(".EXE;.CMD;.BAT".into()).split(';').map(|s| s.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if is_exec(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn is_exec(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// Look for CLIs on PATH, a running Ollama, and keys in the environment.
pub async fn detect(net: &Net, ollama_base: Option<&str>) -> Vec<Found> {
    let mut out = Vec::new();
    for (kind, prog) in [(Kind::ClaudeCli, "claude"), (Kind::CodexCli, "codex"), (Kind::GeminiCli, "gemini")] {
        if let Some(p) = which(prog) {
            out.push(Found { kind, detail: p.display().to_string(), ready: true, models: vec![] });
        }
    }
    let base = ollama_base.unwrap_or(Kind::Ollama.default_base().unwrap_or_default());
    let probe = tokio::time::timeout(std::time::Duration::from_secs(2), crate::http::ollama_models(net, base)).await;
    if let Ok(Ok(models)) = probe {
        let detail = match models.len() {
            0 => "running, but has no models yet (ollama pull …)".to_string(),
            1 => format!("running, model {}", models[0]),
            n => format!("running, {n} models"),
        };
        out.push(Found { kind: Kind::Ollama, detail, ready: !models.is_empty(), models });
    }
    for kind in [Kind::Anthropic, Kind::OpenAi, Kind::Gemini] {
        let env = kind.key_env().unwrap_or_default();
        let has = std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false);
        let detail = if has { format!("key found in {env}") } else { "needs an API key".to_string() };
        out.push(Found { kind, detail, ready: has, models: vec![] });
    }
    out.push(Found {
        kind: Kind::Command, detail: "for llm, aichat, a script…".into(), ready: false, models: vec![]
    });
    out
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn finds_sh() {
        assert!(super::which("sh").is_some());
        assert!(super::which("surely-not-a-program-here").is_none());
    }
}
