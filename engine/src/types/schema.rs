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
    pub name: String,
    /// Goes up by one on every schema change.
    pub version: u32,
    pub columns: Vec<ColumnDef>,
    /// The id the next added column gets.
    pub next_column_id: u32,
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
            name: name.to_string(),
            version: 1,
            columns: vec![],
            next_column_id: 1,
        })
    }

    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_by_id(&self, id: u32) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.id == id)
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
