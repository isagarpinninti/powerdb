//! The row format.
//!
//! ```text
//! row   := version:u8  count:varint  ( column_id:varint  type_tag:u8  value )*
//! ```
//!
//! Columns are named by a stable numeric id (the name lives in the catalog), so renaming a column, adding one and dropping one never touch a stored
//! row. A column that is null is simply absent. Each value carries its type tag, so a row can be read without its schema and a new type is a new tag.
//!
//! The format is strict: columns appear in ascending id order, varints are minimal, text is valid UTF-8, JSON is in its canonical form (sorted keys,
//! no spaces), booleans are 0 or 1. A row therefore has exactly one valid encoding, `encode(decode(x)) == x` for every `x` the decoder accepts, and
//! two equal rows are equal bytes. Every length is checked against the bytes that remain before anything is allocated, so a damaged row cannot ask
//! for gigabytes.

use super::*;
use crate::error::{Error, Result};

pub const ROW_VERSION: u8 = 1;
/// A row larger than this is refused on write and on read. Large content belongs in an object, not a column.
pub const MAX_ROW_BYTES: usize = 16 << 20;
pub const MAX_COLUMNS: usize = 4096;
/// How deeply a JSON value may nest (well inside what the parser accepts, so what is written can always be read back).
pub const MAX_JSON_DEPTH: usize = 64;

fn bad(m: &str) -> Error {
    Error::Corrupt(format!("row: {m}"))
}

fn invalid(m: String) -> Error {
    Error::Invalid(m)
}

// ---- varints ----

pub(crate) fn put_uvar(out: &mut Vec<u8>, mut v: u128) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn zig(v: i128) -> u128 {
    ((v << 1) ^ (v >> 127)) as u128
}

