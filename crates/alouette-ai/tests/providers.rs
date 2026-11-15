//! Each HTTP provider against the fake server, through the real network stack.

use alouette_ai::prompt::{Task, build};
use alouette_ai::{Kind, Provider};
use alouette_core::config::{Ai, Network};
use alouette_net::Net;

#[tokio::test(flavor = "multi_thread")]
async fn every_http_provider_answers() {
    let (base, state) = alouette_ai::testserver::start().await;
    let net = Net::new(&Network::default()).unwrap();
    let req = build(&Task::Summarize, "email", "Lunch?", "Want lunch Friday at noon?", 800);
    for (kind, base, model) in [
        (Kind::Ollama, base.clone(), "tinyllama:latest"),
        (Kind::Anthropic, base.clone(), ""),
        (Kind::OpenAi, format!("{base}/v1"), "m"),
        (Kind::Gemini, base.clone(), "g"),
    ] {
        let cfg = Ai {
            provider: Some(kind.id().into()),
            base_url: Some(base),
            model: (!model.is_empty()).then(|| model.to_string()),
            ..Ai::default()
        };
        let p = Provider::from_config(&cfg, Some("k".into())).unwrap();
        let r = p.ask(&net, &req).await.unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert!(r.text.contains("meet on Friday"), "{kind:?}: {}", r.text);
        assert!(r.input_tokens > 0, "{kind:?}");
    }
    assert_eq!(state.lock().unwrap().requests.len(), 4);
    let models = alouette_ai::http::ollama_models(&net, &base).await.unwrap();
    assert_eq!(models[0], "tinyllama:latest");

    let cfg = Ai { provider: Some("anthropic".into()), base_url: Some(base.clone()), ..Ai::default() };
    let p = Provider::from_config(&cfg, Some("bad".into())).unwrap();
    assert!(p.ask(&net, &req).await.unwrap_err().contains("refused the key"));
}
