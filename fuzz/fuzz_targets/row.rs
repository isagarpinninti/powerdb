#![no_main]
use libfuzzer_sys::fuzz_target;
use powerdb::types::row::{decode_row, encode_row};

// Whatever the decoder accepts is the one canonical encoding of what it read, so re-encoding gives the same bytes. Anything else must be refused
// without panicking and without asking for a huge allocation.
fuzz_target!(|data: &[u8]| {
    if let Ok(cols) = decode_row(data) {
        assert_eq!(encode_row(&cols).expect("a decoded row can be encoded"), data);
    }
});
