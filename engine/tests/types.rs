//! The type system's codecs: rows and keys. Seeded random tests, so a failure repeats. `POWERDB_TYPES_ITERS` raises the number of cases.

use powerdb::types::key::{decode_key, encode_key};
use powerdb::types::row::{decode_row, encode_row};
use powerdb::types::{Decimal, FileValue, Type, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cmp::Ordering;

/// A single allocation of this size or more is a bug in a decoder that should have checked a length first.
struct Guard;
unsafe impl GlobalAlloc for Guard {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        assert!(
            l.size() < (256 << 20),
            "a decoder asked for {} bytes in one allocation",
            l.size()
        );
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        assert!(
            n < (256 << 20),
            "a decoder asked for {n} bytes in one allocation"
        );
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Guard = Guard;

fn iters() -> usize {
    std::env::var("POWERDB_TYPES_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000)
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

fn all_types() -> Vec<Type> {
    vec![
        Type::Bool,
        Type::I8,
        Type::I16,
        Type::I32,
        Type::I64,
        Type::U8,
        Type::U16,
        Type::U32,
        Type::U64,
        Type::F32,
        Type::F64,
        Type::Decimal,
        Type::Text,
        Type::Uuid,
        Type::Date,
        Type::Time,
        Type::Timestamp,
        Type::TimestampTz,
        Type::Json,
        Type::Binary,
        Type::Enum,
        Type::Array(Box::new(Type::I32)),
        Type::File,
        Type::Number,
    ]
}

fn indexable_types() -> Vec<Type> {
    all_types().into_iter().filter(|t| t.indexable()).collect()
}

fn text(r: &mut Rng) -> String {
    let alphabet = ['a', 'b', 'z', '\0', ' ', 'é', '日', '\u{1F600}', '0', 'A'];
    (0..r.below(8)).map(|_| r.pick(&alphabet)).collect()
}

fn json(r: &mut Rng, depth: usize) -> serde_json::Value {
    use serde_json::{Value as J, json};
    match r.below(if depth == 0 { 5 } else { 7 }) {
        0 => J::Null,
        1 => J::Bool(r.below(2) == 0),
        2 => json!(r.next() as i64),
        3 => json!(
            f64::from_bits(r.next()).clamp(-1e300, 1e300).max(-1e300)
                * if r.below(2) == 0 { 1.0 } else { 1e-3 }
        ),
        4 => J::String(text(r)),
        5 => J::Array((0..r.below(4)).map(|_| json(r, depth - 1)).collect()),
        _ => J::Object(
            (0..r.below(4))
                .map(|_| (text(r), json(r, depth - 1)))
                .collect(),
        ),
    }
}

fn value(r: &mut Rng, t: &Type) -> Value {
    let n = r.next();
    match t {
        Type::Bool => Value::Bool(r.below(2) == 0),
        Type::I8 => Value::I8(r.pick(&[i8::MIN, -1, 0, 1, i8::MAX, n as i8])),
        Type::I16 => Value::I16(r.pick(&[i16::MIN, -1, 0, 1, i16::MAX, n as i16])),
        Type::I32 => Value::I32(r.pick(&[i32::MIN, -1, 0, 1, i32::MAX, n as i32])),
        Type::I64 => Value::I64(r.pick(&[i64::MIN, -1, 0, 1, i64::MAX, n as i64])),
        Type::U8 => Value::U8(r.pick(&[0, 1, u8::MAX, n as u8])),
        Type::U16 => Value::U16(r.pick(&[0, 1, u16::MAX, n as u16])),
        Type::U32 => Value::U32(r.pick(&[0, 1, u32::MAX, n as u32])),
        Type::U64 => Value::U64(r.pick(&[0, 1, u64::MAX, n])),
        Type::F32 => Value::F32(r.pick(&[
            0.0,
            -0.0,
            1.0,
            -1.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::MIN_POSITIVE,
            f32::MAX,
            f32::from_bits(n as u32),
        ])),
        Type::F64 => Value::F64(r.pick(&[
            0.0,
            -0.0,
            1.0,
            -1.0,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::from_bits(n),
        ])),
        Type::Decimal => {
            let scale = r.below(13) as u8;
            let mag = r.pick(&[0i128, 1, 9, 10, 99, 100, 12345, 1_000_000_000_000_000_000]);
            let u = if r.below(3) == 0 {
                (r.next() % 1_000_000_000_000_000_000) as i128
            } else {
                mag
            };
            Value::Decimal(Decimal::new(if r.below(2) == 0 { -u } else { u }, scale).unwrap())
        }
        Type::Text => Value::Text(text(r)),
        Type::Binary => Value::Binary(
            (0..r.below(8))
                .map(|_| r.pick(&[0u8, 1, 0xFF, n as u8]))
                .collect(),
        ),
        Type::Uuid => {
            let mut u = [0u8; 16];
            for b in &mut u {
                *b = r.next() as u8;
            }
            Value::Uuid(u)
        }
        Type::Date => Value::Date(r.pick(&[i32::MIN, -1, 0, 19_000, i32::MAX, n as i32])),
        Type::Time => Value::Time(r.pick(&[0, 1, 86_399_999_999, n as i64 % 86_400_000_000])),
        Type::Timestamp => {
            Value::Timestamp(r.pick(&[i64::MIN, -1, 0, 1_700_000_000_000_000, i64::MAX, n as i64]))
        }
        Type::TimestampTz => Value::TimestampTz(r.pick(&[
            i64::MIN,
            -1,
            0,
            1_700_000_000_000_000,
            i64::MAX,
            n as i64,
        ])),
        Type::Json => Value::Json(json(r, 3)),
        Type::Enum => Value::Enum(r.pick(&[0, 1, 2, u32::MAX, n as u32])),
        Type::Array(e) => Value::Array(
            (**e).clone(),
            (0..r.below(5)).map(|_| value(r, e)).collect(),
        ),
        Type::File => {
            let mut f = FileValue {
                object: [0; 16],
                version: [0; 16],
            };
            for b in f.object.iter_mut().chain(f.version.iter_mut()) {
                *b = r.next() as u8;
            }
            Value::File(f)
        }
        Type::Number => Value::Number(match r.below(3) {
            0 => serde_json::Number::from(r.next() as i64),
            1 => serde_json::Number::from(r.next() | (1 << 63)),
            _ => {
                let f = f64::from_bits(r.next());
                serde_json::Number::from_f64(if f.is_nan() {
                    0.5
                } else {
                    f.clamp(-1e300, 1e300)
                })
                .unwrap()
            }
        }),
    }
}

fn row(r: &mut Rng) -> Vec<(u32, Value)> {
    let types = all_types();
    let mut ids: Vec<u32> = (0..r.below(8))
        .map(|_| {
            let m = r.next();
            r.pick(&[0u32, 1, 2, 7, 127, 128, 300, 16_384, u32::MAX, m as u32])
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .map(|id| {
            let t = types[r.below(types.len())].clone();
            let t = if let Type::Array(_) = t {
                let inner = types
                    .iter()
                    .filter(|x| !matches!(x, Type::Array(_)))
                    .cloned()
                    .collect::<Vec<_>>();
                Type::Array(Box::new(inner[r.below(inner.len())].clone()))
            } else {
                t
            };
            (id, value(r, &t))
        })
        .collect()
}

fn same_row(a: &[(u32, Value)], b: &[(u32, Value)]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && x.1.same(&y.1))
}

// ---- rows ----

#[test]
fn every_row_round_trips_exactly_and_has_one_encoding() {
    let mut r = Rng::new(1);
    for i in 0..iters() {
        let cols = row(&mut r);
        let bytes = encode_row(&cols).unwrap_or_else(|e| panic!("case {i}: {e}: {cols:?}"));
        let back = decode_row(&bytes).unwrap_or_else(|e| panic!("case {i}: {e}"));
        assert!(
            same_row(&cols, &back),
            "case {i}: {cols:?} came back as {back:?}"
        );
        assert_eq!(
            encode_row(&back).unwrap(),
            bytes,
            "case {i}: re-encoding changed the bytes"
        );
    }
}

#[test]
fn column_order_given_does_not_matter_and_a_repeat_is_refused() {
    let a = vec![(5u32, Value::I32(1)), (2, Value::Text("x".into()))];
    let b = vec![(2u32, Value::Text("x".into())), (5, Value::I32(1))];
    assert_eq!(encode_row(&a).unwrap(), encode_row(&b).unwrap());
    assert!(encode_row(&[(1, Value::Bool(true)), (1, Value::Bool(false))]).is_err());
}

#[test]
fn a_row_with_no_columns_is_two_bytes() {
    assert_eq!(encode_row(&[]).unwrap(), vec![1, 0]);
    assert!(decode_row(&[1, 0]).unwrap().is_empty());
}

#[test]
fn malformed_rows_are_refused() {
    let ok = |cols: &[(u32, Value)]| encode_row(cols).unwrap();
    let base = ok(&[(1, Value::I32(5)), (2, Value::Text("hi".into()))]);
    assert!(decode_row(&base).is_ok());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty input", vec![]),
        ("a version this build does not know", vec![2, 0]),
        ("trailing bytes", [base.clone(), vec![0]].concat()),
        ("truncated", base[..base.len() - 1].to_vec()),
        ("columns out of order", vec![1, 2, 5, 1, 1, 0, 1, 1, 1]),
        ("a column twice", vec![1, 2, 5, 1, 1, 5, 1, 1]),
        ("a boolean that is 2", vec![1, 1, 1, 1, 2]),
        ("an unknown type tag", vec![1, 1, 1, 99, 0]),
        ("a tag in the reserved range", vec![1, 1, 1, 32, 0]),
        ("an i8 out of range", vec![1, 1, 1, 2, 0xFF, 0x7F]),
        (
            "a varint with a pointless zero byte",
            vec![1, 1, 1, 4, 0x80, 0x00],
        ),
        ("text that is not UTF-8", vec![1, 1, 1, 13, 2, 0xFF, 0xFE]),
        ("text longer than the row", vec![1, 1, 1, 13, 100, b'a']),
        (
            "a huge text length",
            vec![1, 1, 1, 13, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
        ),
        (
            "an array count longer than the row",
            vec![1, 1, 1, 22, 4, 0xFF, 0xFF, 0xFF, 0x0F],
        ),
        ("an array of arrays", vec![1, 1, 1, 22, 22, 0]),
        ("a huge column count", vec![1, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
        (
            "JSON with spaces (not canonical)",
            [vec![1, 1, 1, 19, 8], b"{ \"a\":1}".to_vec()].concat(),
        ),
        (
            "JSON with unsorted keys",
            [vec![1, 1, 1, 19, 13], b"{\"b\":1,\"a\":2}".to_vec()].concat(),
        ),
        (
            "a decimal with too many digits",
            // a scale of 0, then a 19-byte varint: far more than 38 digits
            [vec![1, 1, 1, 12, 0, 0xFE], vec![0xFF; 17], vec![0x03]].concat(),
        ),
        ("a decimal scale of 99", vec![1, 1, 1, 12, 99, 2]),
        (
            "a legacy number in a non-canonical kind",
            vec![1, 1, 1, 24, 1, 2],
        ),
    ];
    for (what, bytes) in cases {
        assert!(
            decode_row(&bytes).is_err(),
            "{what} was accepted: {bytes:?}"
        );
    }
}

#[test]
fn an_unknown_future_type_is_refused_with_a_message_that_says_to_upgrade() {
    let e = decode_row(&[1, 1, 1, 40, 0]).unwrap_err().to_string();
    assert!(e.contains("newer PowerDB"), "{e}");
    let e = decode_row(&[9, 0]).unwrap_err().to_string();
    assert!(e.contains("newer PowerDB"), "{e}");
}

#[test]
fn values_that_cannot_be_written_are_refused() {
    assert!(
        encode_row(&[(1, Value::Array(Type::Array(Box::new(Type::I32)), vec![]))]).is_err(),
        "array of arrays"
    );
    assert!(
        encode_row(&[(1, Value::Array(Type::I32, vec![Value::Text("x".into())]))]).is_err(),
        "mixed array"
    );
    assert!(
        encode_row(&[(
            1,
            Value::Decimal(Decimal {
                unscaled: i128::MAX,
                scale: 0
            })
        )])
        .is_err(),
        "decimal too big"
    );
    let mut deep = serde_json::Value::Null;
    for _ in 0..100 {
        deep = serde_json::Value::Array(vec![deep]);
    }
    assert!(
        encode_row(&[(1, Value::Json(deep))]).is_err(),
        "JSON too deep"
    );
    assert!(
        encode_row(&[(1, Value::Binary(vec![0; 17 << 20]))]).is_err(),
        "a row over the limit"
    );
    let many: Vec<(u32, Value)> = (0..5000).map(|i| (i, Value::Bool(true))).collect();
    assert!(encode_row(&many).is_err(), "too many columns");
}

#[test]
fn the_decoder_survives_mutation_and_whatever_it_accepts_is_canonical() {
    let mut r = Rng::new(2);
    let mut accepted = 0;
    for _ in 0..iters() * 4 {
        let mut bytes = encode_row(&row(&mut r)).unwrap();
        for _ in 0..1 + r.below(4) {
            match r.below(5) {
                0 if !bytes.is_empty() => {
                    let i = r.below(bytes.len());
                    bytes[i] ^= 1 << r.below(8);
                }
                1 if !bytes.is_empty() => {
                    let i = r.below(bytes.len());
                    let n = r.next();
                    bytes[i] = r.pick(&[0, 1, 0x7F, 0x80, 0xFF, n as u8]);
                }
                2 if !bytes.is_empty() => {
                    bytes.truncate(r.below(bytes.len()));
                }
                3 => {
                    let i = r.below(bytes.len() + 1);
                    bytes.insert(i, r.next() as u8);
                }
                _ if !bytes.is_empty() => {
                    let i = r.below(bytes.len());
                    bytes.remove(i);
                }
                _ => {}
            }
        }
        // must neither panic nor ask for a giant allocation (the guard above); if it accepts, the bytes must be the one canonical encoding
        if let Ok(cols) = decode_row(&bytes) {
            accepted += 1;
            assert_eq!(
                encode_row(&cols).unwrap(),
                bytes,
                "accepted bytes that are not canonical: {bytes:?}"
            );
        }
    }
    assert!(
        accepted > 0,
        "the mutations never produced a valid row: the check above proved nothing"
    );
}

#[test]
fn a_typical_row_is_smaller_than_its_json() {
    // an invoice, as the demo data has them
    let json = serde_json::json!({"id": "01a1cb1bdd720dbb82eeb1626d1b8adb", "number": "INV-1077", "customer": "Halcyon Labs", "amount": 7800, "status": "paid",
        "issued": "2026-09-19", "created_at": 1794408963554i64, "updated_at": 1794408963554i64});
    let json_len = serde_json::to_vec(&json).unwrap().len();
    let cols = vec![
        (1u32, Value::Uuid([7; 16])),
        (2, Value::Text("INV-1077".into())),
        (3, Value::Text("Halcyon Labs".into())),
        (4, Value::Decimal(Decimal::new(780_000, 2).unwrap())),
        (5, Value::Enum(1)),
        (6, Value::Date(20_350)),
        (7, Value::TimestampTz(1_794_408_963_554_000)),
        (8, Value::TimestampTz(1_794_408_963_554_000)),
    ];
    let row_len = encode_row(&cols).unwrap().len();
    println!("typical invoice: {row_len} bytes as a row, {json_len} bytes as JSON");
    assert!(row_len < json_len, "{row_len} >= {json_len}");
}

// ---- keys ----

fn cmp_f(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap(), // -0.0 equals 0.0
    }
}

/// The order a value *should* have, written independently of the encoder.
fn cmp_values(a: &Value, b: &Value) -> Ordering {
    use Value::*;
    match (a, b) {
        (Bool(x), Bool(y)) => x.cmp(y),
        (I8(x), I8(y)) => x.cmp(y),
        (I16(x), I16(y)) => x.cmp(y),
        (I32(x), I32(y)) => x.cmp(y),
        (I64(x), I64(y)) => x.cmp(y),
        (U8(x), U8(y)) => x.cmp(y),
        (U16(x), U16(y)) => x.cmp(y),
        (U32(x), U32(y)) => x.cmp(y),
        (U64(x), U64(y)) => x.cmp(y),
        (F32(x), F32(y)) => cmp_f(*x as f64, *y as f64),
        (F64(x), F64(y)) => cmp_f(*x, *y),
        (Decimal(x), Decimal(y)) => {
            // align the scales in plain integer arithmetic (the test values are small enough to fit)
            let m = x.scale.max(y.scale) as u32;
            let ax = x.unscaled * 10i128.pow(m - x.scale as u32);
            let ay = y.unscaled * 10i128.pow(m - y.scale as u32);
            ax.cmp(&ay)
        }
        (Text(x), Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Binary(x), Binary(y)) => x.cmp(y),
        (Uuid(x), Uuid(y)) => x.cmp(y),
        (Date(x), Date(y)) => x.cmp(y),
        (Time(x), Time(y)) | (Timestamp(x), Timestamp(y)) | (TimestampTz(x), TimestampTz(y)) => {
            x.cmp(y)
        }
        (Enum(x), Enum(y)) => x.cmp(y),
        _ => panic!("different types"),
    }
}

fn key(v: Option<&Value>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_key(&mut out, v).unwrap();
    out
}

fn equal_after_round_trip(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F32(x), Value::F32(y)) => (x.is_nan() && y.is_nan()) || x == y, // -0.0 comes back as 0.0, which is equal
        (Value::F64(x), Value::F64(y)) => (x.is_nan() && y.is_nan()) || x == y,
        (Value::Decimal(x), Value::Decimal(y)) => x.cmp_numeric(y) == Ordering::Equal,
        (x, y) => x == y,
    }
}

#[test]
fn key_order_is_value_order_for_every_indexable_type() {
    let mut r = Rng::new(3);
    for t in indexable_types() {
        for i in 0..iters() {
            let (a, b) = (value(&mut r, &t), value(&mut r, &t));
            let want = cmp_values(&a, &b);
            let got = key(Some(&a)).cmp(&key(Some(&b)));
            assert_eq!(got, want, "{t:?} case {i}: {a:?} vs {b:?}");
            // and the key decodes back to an equal value, using up exactly its own bytes
            let k = key(Some(&a));
            let mut cursor: &[u8] = &k;
            let back = decode_key(&mut cursor, &t).unwrap().unwrap();
            assert!(
                equal_after_round_trip(&a, &back),
                "{t:?}: {a:?} decoded as {back:?}"
            );
            assert!(cursor.is_empty(), "{t:?}: the decoder left bytes behind");
        }
    }
}

#[test]
fn nulls_sort_before_every_value() {
    let mut r = Rng::new(4);
    for t in indexable_types() {
        let null = key(None);
        for _ in 0..200 {
            let v = value(&mut r, &t);
            assert!(
                null < key(Some(&v)),
                "{t:?}: a null did not sort first against {v:?}"
            );
        }
        let mut cursor: &[u8] = &null;
        assert!(decode_key(&mut cursor, &t).unwrap().is_none());
    }
}

#[test]
fn floats_have_a_total_order_with_nan_last_and_negative_zero_equal_to_zero() {
    let ordered = [
        f64::NEG_INFINITY,
        -1e300,
        -1.0,
        -f64::MIN_POSITIVE,
        0.0,
        f64::MIN_POSITIVE,
        1.0,
        1e300,
        f64::INFINITY,
        f64::NAN,
    ];
    let keys: Vec<Vec<u8>> = ordered.iter().map(|v| key(Some(&Value::F64(*v)))).collect();
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "the order is not strictly increasing"
    );
    assert_eq!(key(Some(&Value::F64(-0.0))), key(Some(&Value::F64(0.0))));
    assert_eq!(
        key(Some(&Value::F64(f64::NAN))),
        key(Some(&Value::F64(-f64::NAN)))
    );
    assert_eq!(key(Some(&Value::F32(-0.0))), key(Some(&Value::F32(0.0))));
}

#[test]
fn decimals_with_different_scales_but_one_value_are_one_key() {
    let d = |u: i128, s: u8| Value::Decimal(Decimal::new(u, s).unwrap());
    assert_eq!(key(Some(&d(150, 2))), key(Some(&d(15, 1))));
    assert_eq!(key(Some(&d(1500, 3))), key(Some(&d(15, 1))));
    assert_eq!(key(Some(&d(0, 0))), key(Some(&d(0, 5))));
    assert_eq!(key(Some(&d(-150, 2))), key(Some(&d(-15, 1))));
    assert_ne!(key(Some(&d(15, 1))), key(Some(&d(-15, 1))));
    // ordering across signs, magnitudes and lengths
    let ordered = [
        d(-100, 0),
        d(-15, 1),
        d(-1, 0),
        d(-1, 3),
        d(0, 0),
        d(1, 3),
        d(1, 0),
        d(11, 1),
        d(12, 1),
        d(100, 0),
        d(1001, 1),
    ];
    let keys: Vec<Vec<u8>> = ordered.iter().map(|v| key(Some(v))).collect();
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "decimals are not in order"
    );
    // the largest and smallest values the type allows
    let max = Decimal::new(10i128.pow(38) - 1, 0).unwrap();
    let min = Decimal::new(-(10i128.pow(38) - 1), 0).unwrap();
    let tiny = Decimal::new(1, 38).unwrap();
    for x in [max, min, tiny] {
        let k = key(Some(&Value::Decimal(x)));
        let mut c: &[u8] = &k;
        let Value::Decimal(back) = decode_key(&mut c, &Type::Decimal).unwrap().unwrap() else {
            panic!()
        };
        assert_eq!(
            back.cmp_numeric(&x),
            Ordering::Equal,
            "{x} decoded as {back}"
        );
    }
    assert!(
        key(Some(&Value::Decimal(min))) < key(Some(&Value::Decimal(tiny)))
            && key(Some(&Value::Decimal(tiny))) < key(Some(&Value::Decimal(max)))
    );
}

#[test]
fn composite_keys_sort_column_by_column_and_never_collide() {
    let comp = |a: &str, b: i32| {
        let mut out = Vec::new();
        encode_key(&mut out, Some(&Value::Text(a.into()))).unwrap();
        encode_key(&mut out, Some(&Value::I32(b))).unwrap();
        out
    };
    assert_ne!(comp("a", 1), comp("ab", 1));
    // ("a", then a text that could look like the start of the next column) must not equal ("a\0...", ...)
    let mut one = Vec::new();
    encode_key(&mut one, Some(&Value::Text("a".into()))).unwrap();
    encode_key(&mut one, Some(&Value::Text("b".into()))).unwrap();
    let mut two = Vec::new();
    encode_key(&mut two, Some(&Value::Text("ab".into()))).unwrap();
    encode_key(&mut two, Some(&Value::Text("".into()))).unwrap();
    assert_ne!(one, two);

    let mut r = Rng::new(5);
    for i in 0..iters() {
        let mk = |r: &mut Rng| {
            let n = r.next();
            (text(r), r.pick(&[i32::MIN, -1, 0, 1, i32::MAX, n as i32]))
        };
        let (a, b) = (mk(&mut r), mk(&mut r));
        let want = a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(&b.1));
        assert_eq!(
            comp(&a.0, a.1).cmp(&comp(&b.0, b.1)),
            want,
            "case {i}: {a:?} vs {b:?}"
        );
        // and a composite decodes column by column
        let k = comp(&a.0, a.1);
        let mut c: &[u8] = &k;
        assert_eq!(
            decode_key(&mut c, &Type::Text).unwrap(),
            Some(Value::Text(a.0.clone()))
        );
        assert_eq!(
            decode_key(&mut c, &Type::I32).unwrap(),
            Some(Value::I32(a.1))
        );
        assert!(c.is_empty());
    }
}

#[test]
fn only_orderable_types_can_be_indexed() {
    for v in [
        Value::Json(serde_json::json!({})),
        Value::Array(Type::I32, vec![]),
        Value::File(FileValue {
            object: [0; 16],
            version: [0; 16],
        }),
        Value::Number(serde_json::Number::from(1)),
    ] {
        assert!(encode_key(&mut Vec::new(), Some(&v)).is_err(), "{v:?}");
    }
    assert!(decode_key(&mut &[1u8, 0][..], &Type::Json).is_err());
}

#[test]
fn damaged_keys_are_refused_not_misread() {
    for (t, bytes) in [
        (Type::I32, vec![1u8, 0, 0]),
        (Type::Bool, vec![1, 2]),
        (Type::Text, vec![1, b'a']),
        (Type::Text, vec![1, b'a', 0, 7]),
        (Type::Decimal, vec![9]),
        (Type::I8, vec![5]),
        (Type::Decimal, vec![1, 3]),
    ] {
        let mut c: &[u8] = &bytes;
        assert!(decode_key(&mut c, &t).is_err(), "{t:?} {bytes:?}");
    }
}

#[test]
fn random_bytes_never_break_the_key_decoder_and_what_it_accepts_re_encodes_stably() {
    let mut r = Rng::new(6);
    let types = indexable_types();
    let mut accepted = 0;
    for _ in 0..iters() * 4 {
        // half pure noise, half a valid key with a few bytes damaged
        let mut bytes: Vec<u8> = if r.below(2) == 0 {
            (0..r.below(12)).map(|_| r.next() as u8).collect()
        } else {
            let t = types[r.below(types.len())].clone();
            let v = value(&mut r, &t);
            let mut k = Vec::new();
            encode_key(&mut k, Some(&v)).unwrap();
            k
        };
        for _ in 0..r.below(3) {
            if !bytes.is_empty() {
                let i = r.below(bytes.len());
                bytes[i] = r.next() as u8;
            }
        }
        for t in &types {
            let mut cursor: &[u8] = &bytes;
            if let Ok(v) = decode_key(&mut cursor, t) {
                accepted += 1;
                let mut out = Vec::new();
                encode_key(&mut out, v.as_ref()).unwrap();
                let mut again: &[u8] = &out;
                let back = decode_key(&mut again, t).unwrap();
                assert!(again.is_empty());
                let mut out2 = Vec::new();
                encode_key(&mut out2, back.as_ref()).unwrap();
                assert_eq!(
                    out, out2,
                    "{t:?}: {bytes:?} decoded to {v:?}, which does not re-encode stably"
                );
            }
        }
    }
    assert!(
        accepted > 100,
        "too few accepted keys ({accepted}) for the check to mean anything"
    );
}
