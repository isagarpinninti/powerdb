//! The type system for typed tables (see docs/TYPES_AND_TABLES.md): the values a column can hold, how a row is written to bytes, and how a value
//! becomes an index key that sorts the way the value does.
//!
//! This module is a library. It does not touch storage: nothing here changes what is on disk today. The row format is permanent once data is
//! written in it, so it is deliberately plain, strict (a row has exactly one valid encoding) and covered by property tests.

pub mod decimal;
pub mod key;
pub mod row;
pub mod schema;

pub use decimal::Decimal;

/// A file column's value: which object, and which version of it. (Resolving this to bytes goes through the object catalog.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileValue {
    pub object: [u8; 16],
    pub version: [u8; 16],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Decimal(Decimal),
    Text(String),
    Uuid([u8; 16]),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since midnight.
    Time(i64),
    /// Microseconds since the epoch, as a wall-clock value with no zone.
    Timestamp(i64),
    /// Microseconds since the epoch: an instant.
    TimestampTz(i64),
    Json(serde_json::Value),
    Binary(Vec<u8>),
    /// The index of the member in the column's enum.
    Enum(u32),
    /// A list of values of one type (which is not itself an array). The element type is kept so an empty list still has one.
    Array(Type, Vec<Value>),
    File(FileValue),
    /// What a `number` field of an existing collection holds: a JSON number, kept exactly as written.
    Number(serde_json::Number),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Decimal,
    Text,
    Uuid,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Json,
    Binary,
    Enum,
    Array(Box<Type>),
    File,
    Number,
}

// The tag stored with each value in a row. Numbers are never reused or renumbered: a stored row names its types by these. Values from 32 up are
// kept free for types not built yet (vectors, embeddings, full text), so adding one never disturbs a tag that exists.
pub(crate) const T_BOOL: u8 = 1;
pub(crate) const T_I8: u8 = 2;
pub(crate) const T_I16: u8 = 3;
pub(crate) const T_I32: u8 = 4;
pub(crate) const T_I64: u8 = 5;
pub(crate) const T_U8: u8 = 6;
pub(crate) const T_U16: u8 = 7;
pub(crate) const T_U32: u8 = 8;
pub(crate) const T_U64: u8 = 9;
pub(crate) const T_F32: u8 = 10;
pub(crate) const T_F64: u8 = 11;
pub(crate) const T_DECIMAL: u8 = 12;
pub(crate) const T_TEXT: u8 = 13;
pub(crate) const T_UUID: u8 = 14;
pub(crate) const T_DATE: u8 = 15;
pub(crate) const T_TIME: u8 = 16;
pub(crate) const T_TIMESTAMP: u8 = 17;
pub(crate) const T_TIMESTAMPTZ: u8 = 18;
pub(crate) const T_JSON: u8 = 19;
pub(crate) const T_BINARY: u8 = 20;
pub(crate) const T_ENUM: u8 = 21;
pub(crate) const T_ARRAY: u8 = 22;
pub(crate) const T_FILE: u8 = 23;
pub(crate) const T_NUMBER: u8 = 24;

impl Type {
    pub(crate) fn tag(&self) -> u8 {
        match self {
            Type::Bool => T_BOOL,
            Type::I8 => T_I8,
            Type::I16 => T_I16,
            Type::I32 => T_I32,
            Type::I64 => T_I64,
            Type::U8 => T_U8,
            Type::U16 => T_U16,
            Type::U32 => T_U32,
            Type::U64 => T_U64,
            Type::F32 => T_F32,
            Type::F64 => T_F64,
            Type::Decimal => T_DECIMAL,
            Type::Text => T_TEXT,
            Type::Uuid => T_UUID,
            Type::Date => T_DATE,
            Type::Time => T_TIME,
            Type::Timestamp => T_TIMESTAMP,
            Type::TimestampTz => T_TIMESTAMPTZ,
            Type::Json => T_JSON,
            Type::Binary => T_BINARY,
            Type::Enum => T_ENUM,
            Type::Array(_) => T_ARRAY,
            Type::File => T_FILE,
            Type::Number => T_NUMBER,
        }
    }

    /// The type for a tag that stands alone (everything but an array, whose element type follows it).
    pub(crate) fn from_tag(t: u8) -> Option<Type> {
        Some(match t {
            T_BOOL => Type::Bool,
            T_I8 => Type::I8,
            T_I16 => Type::I16,
            T_I32 => Type::I32,
            T_I64 => Type::I64,
            T_U8 => Type::U8,
            T_U16 => Type::U16,
            T_U32 => Type::U32,
            T_U64 => Type::U64,
            T_F32 => Type::F32,
            T_F64 => Type::F64,
            T_DECIMAL => Type::Decimal,
            T_TEXT => Type::Text,
            T_UUID => Type::Uuid,
            T_DATE => Type::Date,
            T_TIME => Type::Time,
            T_TIMESTAMP => Type::Timestamp,
            T_TIMESTAMPTZ => Type::TimestampTz,
            T_JSON => Type::Json,
            T_BINARY => Type::Binary,
            T_ENUM => Type::Enum,
            T_FILE => Type::File,
            T_NUMBER => Type::Number,
            _ => return None,
        })
    }

    /// Whether a value of this type can be part of an index key.
    pub fn indexable(&self) -> bool {
        !matches!(
            self,
            Type::Json | Type::Array(_) | Type::File | Type::Number
        )
    }
}

impl Value {
    pub fn type_of(&self) -> Type {
        match self {
            Value::Bool(_) => Type::Bool,
            Value::I8(_) => Type::I8,
            Value::I16(_) => Type::I16,
            Value::I32(_) => Type::I32,
            Value::I64(_) => Type::I64,
            Value::U8(_) => Type::U8,
            Value::U16(_) => Type::U16,
            Value::U32(_) => Type::U32,
            Value::U64(_) => Type::U64,
            Value::F32(_) => Type::F32,
            Value::F64(_) => Type::F64,
            Value::Decimal(_) => Type::Decimal,
            Value::Text(_) => Type::Text,
            Value::Uuid(_) => Type::Uuid,
            Value::Date(_) => Type::Date,
            Value::Time(_) => Type::Time,
            Value::Timestamp(_) => Type::Timestamp,
            Value::TimestampTz(_) => Type::TimestampTz,
            Value::Json(_) => Type::Json,
            Value::Binary(_) => Type::Binary,
            Value::Enum(_) => Type::Enum,
            Value::Array(t, _) => Type::Array(Box::new(t.clone())),
            Value::File(_) => Type::File,
            Value::Number(_) => Type::Number,
        }
    }

    /// Equality that compares floats by their bits, so a NaN equals itself and `0.0` differs from `-0.0`. What a stored value must round-trip to.
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::F32(a), Value::F32(b)) => a.to_bits() == b.to_bits(),
            (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
            (Value::Array(ta, a), Value::Array(tb, b)) => {
                ta == tb && a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (Value::Number(a), Value::Number(b)) => a.to_string() == b.to_string(),
            (a, b) => a == b,
        }
    }
}
