//! The JSON form of every value: what the HTTP API sends and accepts.
//!
//! | type | JSON |
//! |---|---|
//! | `bool` | `true` / `false` |
//! | `int8` `int16` `int32` `uint8` `uint16` `uint32` | a number (or a string of digits) |
//! | `int64` `uint64` | a **string** of digits on output (a JavaScript number is exact only to 2^53); a number or a string on input |
//! | `float32` `float64` | a number; `"NaN"`, `"Infinity"` and `"-Infinity"` as strings (JSON has no such numbers) |
//! | `decimal` | a **string** (`"12.50"`): the scale is kept; a number is accepted on input if it has no exponent |
//! | `text` | a string |
//! | `uuid` | `8-4-4-4-12` hexadecimal; 32 hexadecimal digits are accepted on input |
//! | `date` | `"2026-11-12"` |
//! | `time` | `"13:45:07.250000"` (the fraction is optional on input, up to six digits) |
//! | `timestamp` | `"2026-11-12T13:45:07.250000"`, no zone |
//! | `timestamptz` | `"2026-11-12T13:45:07.250000Z"` on output; on input a `Z` or an offset (`+02:00`) is required |
//! | `json` | any JSON |
//! | `binary` | standard base64 |
//! | `array<T>` | an array of `T` |
//! | `file` | on output `{"id", "size", "mime", "name"}`; never accepted as JSON (files are uploaded or attached) |

use super::convert::{format_date, format_time, format_timestamp, format_uuid};
use super::{Decimal, FileValue, Type, Value};
use crate::error::{Error, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value as J;

fn bad(what: &str, got: &J) -> Error {
    let shown = got.to_string();
    let shown = if shown.len() > 60 {
        format!("{}...", &shown[..60])
    } else {
        shown
    };
    Error::Invalid(format!("{what} (got {shown})"))
}

/// A value as the API sends it.
pub fn to_json(v: &Value) -> J {
    match v {
        Value::Bool(b) => J::Bool(*b),
        Value::I8(x) => J::from(*x),
        Value::I16(x) => J::from(*x),
        Value::I32(x) => J::from(*x),
        Value::I64(x) => J::String(x.to_string()),
        Value::U8(x) => J::from(*x),
        Value::U16(x) => J::from(*x),
        Value::U32(x) => J::from(*x),
        Value::U64(x) => J::String(x.to_string()),
        Value::F32(x) => float(*x as f64),
        Value::F64(x) => float(*x),
        Value::Decimal(d) => J::String(d.to_string()),
        Value::Text(s) => J::String(s.clone()),
        Value::Uuid(u) => J::String(format_uuid(u)),
        Value::Date(d) => J::String(format_date(*d)),
        Value::Time(t) => J::String(format_time(*t)),
        Value::Timestamp(t) => J::String(format_timestamp(*t, false)),
        Value::TimestampTz(t) => J::String(format_timestamp(*t, true)),
        Value::Json(j) => j.clone(),
        Value::Binary(b) => J::String(STANDARD.encode(b)),
        Value::Enum(i) => J::from(*i),
        Value::Array(_, items) => J::Array(items.iter().map(to_json).collect()),
        Value::File(f) => file_json(f),
        Value::Number(n) => J::Number(n.clone()),
    }
}

fn float(x: f64) -> J {
    if x.is_nan() {
        J::String("NaN".into())
    } else if x.is_infinite() {
        J::String(if x > 0.0 { "Infinity" } else { "-Infinity" }.into())
    } else {
        serde_json::Number::from_f64(x)
            .map(J::Number)
            .unwrap_or(J::Null)
    }
}

pub fn file_json(f: &FileValue) -> J {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), J::String(crate::util::hex(&f.id)));
    m.insert("size".into(), J::from(f.size));
    m.insert("mime".into(), J::String(f.mime.clone()));
    if let Some(n) = &f.name {
        m.insert("name".into(), J::String(n.clone()));
    }
    J::Object(m)
}

// ---- parsing ----

