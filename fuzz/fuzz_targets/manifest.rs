#![no_main]
use libfuzzer_sys::fuzz_target;
use powerdb::storage::model::Manifest;

fuzz_target!(|data: &[u8]| {
    if let Some(m) = Manifest::from_bytes(data) {
        assert_eq!(m.to_bytes(), data);
        let _ = (m.file_id(), m.unique_keys());
    }
});
