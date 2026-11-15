//! Providers that are programs on this machine: the claude, codex and gemini CLIs, or any
//! command that reads a prompt on stdin and writes an answer on stdout. They use the person's
//! own sign-in, so Alouette never sees a key.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::{Kind, Provider, Reply, Request};

/// The longest a CLI may take.
const TIMEOUT: Duration = Duration::from_secs(300);

/// The program and arguments for a CLI provider. The prompt always goes on stdin, so it never
/// shows up in a process list.
pub fn argv(p: &Provider) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut a = match p.kind {
        Kind::ClaudeCli => s(&["claude", "-p", "--output-format", "json"]),
        Kind::CodexCli => s(&["codex", "exec", "--skip-git-repo-check", "-"]),
        Kind::GeminiCli => s(&["gemini", "-p", "Follow the instructions above."]),
        Kind::Command => p.command.clone(),
        _ => Vec::new(),
    };
    if !p.model.is_empty() {
        match p.kind {
            Kind::ClaudeCli => a.extend(s(&["--model", &p.model])),
            Kind::CodexCli => a.splice(2..2, s(&["--model", &p.model])).for_each(drop),
            Kind::GeminiCli => a.extend(s(&["--model", &p.model])),
            _ => {}
        }
    }
    a
}

pub fn parse(kind: Kind, model: &str, stdout: &str) -> Result<Reply, String> {
    let mut r = Reply { model: model.to_string(), ..Reply::default() };
    if kind == Kind::ClaudeCli {
        // The JSON result: {"type":"result","is_error":false,"result":"...","total_cost_usd":0.01,"usage":{...}}
        let v: Value = stdout
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<Value>(l).ok().filter(|v| v.get("result").is_some()))
            .or_else(|| serde_json::from_str(stdout).ok())
            .ok_or_else(|| {
                format!("claude did not answer in JSON: {}", stdout.chars().take(200).collect::<String>())
            })?;
        if v["is_error"] == true {
            return Err(format!("claude: {}", v["result"].as_str().unwrap_or("failed")));
        }
        r.text = v["result"].as_str().unwrap_or("").to_string();
        let u = &v["usage"];
        r.input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        r.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
        r.cost = v["total_cost_usd"].as_f64();
    } else {
        r.text = stdout.trim().to_string();
    }
    if r.text.trim().is_empty() {
        return Err(format!("{} gave an empty answer", kind.id()));
    }
    Ok(r)
}

pub async fn ask(p: &Provider, req: &Request) -> Result<Reply, String> {
    let a = argv(p);
    let (prog, args) = a.split_first().ok_or("no command to run")?;
    let mut child = tokio::process::Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start {prog}: {e}"))?;
    let prompt = req.flat();
    if let Some(mut stdin) = child.stdin.take() {
        // A program that exits early closes the pipe; its output says why.
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("{prog} took longer than {} seconds", TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().chain(stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output");
        // The claude CLI reports its own errors as JSON on stdout.
        if p.kind == Kind::ClaudeCli
            && let Err(e) = parse(p.kind, &p.model, &stdout)
            && !e.contains("did not answer in JSON")
        {
            return Err(e);
        }
        return Err(format!("{prog} failed ({}): {first}", out.status));
    }
    parse(p.kind, &p.model, &stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(kind: Kind, model: &str) -> Provider {
        Provider { kind, model: model.into(), base: String::new(), key: None, command: vec!["cat".into()] }
    }

    #[test]
    fn args() {
        assert_eq!(argv(&prov(Kind::ClaudeCli, "")), ["claude", "-p", "--output-format", "json"]);
        assert_eq!(argv(&prov(Kind::CodexCli, "o3")), ["codex", "exec", "--model", "o3", "--skip-git-repo-check", "-"]);
        assert_eq!(argv(&prov(Kind::Command, "")), ["cat"]);
    }

    #[test]
    fn claude_json() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"Sum.","total_cost_usd":0.0123,"usage":{"input_tokens":5,"cache_read_input_tokens":100,"output_tokens":7}}"#;
        let r = parse(Kind::ClaudeCli, "", out).unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens, r.output_tokens, r.cost), ("Sum.", 105, 7, Some(0.0123)));
        assert!(
            parse(Kind::ClaudeCli, "", r#"{"is_error":true,"result":"Not logged in"}"#)
                .unwrap_err()
                .contains("Not logged in")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_command() {
        let p = prov(Kind::Command, "");
        let r = ask(&p, &Request::new("sys", "hello there")).await.unwrap();
        assert_eq!(r.text, "sys\n\nhello there");
        let mut bad = prov(Kind::Command, "");
        bad.command = vec!["sh".into(), "-c".into(), "echo oops >&2; exit 3".into()];
        assert!(ask(&bad, &Request::new("", "x")).await.unwrap_err().contains("oops"));
    }
}
