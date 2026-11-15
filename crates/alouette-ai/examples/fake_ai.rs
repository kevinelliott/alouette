//! A fake model server for live checks: `cargo run -p alouette-ai --features test-server --example fake_ai`.
//! Point Alouette at it with `ai.base_url` (any provider but the CLIs) and any key but `bad`.

#[tokio::main]
async fn main() {
    let (url, _state) = alouette_ai::testserver::start().await;
    println!("fake AI server at {url}");
    println!("  ollama:    ai.provider = \"ollama\", ai.base_url = \"{url}\", ai.model = \"tinyllama:latest\"");
    println!("  anthropic: ai.provider = \"anthropic\", ai.base_url = \"{url}\"");
    println!("  openai:    ai.provider = \"openai\", ai.base_url = \"{url}/v1\", ai.model = \"m\"");
    println!("  gemini:    ai.provider = \"gemini\", ai.base_url = \"{url}\", ai.model = \"g\"");
    std::future::pending::<()>().await;
}
