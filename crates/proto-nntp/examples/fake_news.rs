//! An in-memory news server for trying the News window by hand.
//!
//! `cargo run -p proto-nntp --example fake_news` prints the address; open
//! `nntp://127.0.0.1:<port>/` in Alouette.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let srv = proto_nntp::testserver::Server::new();
    srv.group("comp.lang.rust", "The Rust programming language");
    srv.group("alt.folklore.computers", "Stories of computing past");
    srv.group("comp.sys.minuet", "The Minnesota Internet Users Essential Tool");
    let mut n = 0;
    for (i, subject) in ["Is Alouette any good?", "Gopher still lives", "NNTP in 2026"].iter().enumerate() {
        n += 1;
        let root = format!("<r{n}@example.org>");
        let a = format!(
            "From: Ada <ada@example.org>\r\nNewsgroups: comp.sys.minuet\r\nSubject: {subject}\r\nDate: Tue, {} Jan 2024 10:00:00 +0000\r\nMessage-ID: {root}\r\n\r\nWhat does everyone think?\r\n\r\n-- \r\nAda\r\n",
            i + 2
        );
        srv.deliver("comp.sys.minuet", a.as_bytes());
        for j in 0..2 {
            n += 1;
            let r = format!(
                "From: Grace <grace@example.org>\r\nNewsgroups: comp.sys.minuet\r\nSubject: Re: {subject}\r\nDate: Tue, {} Jan 2024 11:0{j}:00 +0000\r\nMessage-ID: <r{n}@example.org>\r\nReferences: {root}\r\n\r\nAda wrote:\r\n> What does everyone think?\r\n\r\nI like it, reply {j}.\r\n",
                i + 2
            );
            srv.deliver("comp.sys.minuet", r.as_bytes());
        }
    }
    srv.deliver("comp.lang.rust", b"From: Kay <kay@example.org>\r\nNewsgroups: comp.lang.rust\r\nSubject: Borrow checker tips\r\nDate: Wed, 3 Jan 2024 09:00:00 +0000\r\nMessage-ID: <k1@example.org>\r\n\r\nClone less, borrow more.\r\n");
    let addr = proto_nntp::testserver::start(srv).await;
    println!("nntp://{addr}/");
    std::future::pending::<()>().await;
}
