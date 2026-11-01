#![no_main]
use libfuzzer_sys::fuzz_target;
use powerdb::security::magic;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, head)) = data.split_first() else { return };
    let mime = ["", "application/octet-stream", "image/png", "application/pdf", "text/plain"][sel as usize % 5];
    if magic::sniff(head).kind == magic::Kind::Executable {
        assert!(magic::check(mime, head).is_err(), "an executable was accepted");
    } else {
        let _ = magic::check(mime, head);
    }
});
