#![no_main]
use libfuzzer_sys::fuzz_target;
use powerdb::types::key::{decode_key, encode_key};
use powerdb::types::Type;

// A key that decodes, for any type, must decode to a value that encodes to a key that decodes to the same value (no panic, no unbounded work).
fuzz_target!(|data: &[u8]| {
    let types = [
        Type::Bool, Type::I8, Type::I16, Type::I32, Type::I64, Type::U8, Type::U16, Type::U32, Type::U64, Type::F32, Type::F64,
        Type::Decimal, Type::Text, Type::Uuid, Type::Date, Type::Time, Type::Timestamp, Type::TimestampTz, Type::Binary, Type::Enum,
    ];
    for t in &types {
        let mut cursor = data;
        if let Ok(v) = decode_key(&mut cursor, t) {
            let mut out = Vec::new();
            encode_key(&mut out, v.as_ref()).expect("a decoded value can be encoded");
            let mut again: &[u8] = &out;
            let back = decode_key(&mut again, t).expect("an encoded key decodes");
            assert!(again.is_empty());
            // (floats and decimals may differ in representation but encode to the same key)
            let mut out2 = Vec::new();
            encode_key(&mut out2, back.as_ref()).unwrap();
            assert_eq!(out, out2);
        }
    }
});
