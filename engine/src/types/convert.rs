//! Conversions around values: dates and times as text, time-ordered UUIDs, and the JSON view of a row that `CHECK` constraints read.

use super::Value;
use serde_json::{Map, Number, Value as J};

/// Days since 1970-01-01 as `YYYY-MM-DD` (proleptic Gregorian, so it works for dates far on either side of the epoch).
pub fn format_date(days: i32) -> String {
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Microseconds since midnight as `HH:MM:SS.ffffff`.
pub fn format_time(micros: i64) -> String {
    let us = micros.rem_euclid(86_400_000_000);
    format!(
        "{:02}:{:02}:{:02}.{:06}",
        us / 3_600_000_000,
        us / 60_000_000 % 60,
        us / 1_000_000 % 60,
        us % 1_000_000
    )
}

/// Microseconds since the epoch as `YYYY-MM-DDTHH:MM:SS.ffffff`, with a trailing `Z` for an instant.
pub fn format_timestamp(micros: i64, utc: bool) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{}{}",
        format_time(micros.rem_euclid(86_400_000_000)),
        if utc { "Z" } else { "" }
    )
}

/// Howard Hinnant's algorithm: days since the epoch to year, month, day.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `8-4-4-4-12` hexadecimal.
pub fn format_uuid(u: &[u8; 16]) -> String {
    let h: String = u.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// A version 7 UUID (RFC 9562): the first 48 bits are the time in milliseconds, so UUIDs made later sort later, and the rest is random.
pub fn uuid_v7() -> [u8; 16] {
    let mut u = [0u8; 16];
    getrandom::fill(&mut u).expect("the operating system's random number generator failed");
    let ms = (crate::util::now_ms() as u64) & 0xFFFF_FFFF_FFFF;
    u[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    u[6] = (u[6] & 0x0F) | 0x70;
    u[8] = (u[8] & 0x3F) | 0x80;
    u
}

/// The view of a value that a `CHECK` expression sees. Integers, floats, text and booleans are themselves; a decimal is a floating-point number (so
/// comparisons on it are approximate); a date, time or timestamp is its ISO text, which compares in the right order; a uuid is its text; JSON and
/// arrays are themselves; binary is not visible (null).
pub fn json_view(v: &Value) -> J {
    match v {
        Value::Bool(b) => J::Bool(*b),
        Value::I8(x) => J::from(*x),
        Value::I16(x) => J::from(*x),
        Value::I32(x) => J::from(*x),
        Value::I64(x) => J::from(*x),
        Value::U8(x) => J::from(*x),
        Value::U16(x) => J::from(*x),
        Value::U32(x) => J::from(*x),
        Value::U64(x) => J::from(*x),
        Value::F32(x) => Number::from_f64(*x as f64)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::F64(x) => Number::from_f64(*x).map(J::Number).unwrap_or(J::Null),
        Value::Decimal(d) => d
            .to_string()
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Text(s) => J::String(s.clone()),
        Value::Uuid(u) => J::String(format_uuid(u)),
        Value::Date(d) => J::String(format_date(*d)),
        Value::Time(t) => J::String(format_time(*t)),
        Value::Timestamp(t) => J::String(format_timestamp(*t, false)),
        Value::TimestampTz(t) => J::String(format_timestamp(*t, true)),
        Value::Json(j) => j.clone(),
        Value::Binary(_) => J::Null,
        // a file shows its labels, so a CHECK can say `doc != null && doc.size < 5000000`
        Value::File(f) => {
            let mut m = Map::new();
            m.insert("size".into(), J::from(f.size));
            m.insert("mime".into(), J::String(f.mime.clone()));
            if let Some(n) = &f.name {
                m.insert("name".into(), J::String(n.clone()));
            }
            J::Object(m)
        }
        Value::Enum(i) => J::from(*i),
        Value::Array(_, items) => J::Array(items.iter().map(json_view).collect()),
        Value::Number(n) => J::Number(n.clone()),
    }
}

/// A row as a JSON object keyed by column name, for evaluating a `CHECK`.
pub fn row_view(row: &[(String, Option<Value>)]) -> Map<String, J> {
    row.iter()
        .map(|(n, v)| (n.clone(), v.as_ref().map(json_view).unwrap_or(J::Null)))
        .collect()
}
