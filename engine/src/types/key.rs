//! Index and primary-key encoding: a value becomes bytes whose byte order is the value's order.
//!
//! A key is the concatenation of its columns' encodings, each starting with a presence byte (`0x00` null, `0x01` a value), so nulls sort first and
//! every encoding is self-delimiting: `("a", "b")` and `("ab", "")` cannot collide, and a composite key sorts column by column.
//!
//! * integers: fixed width, big-endian, the sign bit flipped for signed types;
//! * floats: the usual total-order transform of the bits; `-0.0` is written as `0.0` (they compare equal) and every NaN as one NaN, which sorts after
//!   everything else, including infinity;
//! * decimals: sign, then the decimal exponent and significant digits, so `1.5` and `1.50` are the same key; a negative number is the bitwise
//!   complement of its magnitude's encoding;
//! * text and binary: the bytes with `0x00` written as `0x00 0xFF`, ended by `0x00 0x00`;
//! * dates, times, timestamps: as the signed integers they are; uuids: their 16 bytes; enums: the member index.
//!
//! JSON, arrays, files and legacy numbers cannot be indexed.

use super::*;
use crate::error::{Error, Result};

fn not_indexable(t: &Type) -> Error {
    Error::Invalid(format!("a column of type {t:?} cannot be part of an index"))
}

fn bad(m: &str) -> Error {
    Error::Corrupt(format!("key: {m}"))
}

fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&((v as u64) ^ (1 << 63)).to_be_bytes());
}
fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&((v as u32) ^ (1 << 31)).to_be_bytes());
}
fn put_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&((v as u16) ^ (1 << 15)).to_be_bytes());
}
fn put_i8(out: &mut Vec<u8>, v: i8) {
    out.push((v as u8) ^ 0x80);
}

fn f64_bits(v: f64) -> u64 {
    let v = if v.is_nan() {
        f64::NAN
    } else if v == 0.0 {
        0.0
    } else {
        v
    };
    let b = if v.is_nan() {
        0x7FF8_0000_0000_0000
    } else {
        v.to_bits()
    };
    if b >> 63 == 1 { !b } else { b | (1 << 63) }
}
fn f32_bits(v: f32) -> u32 {
    let v = if v == 0.0 { 0.0 } else { v };
    let b = if v.is_nan() { 0x7FC0_0000 } else { v.to_bits() };
    if b >> 31 == 1 { !b } else { b | (1 << 31) }
}

fn put_escaped(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        out.push(b);
        if b == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

fn put_decimal(out: &mut Vec<u8>, d: &Decimal) {
    let (sign, exponent, digits) = d.canonical();
    if sign == 0 {
        out.push(0x01);
        return;
    }
    let mut body = Vec::with_capacity(5 + digits.len());
    body.extend_from_slice(&((exponent as u32) ^ (1 << 31)).to_be_bytes());
    body.extend(digits.iter().map(|d| d + 1));
    body.push(0);
    if sign < 0 {
        out.push(0x00);
        out.extend(body.iter().map(|b| !b));
    } else {
        out.push(0x02);
        out.extend(body);
    }
}

/// Appends the key encoding of one column (`None` is a null).
pub fn encode_key(out: &mut Vec<u8>, v: Option<&Value>) -> Result<()> {
    let Some(v) = v else {
        out.push(0);
        return Ok(());
    };
    let t = v.type_of();
    if !t.indexable() {
        return Err(not_indexable(&t));
    }
    out.push(1);
    match v {
        Value::Bool(b) => out.push(*b as u8),
        Value::I8(x) => put_i8(out, *x),
        Value::I16(x) => put_i16(out, *x),
        Value::I32(x) => put_i32(out, *x),
        Value::I64(x) => put_i64(out, *x),
        Value::U8(x) => out.push(*x),
        Value::U16(x) => out.extend_from_slice(&x.to_be_bytes()),
        Value::U32(x) => out.extend_from_slice(&x.to_be_bytes()),
        Value::U64(x) => out.extend_from_slice(&x.to_be_bytes()),
        Value::F32(x) => out.extend_from_slice(&f32_bits(*x).to_be_bytes()),
        Value::F64(x) => out.extend_from_slice(&f64_bits(*x).to_be_bytes()),
        Value::Decimal(d) => put_decimal(out, d),
        Value::Text(s) => put_escaped(out, s.as_bytes()),
        Value::Binary(b) => put_escaped(out, b),
        Value::Uuid(u) => out.extend_from_slice(u),
        Value::Date(d) => put_i32(out, *d),
        Value::Time(t) | Value::Timestamp(t) | Value::TimestampTz(t) => put_i64(out, *t),
        Value::Enum(i) => out.extend_from_slice(&i.to_be_bytes()),
        Value::Json(_) | Value::Array(..) | Value::File(_) | Value::Number(_) => {
            unreachable!("refused above")
        }
    }
    Ok(())
}

struct In<'a>(&'a [u8]);
impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad("it ends in the middle of a value"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
}

