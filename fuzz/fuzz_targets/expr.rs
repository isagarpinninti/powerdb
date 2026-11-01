#![no_main]
use libfuzzer_sys::fuzz_target;
use powerdb::security::expr;
use serde_json::json;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else { return };
    if let Ok(e) = expr::parse(src) {
        let auth = json!({"id": "u1"});
        let body = json!({"a": 1});
        let ctx = expr::Ctx { record: None, auth: Some(&auth), body: Some(&body), method: "POST" };
        assert_eq!(e.eval(&ctx), e.eval(&ctx));
        let _ = (e.matches(&ctx), e.uses_record());
    }
});
