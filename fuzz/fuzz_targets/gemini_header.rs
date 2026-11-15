#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if let Ok(h) = proto_gemini::Header::parse(&s) { let _ = (h.kind(), h.mime(), h.describe()); }
});