fn int(j: &J, ty: &str, lo: i128, hi: i128) -> Result<i128> {
    let n: i128 = match j {
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                i as i128
            } else if let Some(u) = n.as_u64() {
                u as i128
            } else {
                return Err(bad(&format!("expected a whole number for {ty}"), j));
            }
        }
        J::String(s) => {
            let digits = s.strip_prefix('-').unwrap_or(s);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || s.len() > 40 {
                return Err(bad(&format!("expected digits for {ty}"), j));
            }
            s.parse::<i128>()
                .map_err(|_| bad(&format!("a number out of range for {ty}"), j))?
        }
        _ => return Err(bad(&format!("expected a number for {ty}"), j)),
    };
    if n < lo || n > hi {
        return Err(bad(
            &format!("a number out of range for {ty} ({lo} to {hi})"),
            j,
        ));
    }
    Ok(n)
}

fn parse_f64(j: &J, ty: &str) -> Result<f64> {
    match j {
        J::Number(n) => n
            .as_f64()
            .ok_or_else(|| bad(&format!("expected a number for {ty}"), j)),
        J::String(s) => match s.as_str() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            _ => Err(bad(
                &format!("expected a number, or \"NaN\", \"Infinity\" or \"-Infinity\", for {ty}"),
                j,
            )),
        },
        _ => Err(bad(&format!("expected a number for {ty}"), j)),
    }
}

/// `[-]digits[.digits]` with no exponent and no sign other than a leading minus.
pub fn parse_decimal(s: &str) -> Option<Decimal> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int_part, frac) = match body.split_once('.') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || (body.contains('.') && frac.is_empty())
    {
        return None;
    }
    let digits = format!("{int_part}{frac}");
    let trimmed = digits.trim_start_matches('0');
    if trimmed.len() > super::decimal::MAX_DIGITS as usize
        || frac.len() > super::decimal::MAX_SCALE as usize
    {
        return None;
    }
    let mut unscaled: i128 = if trimmed.is_empty() {
        0
    } else {
        trimmed.parse().ok()?
    };
    if neg {
        unscaled = -unscaled;
    }
    Decimal::new(unscaled, frac.len() as u8).ok()
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Howard Hinnant's algorithm: year, month, day to days since the epoch.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn parse_date(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (num(0..4)?, num(5..7)? as u32, num(8..10)? as u32);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    i32::try_from(days_from_civil(y, m, d)).ok()
}

pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 8 || b[2] != b':' || b[5] != b':' {
        return None;
    }
    let two = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if t.len() == 2 && t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (h, m, sec) = (two(0..2)?, two(3..5)?, two(6..8)?);
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let mut micros = 0i64;
    if b.len() > 8 {
        if b[8] != b'.' || b.len() > 15 || b.len() == 9 {
            return None;
        }
        let frac = s.get(9..)?;
        if !frac.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        micros = format!("{frac:0<6}").parse().ok()?;
    }
    Some(((h * 60 + m) * 60 + sec) * 1_000_000 + micros)
}