fn unzig(v: u128) -> i128 {
    ((v >> 1) as i128) ^ -((v & 1) as i128)
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(bad("it ends in the middle of a value"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    /// A minimal varint of at most `bits` bits.
    fn uvar(&mut self, bits: u32) -> Result<u128> {
        let mut v: u128 = 0;
        let mut shift = 0u32;
        let mut first = true;
        loop {
            let b = self.byte()?;
            let low = (b & 0x7f) as u128;
            if shift >= 128 || (shift > 0 && low != 0 && (low << shift) >> shift != low) {
                return Err(bad("a number is too large"));
            }
            v |= low << shift;
            if b & 0x80 == 0 {
                if b == 0 && !first {
                    return Err(bad("a number is not in its shortest form"));
                }
                break;
            }
            first = false;
            shift += 7;
        }
        if bits < 128 && v >> bits != 0 {
            return Err(bad("a number is out of range"));
        }
        Ok(v)
    }
    fn ivar(&mut self, bits: u32) -> Result<i128> {
        // a signed value of `bits` bits needs `bits` bits once zig-zagged
        let v = unzig(self.uvar(bits)?);
        if bits < 128 {
            let lim = 1i128 << (bits - 1);
            if v < -lim || v >= lim {
                return Err(bad("a number is out of range"));
            }
        }
        Ok(v)
    }
    pub(crate) fn finished(&self) -> bool {
        self.remaining() == 0
    }
}

// ---- values ----

fn json_depth(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        serde_json::Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn put_value(out: &mut Vec<u8>, v: &Value) -> Result<()> {
    match v {
        Value::Bool(b) => out.push(*b as u8),
        Value::I8(x) => put_uvar(out, zig(*x as i128)),
        Value::I16(x) => put_uvar(out, zig(*x as i128)),
        Value::I32(x) => put_uvar(out, zig(*x as i128)),
        Value::I64(x) => put_uvar(out, zig(*x as i128)),
        Value::U8(x) => put_uvar(out, *x as u128),
        Value::U16(x) => put_uvar(out, *x as u128),
        Value::U32(x) => put_uvar(out, *x as u128),
        Value::U64(x) => put_uvar(out, *x as u128),
        Value::F32(x) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        Value::F64(x) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        Value::Decimal(d) => {
            Decimal::new(d.unscaled, d.scale)?; // refuses an out-of-range decimal built by hand
            out.push(d.scale);
            put_uvar(out, zig(d.unscaled));
        }
        Value::Text(s) => {
            put_uvar(out, s.len() as u128);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Binary(b) => {
            put_uvar(out, b.len() as u128);
            out.extend_from_slice(b);
        }
        Value::Uuid(u) => out.extend_from_slice(u),
        Value::Date(d) => put_uvar(out, zig(*d as i128)),
        Value::Time(t) | Value::Timestamp(t) | Value::TimestampTz(t) => {
            put_uvar(out, zig(*t as i128))
        }
        Value::Json(j) => {
            if json_depth(j) > MAX_JSON_DEPTH {
                return Err(invalid(format!(
                    "JSON nested more than {MAX_JSON_DEPTH} levels deep"
                )));
            }
            let text = serde_json::to_vec(j)?; // sorted keys, no spaces: the canonical form
            put_uvar(out, text.len() as u128);
            out.extend_from_slice(&text);
        }
        Value::Enum(i) => put_uvar(out, *i as u128),
        Value::Array(elem, items) => {
            if matches!(elem, Type::Array(_)) {
                return Err(invalid("an array of arrays is not supported".into()));
            }
            out.push(elem.tag());
            put_uvar(out, items.len() as u128);
            for it in items {
                if it.type_of() != *elem {
                    return Err(invalid(
                        "every element of an array must have the array's element type".into(),
                    ));
                }
                put_value(out, it)?;
            }
        }
        Value::File(f) => {
            if f.mime.len() > MAX_FILE_MIME
                || f.name.as_ref().is_some_and(|n| n.len() > MAX_FILE_NAME)
            {
                return Err(invalid(format!(
                    "a file's type is at most {MAX_FILE_MIME} bytes and its name at most {MAX_FILE_NAME}"
                )));
            }
            out.extend_from_slice(&f.id);
            put_uvar(out, f.size as u128);
            put_uvar(out, f.mime.len() as u128);
            out.extend_from_slice(f.mime.as_bytes());
            match &f.name {
                None => out.push(0),
                Some(n) => {
                    out.push(1);
                    put_uvar(out, n.len() as u128);
                    out.extend_from_slice(n.as_bytes());
                }
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push(0);
                put_uvar(out, zig(i as i128));
            } else if let Some(u) = n.as_u64() {
                out.push(1);
                put_uvar(out, u as u128);
            } else {
                let f = n
                    .as_f64()
                    .ok_or_else(|| invalid("a number that is not a finite value".into()))?;
                out.push(2);
                out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
        }
    }
    Ok(())
}

fn get_value(r: &mut Reader, ty: &Type) -> Result<Value> {
    Ok(match ty {
        Type::Bool => match r.byte()? {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            _ => return Err(bad("a boolean is not 0 or 1")),
        },
        Type::I8 => Value::I8(r.ivar(8)? as i8),
        Type::I16 => Value::I16(r.ivar(16)? as i16),
        Type::I32 => Value::I32(r.ivar(32)? as i32),
        Type::I64 => Value::I64(r.ivar(64)? as i64),
        Type::U8 => Value::U8(r.uvar(8)? as u8),
        Type::U16 => Value::U16(r.uvar(16)? as u16),
        Type::U32 => Value::U32(r.uvar(32)? as u32),
        Type::U64 => Value::U64(r.uvar(64)? as u64),
        Type::F32 => Value::F32(f32::from_bits(u32::from_le_bytes(
            r.take(4)?.try_into().map_err(|_| bad("a float"))?,
        ))),
        Type::F64 => Value::F64(f64::from_bits(u64::from_le_bytes(
            r.take(8)?.try_into().map_err(|_| bad("a float"))?,
        ))),
        Type::Decimal => {
            let scale = r.byte()?;
            let unscaled = unzig(r.uvar(128)?);
            Value::Decimal(
                Decimal::new(unscaled, scale).map_err(|_| bad("a decimal is out of range"))?,
            )
        }
        Type::Text => {
            let n = r.uvar(32)? as usize;
            Value::Text(
                String::from_utf8(r.take(n)?.to_vec())
                    .map_err(|_| bad("text that is not valid UTF-8"))?,
            )
        }
        Type::Binary => {
            let n = r.uvar(32)? as usize;
            Value::Binary(r.take(n)?.to_vec())
        }
        Type::Uuid => Value::Uuid(r.take(16)?.try_into().map_err(|_| bad("a uuid"))?),
        Type::Date => Value::Date(r.ivar(32)? as i32),
        Type::Time => Value::Time(r.ivar(64)? as i64),
        Type::Timestamp => Value::Timestamp(r.ivar(64)? as i64),
        Type::TimestampTz => Value::TimestampTz(r.ivar(64)? as i64),
        Type::Json => {
            let n = r.uvar(32)? as usize;
            let text = r.take(n)?;
            let v: serde_json::Value =
                serde_json::from_slice(text).map_err(|_| bad("JSON that does not parse"))?;
            if json_depth(&v) > MAX_JSON_DEPTH || serde_json::to_vec(&v)? != text {
                return Err(bad("JSON that is not in its canonical form"));
            }
            Value::Json(v)
        }
        Type::Enum => Value::Enum(r.uvar(32)? as u32),
        Type::Array(_) => {
            let t = r.byte()?;
            if t == T_ARRAY {
                return Err(bad("an array of arrays"));
            }
            let elem = Type::from_tag(t).ok_or_else(|| bad("an unknown element type"))?;
            let n = r.uvar(32)? as usize;
            // each element takes at least one byte, so the count cannot honestly exceed what is left
            if n > r.remaining() {
                return Err(bad("an array longer than its row"));
            }
            let mut items = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                items.push(get_value(r, &elem)?);
            }
            Value::Array(elem, items)
        }
        Type::File => {
            let id: [u8; 32] = r
                .take(32)?
                .try_into()
                .map_err(|_| bad("a file reference"))?;
            let size = r.uvar(64)? as u64;
            let n = r.uvar(32)? as usize;
            if n > MAX_FILE_MIME {
                return Err(bad("a file type that is too long"));
            }
            let mime = String::from_utf8(r.take(n)?.to_vec())
                .map_err(|_| bad("a file type that is not valid UTF-8"))?;
            let name = match r.byte()? {
                0 => None,
                1 => {
                    let n = r.uvar(32)? as usize;
                    if n > MAX_FILE_NAME {
                        return Err(bad("a file name that is too long"));
                    }
                    Some(
                        String::from_utf8(r.take(n)?.to_vec())
                            .map_err(|_| bad("a file name that is not valid UTF-8"))?,
                    )
                }
                _ => return Err(bad("a file name flag")),
            };
            Value::File(FileValue {
                id,
                size,
                mime,
                name,
            })
        }
        Type::Number => match r.byte()? {
            0 => Value::Number(serde_json::Number::from(r.ivar(64)? as i64)),
            1 => {
                let u = r.uvar(64)? as u64;
                if u <= i64::MAX as u64 {
                    return Err(bad("a number not in its canonical form"));
                }
                Value::Number(serde_json::Number::from(u))
            }
            2 => {
                let f = f64::from_bits(u64::from_le_bytes(
                    r.take(8)?.try_into().map_err(|_| bad("a float"))?,
                ));
                Value::Number(
                    serde_json::Number::from_f64(f)
                        .ok_or_else(|| bad("a number that is not finite"))?,
                )
            }
            _ => return Err(bad("an unknown number kind")),
        },
    })
}

/// Writes a row. `columns` are `(column id, value)`; null columns are left out. Ids may come in any order but must not repeat.
pub fn encode_row(columns: &[(u32, Value)]) -> Result<Vec<u8>> {
    if columns.len() > MAX_COLUMNS {
        return Err(invalid(format!("a row has at most {MAX_COLUMNS} columns")));
    }
    let mut sorted: Vec<&(u32, Value)> = columns.iter().collect();
    sorted.sort_by_key(|c| c.0);
    if sorted.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(invalid("a column appears twice in a row".into()));
    }
    let mut out = vec![ROW_VERSION];
    put_uvar(&mut out, sorted.len() as u128);
    for (id, v) in sorted {
        put_uvar(&mut out, *id as u128);
        out.push(v.type_of().tag());
        put_value(&mut out, v)?;
        if out.len() > MAX_ROW_BYTES {
            return Err(invalid(format!(
                "a row is at most {MAX_ROW_BYTES} bytes; store large content as an object"
            )));
        }
    }
    Ok(out)
}

/// Reads a row written by `encode_row`, refusing anything that is not exactly one valid encoding.
pub fn decode_row(bytes: &[u8]) -> Result<Vec<(u32, Value)>> {
    if bytes.len() > MAX_ROW_BYTES {
        return Err(bad("it is larger than a row may be"));
    }
    let mut r = Reader::new(bytes);
    let version = r.byte()?;
    if version != ROW_VERSION {
        return Err(bad(&format!(
            "version {version} is not understood (this build reads version {ROW_VERSION}); run a newer PowerDB"
        )));
    }
    let n = r.uvar(32)? as usize;
    if n > MAX_COLUMNS || n > r.remaining() {
        return Err(bad("more columns than it can hold"));
    }
    let mut out: Vec<(u32, Value)> = Vec::with_capacity(n);
    let mut last: Option<u32> = None;
    for _ in 0..n {
        let id = r.uvar(32)? as u32;
        if last.is_some_and(|l| id <= l) {
            return Err(bad("columns are not in ascending order"));
        }
        last = Some(id);
        let tag = r.byte()?;
        let ty = if tag == T_ARRAY {
            Type::Array(Box::new(Type::Bool)) // the element type follows inside the value
        } else {
            Type::from_tag(tag).ok_or_else(|| {
                bad(&format!(
                    "type {tag} is not known to this build; run a newer PowerDB"
                ))
            })?
        };
        out.push((id, get_value(&mut r, &ty)?));
    }
    if !r.finished() {
        return Err(bad("there are bytes after the last column"));
    }
    Ok(out)
}
