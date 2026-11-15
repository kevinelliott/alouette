#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    for item in proto_gopher::parse_menu(&s) { let _ = item.url(false); let _ = item.url(true); }
    let _ = proto_gopher::text_body(data);
});
