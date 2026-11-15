//! Run the test homeserver: `cargo run -p alouette-matrix --features test-server --example fake_matrix`.
#[tokio::main]
async fn main() {
    let (url, _state) = alouette_matrix::testserver::start().await;
    println!("Matrix homeserver at {url} (user @me:localhost, password pw)");
    std::future::pending::<()>().await;
}
