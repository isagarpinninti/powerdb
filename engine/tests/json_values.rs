//! The JSON form of values (what the HTTP API sends and accepts): round trips for every type, the exact output forms, and strict refusal of what does
//! not fit.

use powerdb::types::convert::{format_date, format_timestamp};
use powerdb::types::json::{
    from_json, parse_date, parse_decimal, parse_time, parse_timestamp, to_json,
};
use powerdb::types::{Decimal, FileValue, Type, Value};
use serde_json::json;

fn rt(ty: &Type, v: Value) {
    let j = to_json(&v);
    let back =
        from_json(ty, &j).unwrap_or_else(|e| panic!("{ty:?} {v:?} -> {j} did not come back: {e}"));
    assert!(back.same(&v), "{ty:?}: {v:?} -> {j} -> {back:?}");
}

#[test]
fn every_type_round_trips_through_json() {
    for v in [i8::MIN, -1, 0, 1, i8::MAX] {
        rt(&Type::I8, Value::I8(v));
    }
    for v in [i16::MIN, -1, 0, i16::MAX] {
        rt(&Type::I16, Value::I16(v));
    }
    for v in [i32::MIN, -1, 0, i32::MAX] {
        rt(&Type::I32, Value::I32(v));
    }
    for v in [i64::MIN, -1, 0, i64::MAX, 9_007_199_254_740_993] {
        rt(&Type::I64, Value::I64(v));
    }
    for v in [0u8, 255] {
        rt(&Type::U8, Value::U8(v));
    }
    for v in [0u16, u16::MAX] {
        rt(&Type::U16, Value::U16(v));
    }
    for v in [0u32, u32::MAX] {
        rt(&Type::U32, Value::U32(v));
    }
    for v in [0u64, u64::MAX, 1 << 63] {
        rt(&Type::U64, Value::U64(v));
    }
    rt(&Type::Bool, Value::Bool(true));
    rt(&Type::Bool, Value::Bool(false));
    for v in [
        0.0f64,
        -0.0,
        1.5,
        -1e300,
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        0.1 + 0.2,
    ] {
        rt(&Type::F64, Value::F64(v));
    }
    for v in [
        0.0f32,
        -0.0,
        1.5,
        f32::MAX,
        f32::MIN_POSITIVE,
        f32::INFINITY,
        f32::NAN,
        16_777_217.0,
    ] {
        rt(&Type::F32, Value::F32(v));
    }
    for (u, s) in [
        (0i128, 0u8),
        (1250, 2),
        (-1250, 2),
        (1, 38),
        (10i128.pow(38) - 1, 0),
        (-(10i128.pow(38) - 1), 0),
        (100, 0),
        (5, 10),
    ] {
        rt(&Type::Decimal, Value::Decimal(Decimal::new(u, s).unwrap()));
    }
    for s in [
        "",
        "plain",
        "unicode \u{65e5}\u{672c} \u{1F600}",
        "quote \" and \\ and \n and \0",
    ] {
        rt(&Type::Text, Value::Text(s.into()));
    }
    rt(&Type::Uuid, Value::Uuid([0xAB; 16]));
    rt(&Type::Uuid, Value::Uuid([0; 16]));
    for d in [0, -1, 1, 20_000, -719_162, 2_932_896] {
        rt(&Type::Date, Value::Date(d));
    }
    for t in [0i64, 1, 86_399_999_999, 45_907_250_000] {
        rt(&Type::Time, Value::Time(t));
    }
    for t in [
        0i64,
        -1,
        1,
        1_794_408_963_554_000,
        -62_135_596_800_000_000,
        253_402_300_799_999_999,
    ] {
        rt(&Type::Timestamp, Value::Timestamp(t));
        rt(&Type::TimestampTz, Value::TimestampTz(t));
    }
    rt(
        &Type::Json,
        Value::Json(json!({"a": [1, 2.5, null, "x"], "b": {"c": true}})),
    );
    rt(&Type::Binary, Value::Binary(vec![]));
    rt(&Type::Binary, Value::Binary((0..=255).collect()));
    rt(
        &Type::Array(Box::new(Type::I64)),
        Value::Array(Type::I64, vec![Value::I64(1), Value::I64(i64::MAX)]),
    );
    rt(
        &Type::Array(Box::new(Type::Text)),
        Value::Array(Type::Text, vec![]),
    );
    rt(
        &Type::Array(Box::new(Type::Decimal)),
        Value::Array(
            Type::Decimal,
            vec![Value::Decimal(Decimal::new(5, 1).unwrap())],
        ),
    );
}