/// `YYYY-MM-DDTHH:MM:SS[.ffffff]` (a space may replace the `T`), then nothing, `Z`, or `+HH:MM` / `-HH:MM`. Returns microseconds since the epoch (in UTC if a
/// zone was given) and whether a zone was present.
pub fn parse_timestamp(s: &str) -> Option<(i64, bool)> {
    let date = parse_date(s.get(..10)?)?;
    let rest = s.get(10..)?;
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    // the zone, if any, starts at the first 'Z', '+' or '-' after the time
    let zone_at = rest.find(['Z', '+', '-']);
    let (time_part, zone) = match zone_at {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let t = parse_time(time_part)?;
    let mut micros = date as i64 * 86_400_000_000 + t;
    let has_zone = match zone {
        None => false,
        Some("Z") => true,
        Some(z) => {
            let sign = match z.as_bytes().first()? {
                b'+' => 1i64,
                b'-' => -1,
                _ => return None,
            };
            let off = z.get(1..)?;
            let (hh, mm) = off.split_once(':')?;
            if hh.len() != 2
                || mm.len() != 2
                || !hh.bytes().all(|c| c.is_ascii_digit())
                || !mm.bytes().all(|c| c.is_ascii_digit())
            {
                return None;
            }
            let (hh, mm): (i64, i64) = (hh.parse().ok()?, mm.parse().ok()?);
            if hh > 23 || mm > 59 {
                return None;
            }
            micros -= sign * (hh * 60 + mm) * 60 * 1_000_000;
            true
        }
    };
    Some((micros, has_zone))
}

fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let hex: String = if s.len() == 36 {
        let b = s.as_bytes();
        if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
            return None;
        }
        s.chars().filter(|c| *c != '-').collect()
    } else {
        s.to_string()
    };
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A JSON value as a value of type `ty`, strictly: what does not fit is refused, never adjusted.
pub fn from_json(ty: &Type, j: &J) -> Result<Value> {
    let s = || -> Result<&str> {
        j.as_str()
            .ok_or_else(|| bad(&format!("expected a string for {}", ty.name()), j))
    };
    Ok(match ty {
        Type::Bool => Value::Bool(
            j.as_bool()
                .ok_or_else(|| bad("expected true or false", j))?,
        ),
        Type::I8 => Value::I8(int(j, "int8", i8::MIN as i128, i8::MAX as i128)? as i8),
        Type::I16 => Value::I16(int(j, "int16", i16::MIN as i128, i16::MAX as i128)? as i16),
        Type::I32 => Value::I32(int(j, "int32", i32::MIN as i128, i32::MAX as i128)? as i32),
        Type::I64 => Value::I64(int(j, "int64", i64::MIN as i128, i64::MAX as i128)? as i64),
        Type::U8 => Value::U8(int(j, "uint8", 0, u8::MAX as i128)? as u8),
        Type::U16 => Value::U16(int(j, "uint16", 0, u16::MAX as i128)? as u16),
        Type::U32 => Value::U32(int(j, "uint32", 0, u32::MAX as i128)? as u32),
        Type::U64 => Value::U64(int(j, "uint64", 0, u64::MAX as i128)? as u64),
        Type::F64 => Value::F64(parse_f64(j, "float64")?),
        Type::F32 => {
            let f = parse_f64(j, "float32")?;
            let g = f as f32;
            if f.is_finite() && g.is_infinite() {
                return Err(bad("a number too large for float32", j));
            }
            Value::F32(g)
        }
        Type::Decimal => {
            let text = match j {
                J::String(s) => s.clone(),
                J::Number(n) => n.to_string(),
                _ => return Err(bad("expected a string like \"12.50\" for decimal", j)),
            };
            Value::Decimal(parse_decimal(&text).ok_or_else(|| bad("expected a decimal like \"12.50\" (digits and one point, at most 38 digits, no exponent)", j))?)
        }
        Type::Text => Value::Text(s()?.to_string()),
        Type::Uuid => Value::Uuid(parse_uuid(s()?).ok_or_else(|| bad("expected a uuid", j))?),
        Type::Date => Value::Date(
            parse_date(s()?).ok_or_else(|| bad("expected a date like \"2026-11-12\"", j))?,
        ),
        Type::Time => Value::Time(parse_time(s()?).ok_or_else(|| {
            bad(
                "expected a time like \"13:45:07\" or \"13:45:07.250000\"",
                j,
            )
        })?),
        Type::Timestamp => match parse_timestamp(s()?) {
            Some((t, false)) => Value::Timestamp(t),
            Some((_, true)) => {
                return Err(bad(
                    "a timestamp has no zone; use timestamptz for an instant, or leave the zone off",
                    j,
                ));
            }
            None => return Err(bad("expected a timestamp like \"2026-11-12T13:45:07\"", j)),
        },
        Type::TimestampTz => match parse_timestamp(s()?) {
            Some((t, true)) => Value::TimestampTz(t),
            Some((_, false)) => {
                return Err(bad(
                    "a timestamptz needs a zone: end it with Z or an offset such as +02:00",
                    j,
                ));
            }
            None => return Err(bad("expected a timestamp like \"2026-11-12T13:45:07Z\"", j)),
        },
        Type::Json => Value::Json(j.clone()),
        Type::Binary => Value::Binary(
            STANDARD
                .decode(s()?)
                .map_err(|_| bad("expected standard base64", j))?,
        ),
        Type::Array(t) => {
            let items = j.as_array().ok_or_else(|| bad("expected an array", j))?;
            Value::Array(
                (**t).clone(),
                items
                    .iter()
                    .map(|i| from_json(t, i))
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        Type::File => {
            return Err(Error::Invalid(
                "a file is set by uploading it or attaching an object, not by a JSON value".into(),
            ));
        }
        Type::Enum | Type::Number => {
            return Err(Error::Invalid(format!(
                "{} columns are not available in tables yet",
                ty.name()
            )));
        }
    })
}