fn get_escaped(r: &mut In) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let b = r.byte()?;
        if b != 0 {
            out.push(b);
            continue;
        }
        match r.byte()? {
            0xFF => out.push(0),
            0 => return Ok(out),
            _ => return Err(bad("a bad escape")),
        }
    }
}

/// Reads one column's key encoding from the front of `input`, advancing it. `ty` says what to expect; a null reads as `None`.
pub fn decode_key(input: &mut &[u8], ty: &Type) -> Result<Option<Value>> {
    if !ty.indexable() {
        return Err(not_indexable(ty));
    }
    let mut r = In(input);
    let v = match r.byte()? {
        0 => None,
        1 => Some(match ty {
            Type::Bool => match r.byte()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(bad("a boolean")),
            },
            Type::I8 => Value::I8((r.byte()? ^ 0x80) as i8),
            Type::I16 => Value::I16(
                (u16::from_be_bytes(r.take(2)?.try_into().map_err(|_| bad("int"))?) ^ (1 << 15))
                    as i16,
            ),
            Type::I32 => Value::I32(
                (u32::from_be_bytes(r.take(4)?.try_into().map_err(|_| bad("int"))?) ^ (1 << 31))
                    as i32,
            ),
            Type::I64 => Value::I64(
                (u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| bad("int"))?) ^ (1 << 63))
                    as i64,
            ),
            Type::U8 => Value::U8(r.byte()?),
            Type::U16 => Value::U16(u16::from_be_bytes(
                r.take(2)?.try_into().map_err(|_| bad("int"))?,
            )),
            Type::U32 => Value::U32(u32::from_be_bytes(
                r.take(4)?.try_into().map_err(|_| bad("int"))?,
            )),
            Type::U64 => Value::U64(u64::from_be_bytes(
                r.take(8)?.try_into().map_err(|_| bad("int"))?,
            )),
            Type::F32 => {
                let b = u32::from_be_bytes(r.take(4)?.try_into().map_err(|_| bad("float"))?);
                Value::F32(f32::from_bits(if b >> 31 == 1 {
                    b & !(1 << 31)
                } else {
                    !b
                }))
            }
            Type::F64 => {
                let b = u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| bad("float"))?);
                Value::F64(f64::from_bits(if b >> 63 == 1 {
                    b & !(1 << 63)
                } else {
                    !b
                }))
            }
            Type::Decimal => {
                let sign = match r.byte()? {
                    0x00 => -1i8,
                    0x01 => 0,
                    0x02 => 1,
                    _ => return Err(bad("a decimal's sign")),
                };
                if sign == 0 {
                    Value::Decimal(Decimal {
                        unscaled: 0,
                        scale: 0,
                    })
                } else {
                    let flip = |b: u8| if sign < 0 { !b } else { b };
                    let exp = u32::from_be_bytes([
                        flip(r.byte()?),
                        flip(r.byte()?),
                        flip(r.byte()?),
                        flip(r.byte()?),
                    ]) ^ (1 << 31);
                    let mut digits = Vec::new();
                    loop {
                        let b = flip(r.byte()?);
                        if b == 0 {
                            break;
                        }
                        if b > 10 || digits.len() > decimal::MAX_DIGITS as usize {
                            return Err(bad("a decimal's digits"));
                        }
                        digits.push(b - 1);
                    }
                    Value::Decimal(Decimal::from_canonical(sign, exp as i32, &digits)?)
                }
            }
            Type::Text => Value::Text(
                String::from_utf8(get_escaped(&mut r)?)
                    .map_err(|_| bad("text that is not UTF-8"))?,
            ),
            Type::Binary => Value::Binary(get_escaped(&mut r)?),
            Type::Uuid => Value::Uuid(r.take(16)?.try_into().map_err(|_| bad("uuid"))?),
            Type::Date => Value::Date(
                (u32::from_be_bytes(r.take(4)?.try_into().map_err(|_| bad("date"))?) ^ (1 << 31))
                    as i32,
            ),
            Type::Time => Value::Time(
                (u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| bad("time"))?) ^ (1 << 63))
                    as i64,
            ),
            Type::Timestamp => Value::Timestamp(
                (u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| bad("timestamp"))?)
                    ^ (1 << 63)) as i64,
            ),
            Type::TimestampTz => Value::TimestampTz(
                (u64::from_be_bytes(r.take(8)?.try_into().map_err(|_| bad("timestamp"))?)
                    ^ (1 << 63)) as i64,
            ),
            Type::Enum => Value::Enum(u32::from_be_bytes(
                r.take(4)?.try_into().map_err(|_| bad("enum"))?,
            )),
            Type::Json | Type::Array(_) | Type::File | Type::Number => {
                unreachable!("refused above")
            }
        }),
        _ => return Err(bad("a presence byte")),
    };
    *input = r.0;
    Ok(v)
}