#[test]
fn the_output_forms_are_the_documented_ones() {
    assert_eq!(to_json(&Value::I64(5)), json!("5"), "int64 is a string");
    assert_eq!(
        to_json(&Value::U64(u64::MAX)),
        json!("18446744073709551615")
    );
    assert_eq!(
        to_json(&Value::I32(5)),
        json!(5),
        "smaller integers are numbers"
    );
    assert_eq!(
        to_json(&Value::Decimal(Decimal::new(1250, 2).unwrap())),
        json!("12.50"),
        "the scale is kept"
    );
    assert_eq!(
        to_json(&Value::Decimal(Decimal::new(-5, 3).unwrap())),
        json!("-0.005")
    );
    assert_eq!(to_json(&Value::F64(f64::NAN)), json!("NaN"));
    assert_eq!(to_json(&Value::F64(f64::NEG_INFINITY)), json!("-Infinity"));
    assert_eq!(to_json(&Value::F64(1.5)), json!(1.5));
    assert_eq!(to_json(&Value::Date(0)), json!("1970-01-01"));
    assert_eq!(to_json(&Value::Date(-1)), json!("1969-12-31"));
    assert_eq!(
        to_json(&Value::Time(45_907_250_000)),
        json!("12:45:07.250000")
    );
    assert_eq!(
        to_json(&Value::Timestamp(0)),
        json!("1970-01-01T00:00:00.000000")
    );
    assert_eq!(
        to_json(&Value::TimestampTz(1_000_000)),
        json!("1970-01-01T00:00:01.000000Z")
    );
    assert_eq!(to_json(&Value::Binary(b"hi".to_vec())), json!("aGk="));
    assert_eq!(
        to_json(&Value::Uuid([0x01; 16])),
        json!("01010101-0101-0101-0101-010101010101")
    );
    let f = FileValue {
        id: [0xAB; 32],
        size: 7,
        mime: "text/plain".into(),
        name: Some("a.txt".into()),
    };
    let j = to_json(&Value::File(f));
    assert_eq!(j["size"], 7);
    assert_eq!(j["mime"], "text/plain");
    assert_eq!(j["name"], "a.txt");
    assert_eq!(j["id"].as_str().unwrap().len(), 64);
}

#[test]
fn integers_are_accepted_as_numbers_or_digit_strings_and_range_checked() {
    assert_eq!(from_json(&Type::I64, &json!(5)).unwrap(), Value::I64(5));
    assert_eq!(
        from_json(&Type::I64, &json!("-9223372036854775808")).unwrap(),
        Value::I64(i64::MIN)
    );
    assert_eq!(
        from_json(&Type::U64, &json!("18446744073709551615")).unwrap(),
        Value::U64(u64::MAX)
    );
    assert_eq!(
        from_json(&Type::U64, &json!(18_446_744_073_709_551_615u64)).unwrap(),
        Value::U64(u64::MAX)
    );
    assert_eq!(from_json(&Type::I8, &json!("127")).unwrap(), Value::I8(127));
    for (ty, j) in [
        (Type::I8, json!(128)),
        (Type::I8, json!(-129)),
        (Type::U8, json!(-1)),
        (Type::U8, json!(256)),
        (Type::I32, json!(2_147_483_648i64)),
        (Type::I64, json!("9223372036854775808")),
        (Type::U64, json!("-1")),
        (Type::I32, json!(1.5)),
        (Type::I32, json!("1.5")),
        (Type::I32, json!("1e3")),
        (Type::I32, json!("")),
        (Type::I32, json!("-")),
        (Type::I32, json!(" 5")),
        (Type::I32, json!("+5")),
        (Type::I32, json!(true)),
        (Type::I32, json!(null)),
    ] {
        assert!(from_json(&ty, &j).is_err(), "{ty:?} accepted {j}");
    }
}

