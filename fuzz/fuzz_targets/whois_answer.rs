#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = proto_whois::referral(&s); let _ = proto_whois::parse_cymru(&s);
});
