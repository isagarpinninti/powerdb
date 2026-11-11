//! Tables, columns and schema changes (see docs/TYPES_AND_TABLES.md, decision 4).
//!
//! A table is a list of columns, each with a stable numeric id that is never reused, and a schema version that goes up on every change. A stored
//! row names its columns by id, so the cheap changes touch no row:
//!
//! * adding a column (a row that lacks it reads as the column's default, or null);
//! * renaming a column (the name lives only here);
//! * dropping a column (its values in old rows are ignored, and go the next time the row is rewritten);
//! * widening a column's type (`INT32` to `INT64`): the reader accepts the narrower stored value and widens it.
//!
//! The changes that need to look at existing data say so (`Effect::NeedsScan`) and the caller does the scan before committing the change. A change
//! that would make stored values mean something else (narrowing or changing a type) is refused unless the table is empty.
//!
//! This is a library: nothing here reads or writes storage.

use super::{Type, Value};
use crate::error::{Error, Result};

pub const MAX_NAME: usize = 63;

pub fn valid_column_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_NAME
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    /// Never reused: a dropped column's id stays retired.
    pub id: u32,
    pub name: String,
    pub ty: Type,
    pub nullable: bool,
    /// What a row that predates the column (or omits it) reads as. `None` reads as null, which `nullable` must then allow.
    pub default: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableDef {
    /// Names the table in storage: never reused, so a dropped table's rows can never be mistaken for a new table's. 0 until the table is stored.
    pub id: u32,
    pub name: String,
    /// Goes up by one on every schema change.
    pub version: u32,
    pub columns: Vec<ColumnDef>,
    /// The id the next added column gets.
    pub next_column_id: u32,
    /// The ids of the columns that identify a row, in key order. Fixed once the table is stored: changing it would change every row's key.
    pub primary_key: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    AddColumn {
        name: String,
        ty: Type,
        nullable: bool,
        default: Option<Value>,
    },
    DropColumn {
        name: String,
    },
    RenameColumn {
        from: String,
        to: String,
    },
    /// To a wider type of the same kind (`INT8` < `INT16` < `INT32` < `INT64`, the same for unsigned, `FLOAT32` < `FLOAT64`), or to the same type.
    WidenColumn {
        name: String,
        to: Type,
    },
    SetNullable {
        name: String,
    },
    SetNotNull {
        name: String,
    },
    /// Any other type change: only while the table has no rows.
    ChangeType {
        name: String,
        to: Type,
    },
}

/// What applying a change costs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Only the catalog changes; no stored row is touched.
    CatalogOnly,
    /// Existing rows must be checked first (and the change refused if one fails): `reason` says what for.
    NeedsScan(String),
}

fn widens(from: &Type, to: &Type) -> bool {
    use Type::*;
    if from == to {
        return true;
    }
    let rank = |t: &Type| match t {
        I8 => Some((0, 0)),
        I16 => Some((0, 1)),
        I32 => Some((0, 2)),
        I64 => Some((0, 3)),
        U8 => Some((1, 0)),
        U16 => Some((1, 1)),
        U32 => Some((1, 2)),
        U64 => Some((1, 3)),
        F32 => Some((2, 0)),
        F64 => Some((2, 1)),
        _ => None,
    };
    match (rank(from), rank(to)) {
        (Some((fk, fr)), Some((tk, tr))) => fk == tk && fr <= tr,
        _ => false,
    }
}

/// Converts a stored value to a wider type. The only conversions are the widenings `widens` allows, so none of them can fail or lose anything.
pub fn widen_value(v: Value, to: &Type) -> Result<Value> {
    use Value::*;
    if v.type_of() == *to {
        return Ok(v);
    }
    let out = match (&v, to) {
        (I8(x), Type::I16) => I16(*x as i16),
        (I8(x), Type::I32) => I32(*x as i32),
        (I8(x), Type::I64) => I64(*x as i64),
        (I16(x), Type::I32) => I32(*x as i32),
        (I16(x), Type::I64) => I64(*x as i64),
        (I32(x), Type::I64) => I64(*x as i64),
        (U8(x), Type::U16) => U16(*x as u16),
        (U8(x), Type::U32) => U32(*x as u32),
        (U8(x), Type::U64) => U64(*x as u64),
        (U16(x), Type::U32) => U32(*x as u32),
        (U16(x), Type::U64) => U64(*x as u64),
        (U32(x), Type::U64) => U64(*x as u64),
        (F32(x), Type::F64) => F64(*x as f64),
        _ => {
            return Err(Error::Corrupt(format!(
                "a stored {:?} cannot be read as {to:?}",
                v.type_of()
            )));
        }
    };
    Ok(out)
}