#[test]
fn floats_decimals_and_text() {
    assert!(matches!(from_json(&Type::F64, &json!("NaN")).unwrap(), Value::F64(f) if f.is_nan()));
    assert_eq!(
        from_json(&Type::F64, &json!("-Infinity")).unwrap(),
        Value::F64(f64::NEG_INFINITY)
    );
    assert!(
        from_json(&Type::F64, &json!("1.5")).is_err(),
        "a number as a string is not a float"
    );
    assert!(from_json(&Type::F64, &json!("nan")).is_err());
    assert!(
        from_json(&Type::F32, &json!(1e300)).is_err(),
        "too large for float32"
    );
    assert_eq!(from_json(&Type::F32, &json!(0.5)).unwrap(), Value::F32(0.5));
    let d = |s: &str| parse_decimal(s);
    assert_eq!(d("12.50"), Some(Decimal::new(1250, 2).unwrap()));
    assert_eq!(d("-0.00"), Some(Decimal::new(0, 2).unwrap()));
    assert_eq!(d("007.5"), Some(Decimal::new(75, 1).unwrap()));
    assert_eq!(d("0"), Some(Decimal::new(0, 0).unwrap()));
    assert!(d(&"9".repeat(38)).is_some());
    assert!(d(&"9".repeat(39)).is_none(), "39 digits");
    assert!(
        d(&format!("0.{}", "1".repeat(38))).is_some(),
        "a scale of 38"
    );
    assert!(
        d(&format!("0.{}", "1".repeat(39))).is_none(),
        "a scale of 39"
    );
    for bad in [
        "", ".5", "5.", "1e3", "1,5", "--1", "+1", " 1", "1 ", "0x10", "1.2.3", "-", "abc",
    ] {
        assert!(d(bad).is_none(), "accepted {bad:?}");
    }
    assert_eq!(
        from_json(&Type::Decimal, &json!(12.5)).unwrap(),
        Value::Decimal(Decimal::new(125, 1).unwrap())
    );
    assert!(
        from_json(&Type::Decimal, &json!(1e30)).is_err(),
        "an exponent in a number: use a string"
    );
    assert!(from_json(&Type::Text, &json!(5)).is_err());
    assert!(from_json(&Type::Bool, &json!("true")).is_err());
}

