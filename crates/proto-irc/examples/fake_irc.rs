//! Run the test IRC server for live checks: `cargo run -p proto-irc --features test-server --example fake_irc`.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (port, _state) = proto_irc::testserver::start().await;
    println!("IRC on 127.0.0.1:{port} (SASL account me / pw; message the bot `echo`)");
    std::future::pending::<()>().await;
}