impl TableDef {
    pub fn new(name: &str) -> Result<Self> {
        if !valid_column_name(name) {
            return Err(Error::Invalid(format!(
                "table name \"{name}\" must be lowercase letters, digits and underscores, starting with a letter (at most {MAX_NAME})"
            )));
        }
        Ok(Self {
            id: 0,
            name: name.to_string(),
            version: 1,
            columns: vec![],
            next_column_id: 1,
            primary_key: vec![],
        })
    }

    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_by_id(&self, id: u32) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.id == id)
    }

    /// A primary-key column cannot be dropped, made nullable or have its type changed: a row's key is built from it, so every key would change.
    fn guard_key(&self, i: usize, what: &str) -> Result<()> {
        if self.primary_key.contains(&self.columns[i].id) {
            return Err(Error::Invalid(format!(
                "\"{}\" is part of the primary key and cannot be {what}",
                self.columns[i].name
            )));
        }
        Ok(())
    }

    /// Sets the primary key (by column name). Every column must exist, be NOT NULL and be of a type that can be indexed.
    pub fn set_primary_key(&mut self, names: &[&str]) -> Result<()> {
        if names.is_empty() {
            return Err(Error::Invalid("a table needs a primary key".into()));
        }
        let mut ids = Vec::new();
        for n in names {
            let c = self.column(n).ok_or_else(|| {
                Error::Invalid(format!(
                    "the primary key names a column \"{n}\" that the table does not have"
                ))
            })?;
            if c.nullable {
                return Err(Error::Invalid(format!(
                    "primary key column \"{n}\" must be NOT NULL"
                )));
            }
            if !c.ty.indexable() {
                return Err(Error::Invalid(format!(
                    "primary key column \"{n}\" has type {}, which cannot be indexed",
                    c.ty.name()
                )));
            }
            if ids.contains(&c.id) {
                return Err(Error::Invalid(format!(
                    "primary key column \"{n}\" is listed twice"
                )));
            }
            ids.push(c.id);
        }
        self.primary_key = ids;
        Ok(())
    }

    /// Checks that the table can be stored: a primary key, and only column types that tables support today.
    pub fn validate_for_storage(&self) -> Result<()> {
        if self.primary_key.is_empty() {
            return Err(Error::Invalid("a table needs a primary key".into()));
        }
        for c in &self.columns {
            let ok = match &c.ty {
                Type::Enum | Type::File | Type::Number => false,
                Type::Array(t) => !matches!(**t, Type::Enum | Type::File | Type::Number),
                _ => true,
            };
            if !ok {
                return Err(Error::Invalid(format!(
                    "column \"{}\": type {} is not available in tables yet",
                    c.name,
                    c.ty.name()
                )));
            }
        }
        Ok(())
    }

    /// The key bytes (after the table id) of a row given as `(column id, value)` pairs.
    pub fn key_for_row(&self, row: &[(u32, Value)]) -> Result<Vec<u8>> {
        let mut key = Vec::new();
        for id in &self.primary_key {
            let v = row
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, v)| v)
                .ok_or_else(|| {
                    Error::Corrupt("a row has no value for a primary key column".into())
                })?;
            super::key::encode_key(&mut key, Some(v))?;
        }
        Ok(key)
    }

    /// The key bytes for primary-key values given in key order. Each must be of its column's type.
    pub fn key_for_values(&self, pk: &[Value]) -> Result<Vec<u8>> {
        if pk.len() != self.primary_key.len() {
            return Err(Error::Invalid(format!(
                "the primary key of \"{}\" has {} column(s); {} given",
                self.name,
                self.primary_key.len(),
                pk.len()
            )));
        }
        let mut key = Vec::new();
        for (id, v) in self.primary_key.iter().zip(pk) {
            let c = self
                .column_by_id(*id)
                .ok_or_else(|| Error::Corrupt("a primary key column is missing".into()))?;
            if v.type_of() != c.ty {
                return Err(Error::Invalid(format!(
                    "primary key column \"{}\" holds {}, not {}",
                    c.name,
                    c.ty.name(),
                    v.type_of().name()
                )));
            }
            super::key::encode_key(&mut key, Some(v))?;
        }
        Ok(key)
    }

    /// The primary-key values of a stored row, in key order.
    pub fn key_values(&self, row: &[(u32, Value)]) -> Result<Vec<Value>> {
        self.primary_key
            .iter()
            .map(|id| {
                row.iter()
                    .find(|(i, _)| i == id)
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| {
                        Error::Corrupt("a row has no value for a primary key column".into())
                    })
            })
            .collect()
    }

    fn find(&self, name: &str) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| Error::NotFound(format!("column \"{name}\"")))
    }

    /// The table after `change`, and what that cost. `table_is_empty` says whether any row exists (the caller knows; this library does not).
    /// The receiver is untouched, so a refused change leaves nothing half done.
    pub fn apply(&self, change: &Change, table_is_empty: bool) -> Result<(TableDef, Effect)> {
        let mut t = self.clone();
        let mut effect = Effect::CatalogOnly;
        match change {
            Change::AddColumn {
                name,
                ty,
                nullable,
                default,
            } => {
                if !valid_column_name(name) {
                    return Err(Error::Invalid(format!(
                        "column name \"{name}\" must be lowercase letters, digits and underscores, starting with a letter (at most {MAX_NAME})"
                    )));
                }
                if t.column(name).is_some() {
                    return Err(Error::Conflict(format!("column \"{name}\" already exists")));
                }
                if let Some(d) = default {
                    if d.type_of() != *ty {
                        return Err(Error::Invalid(format!(
                            "the default of \"{name}\" must be a {ty:?}"
                        )));
                    }
                } else if !nullable && !table_is_empty {
                    return Err(Error::Invalid(format!(
                        "column \"{name}\" is NOT NULL, so it needs a default: the existing rows have no value for it"
                    )));
                }
                t.columns.push(ColumnDef {
                    id: t.next_column_id,
                    name: name.clone(),
                    ty: ty.clone(),
                    nullable: *nullable,
                    default: default.clone(),
                });
                t.next_column_id += 1;
            }
            Change::DropColumn { name } => {
                let i = t.find(name)?;
                t.guard_key(i, "dropped")?;
                t.columns.remove(i); // its id is not given out again: next_column_id only goes up
            }
            Change::RenameColumn { from, to } => {
                if !valid_column_name(to) {
                    return Err(Error::Invalid(format!(
                        "column name \"{to}\" must be lowercase letters, digits and underscores, starting with a letter (at most {MAX_NAME})"
                    )));
                }
                if from != to && t.column(to).is_some() {
                    return Err(Error::Conflict(format!("column \"{to}\" already exists")));
                }
                let i = t.find(from)?;
                t.columns[i].name = to.clone();
            }
            Change::WidenColumn { name, to } => {
                let i = t.find(name)?;
                t.guard_key(i, "widened")?;
                if !widens(&t.columns[i].ty, to) {
                    return Err(Error::Invalid(format!(
                        "{:?} cannot be widened to {to:?}; only to a larger type of the same kind (use ChangeType on an empty table, or migrate the data yourself)",
                        t.columns[i].ty
                    )));
                }
                if let Some(d) = t.columns[i].default.take() {
                    t.columns[i].default = Some(widen_value(d, to)?);
                }
                t.columns[i].ty = to.clone();
            }
            Change::SetNullable { name } => {
                let i = t.find(name)?;
                t.guard_key(i, "made nullable")?;
                t.columns[i].nullable = true;
            }
            Change::SetNotNull { name } => {
                let i = t.find(name)?;
                if t.columns[i].nullable && !table_is_empty {
                    effect =
                        Effect::NeedsScan(format!("every row must have a value for \"{name}\""));
                }
                t.columns[i].nullable = false;
            }
            Change::ChangeType { name, to } => {
                let i = t.find(name)?;
                t.guard_key(i, "given another type")?;
                if t.columns[i].ty != *to && !widens(&t.columns[i].ty, to) && !table_is_empty {
                    return Err(Error::Invalid(format!(
                        "changing \"{name}\" from {:?} to {to:?} would change what the stored values mean; it is only possible while the table has no rows",
                        t.columns[i].ty
                    )));
                }
                if let Some(d) = &t.columns[i].default
                    && d.type_of() != *to
                {
                    t.columns[i].default = widen_value(d.clone(), to).ok();
                }
                t.columns[i].ty = to.clone();
            }
        }
        t.version += 1;
        Ok((t, effect))
    }

    /// Reads a stored row (`(column id, value)` pairs, as `decode_row` returns them) under this schema: one entry per current column, in column
    /// order. A value for a column that has since been dropped is ignored; a missing one is the column's default (or null); a value stored under a
    /// narrower type is widened. A value whose type is not the column's and cannot be widened to it is damage, and an error.
    pub fn project(&self, row: &[(u32, Value)]) -> Result<Vec<(String, Option<Value>)>> {
        let mut out = Vec::with_capacity(self.columns.len());
        for c in &self.columns {
            let stored = row
                .iter()
                .find(|(id, _)| *id == c.id)
                .map(|(_, v)| v.clone());
            let v = match stored {
                Some(v) => Some(widen_value(v, &c.ty)?),
                None => c.default.clone(),
            };
            if v.is_none() && !c.nullable {
                return Err(Error::Corrupt(format!(
                    "a row has no value for the NOT NULL column \"{}\"",
                    c.name
                )));
            }
            out.push((c.name.clone(), v));
        }
        Ok(out)
    }

    /// The row to store for these named values: column ids for the names, a type check on each value, `NOT NULL` and defaults applied.
    pub fn build_row(&self, values: &[(&str, Value)]) -> Result<Vec<(u32, Value)>> {
        for (name, _) in values {
            if self.column(name).is_none() {
                return Err(Error::Invalid(format!(
                    "the table \"{}\" has no column \"{name}\"",
                    self.name
                )));
            }
        }
        let mut out = Vec::new();
        for c in &self.columns {
            let given = values
                .iter()
                .find(|(n, _)| *n == c.name)
                .map(|(_, v)| v.clone());
            match given.or_else(|| c.default.clone()) {
                Some(v) => {
                    if v.type_of() != c.ty {
                        return Err(Error::Invalid(format!(
                            "column \"{}\" holds {:?}, not {:?}",
                            c.name,
                            c.ty,
                            v.type_of()
                        )));
                    }
                    out.push((c.id, v));
                }
                None if c.nullable => {}
                None => {
                    return Err(Error::Invalid(format!(
                        "column \"{}\" is NOT NULL and has no default",
                        c.name
                    )));
                }
            }
        }
        Ok(out)
    }
}