#[test]
fn uuids_dates_times_and_timestamps_are_strict() {
    assert!(from_json(&Type::Uuid, &json!("01010101-0101-0101-0101-010101010101")).is_ok());
    assert!(
        from_json(&Type::Uuid, &json!("01010101010101010101010101010101")).is_ok(),
        "32 digits"
    );
    assert!(
        from_json(&Type::Uuid, &json!("ABCDEFAB-CDEF-ABCD-EFAB-CDEFABCDEFAB")).is_ok(),
        "upper case"
    );
    for bad in [
        "",
        "1234",
        "01010101-0101-0101-0101-01010101010",
        "0101010-10101-0101-0101-010101010101",
        "zz010101-0101-0101-0101-010101010101",
    ] {
        assert!(from_json(&Type::Uuid, &json!(bad)).is_err(), "{bad:?}");
    }
    // dates
    assert_eq!(parse_date("1970-01-01"), Some(0));
    assert_eq!(parse_date("1969-12-31"), Some(-1));
    assert!(parse_date("2000-02-29").is_some(), "2000 is a leap year");
    assert!(parse_date("1900-02-29").is_none(), "1900 is not");
    assert!(parse_date("2024-02-29").is_some() && parse_date("2023-02-29").is_none());
    for bad in [
        "2026-13-01",
        "2026-00-10",
        "2026-04-31",
        "2026-4-1",
        "26-04-01",
        "2026/04/01",
        "2026-04-01 ",
        "",
        "2026-04-0a",
    ] {
        assert!(parse_date(bad).is_none(), "{bad:?}");
    }
    assert_eq!(parse_date("0001-01-01"), Some(-719_162));
    assert!(parse_date("9999-12-31").is_some());
    // times
    assert_eq!(parse_time("00:00:00"), Some(0));
    assert_eq!(parse_time("23:59:59.999999"), Some(86_399_999_999));
    assert_eq!(
        parse_time("12:45:07.25"),
        Some(45_907_250_000),
        "a short fraction is padded"
    );
    for bad in [
        "24:00:00",
        "12:60:00",
        "12:00:60",
        "1:00:00",
        "12:00",
        "12:00:00.",
        "12:00:00.1234567",
        "12:00:00,5",
        "12-00-00",
    ] {
        assert!(parse_time(bad).is_none(), "{bad:?}");
    }
    // timestamps
    assert_eq!(parse_timestamp("1970-01-01T00:00:00"), Some((0, false)));
    assert_eq!(
        parse_timestamp("1970-01-01 00:00:01"),
        Some((1_000_000, false)),
        "a space may replace the T"
    );
    assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some((0, true)));
    assert_eq!(
        parse_timestamp("2026-01-01T00:00:00+02:00"),
        parse_timestamp("2025-12-31T22:00:00Z")
    );
    assert_eq!(
        parse_timestamp("2026-01-01T00:00:00-05:30"),
        parse_timestamp("2026-01-01T05:30:00Z")
    );
    assert!(parse_timestamp("2026-01-01T00:00:00+24:00").is_none());
    assert!(parse_timestamp("2026-01-01T00:00:00+0200").is_none());
    assert!(parse_timestamp("2026-01-01T00:00:00 UTC").is_none());
    assert!(parse_timestamp("2026-01-01").is_none());
    // the zone rules of the two types
    assert!(
        from_json(&Type::TimestampTz, &json!("2026-11-12T13:45:07")).is_err(),
        "timestamptz needs a zone"
    );
    assert!(
        from_json(&Type::Timestamp, &json!("2026-11-12T13:45:07Z")).is_err(),
        "timestamp has no zone"
    );
    assert_eq!(
        from_json(&Type::TimestampTz, &json!("2026-11-12T15:45:07+02:00")).unwrap(),
        from_json(&Type::TimestampTz, &json!("2026-11-12T13:45:07Z")).unwrap()
    );
}

#[test]
fn every_day_from_1800_to_2300_parses_back_to_itself() {
    for d in -61_000..120_000i32 {
        let s = format_date(d);
        assert_eq!(parse_date(&s), Some(d), "{s}");
    }
    // and timestamps, across the range, with fractions
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let t = (x % 253_402_300_800_000_000u64) as i64 - 62_135_596_800_000_000; // year 0001 to 9999
        let s = format_timestamp(t, true);
        assert_eq!(parse_timestamp(&s), Some((t, true)), "{s}");
    }
}

#[test]
fn binary_arrays_json_and_files() {
    assert!(from_json(&Type::Binary, &json!("not base64!")).is_err());
    assert!(
        from_json(&Type::Binary, &json!("aGk")).is_err(),
        "unpadded base64 is refused: one form only"
    );
    assert_eq!(
        from_json(&Type::Binary, &json!("aGk=")).unwrap(),
        Value::Binary(b"hi".to_vec())
    );
    let arr = Type::Array(Box::new(Type::I32));
    assert!(from_json(&arr, &json!([1, 2, 3])).is_ok());
    assert!(
        from_json(&arr, &json!([1, "x"])).is_err(),
        "one bad element refuses the list"
    );
    assert!(from_json(&arr, &json!(5)).is_err());
    assert_eq!(
        from_json(&Type::Json, &json!(null)).unwrap(),
        Value::Json(json!(null))
    );
    assert!(
        from_json(&Type::File, &json!({"id": "00"})).is_err(),
        "a file is not a JSON value"
    );
}
