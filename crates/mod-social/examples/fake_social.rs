//! Run a fake Mastodon server: `cargo run -p mod-social --example fake_social`.
//! Paste `tok` when Alouette asks for a token.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (base, log) = mod_social::testserver::start().await;
    println!("{base}");
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for l in log.lock().unwrap().drain(..) {
            eprintln!("{l}");
        }
    }
}