// ---- the stored form ----

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn corrupt(m: &str) -> Error {
    Error::Corrupt(format!("a table definition: {m}"))
}

impl TableDef {
    /// The catalog entry as stored: JSON, with each default written through the row codec so it is exact.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut cols = Vec::new();
        for c in &self.columns {
            let default = match &c.default {
                Some(v) => serde_json::Value::String(hex_encode(&super::row::encode_row(&[(
                    0,
                    v.clone(),
                )])?)),
                None => serde_json::Value::Null,
            };
            cols.push(serde_json::json!({
                "id": c.id, "name": c.name, "type": c.ty.name(), "nullable": c.nullable, "default": default,
            }));
        }
        Ok(serde_json::to_vec(&serde_json::json!({
            "format": 1, "id": self.id, "name": self.name, "version": self.version, "next_column_id": self.next_column_id,
            "primary_key": self.primary_key, "columns": cols,
        }))?)
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let v: serde_json::Value =
            serde_json::from_slice(b).map_err(|_| corrupt("it does not parse"))?;
        if v["format"] != 1 {
            return Err(corrupt(
                "a format this build does not know; run a newer PowerDB",
            ));
        }
        let num = |k: &str| -> Result<u32> {
            v[k].as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| corrupt(k))
        };
        let mut columns = Vec::new();
        for c in v["columns"].as_array().ok_or_else(|| corrupt("columns"))? {
            let ty = c["type"]
                .as_str()
                .and_then(Type::parse_column_type)
                .ok_or_else(|| corrupt("a column type"))?;
            let default = match c["default"].as_str() {
                None => None,
                Some(h) => {
                    let row = super::row::decode_row(
                        &hex_decode(h).ok_or_else(|| corrupt("a default"))?,
                    )?;
                    match row.as_slice() {
                        [(0, val)] => Some(val.clone()),
                        _ => return Err(corrupt("a default")),
                    }
                }
            };
            columns.push(ColumnDef {
                id: c["id"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| corrupt("a column id"))?,
                name: c["name"]
                    .as_str()
                    .ok_or_else(|| corrupt("a column name"))?
                    .to_string(),
                ty,
                nullable: c["nullable"].as_bool().ok_or_else(|| corrupt("nullable"))?,
                default,
            });
        }
        let primary_key = v["primary_key"]
            .as_array()
            .ok_or_else(|| corrupt("primary_key"))?
            .iter()
            .map(|x| {
                x.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| corrupt("a key column"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            id: num("id")?,
            name: v["name"]
                .as_str()
                .ok_or_else(|| corrupt("name"))?
                .to_string(),
            version: num("version")?,
            columns,
            next_column_id: num("next_column_id")?,
            primary_key,
        })
    }
}
