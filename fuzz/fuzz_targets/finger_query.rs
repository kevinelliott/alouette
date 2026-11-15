#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if let Ok(q) = proto_finger::Query::parse(&s) { let _ = (q.url(), q.request()); }
    let _ = proto_finger::clean(&s);
});
