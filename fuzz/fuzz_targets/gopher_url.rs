#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if let Ok(u) = proto_gopher::GopherUrl::parse(&s) { let again = proto_gopher::GopherUrl::parse(&u.to_string()); assert!(again.is_ok(), "{u}"); let _ = u.request(); }
});
