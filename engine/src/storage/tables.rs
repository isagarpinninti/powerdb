//! Typed tables in storage (see docs/TYPES_AND_TABLES.md).
//!
//! * `tables`: table name -> the table's definition (`TableDef::to_bytes`). Two reserved kinds of key start with a byte below `a`, so no table name can
//!   collide: `[0]` holds the next table id, and `[1] + table id + column id` holds a sequence's last value.
//! * `rows`: `table id (4 bytes, big-endian)` + the primary key's key encoding -> the row (`types::row`).
//! * `indexes`: one entry per row per unique constraint and foreign key: `table id` + `index id` + the indexed values' key encoding (+ the row's primary
//!   key for a foreign key, which many rows can share) -> the row's primary key. A row with a null in an indexed column has no entry.
//!
//! Tables live beside collections: collections and their JSON records are untouched. A row names its columns by id, so the schema changes that touch only
//! the catalog (add, drop, rename, widen) leave stored rows as they are; reading a row applies the current schema to it (`TableDef::project`).
//!
//! Every write is one atomic batch under the commit lock: the row, its index entries and any sequence counter go in together, so the failure points and
//! the crash tests cover them. The constraints are enforced inside that lock, before the batch is written:
//!
//! * NOT NULL, types and defaults (and generated values) by building the row;
//! * CHECK by evaluating the expression against a JSON view of the row;
//! * UNIQUE by looking up the unique index entry the row would add;
//! * FOREIGN KEY by looking up the referenced row; and, when a row is deleted, by looking up (in the foreign key's index) the rows that refer to it and
//!   restricting, cascading or setting null.
//!
//! Index entries are always computed from the row as the current schema reads it (defaults included), so the entries made while a constraint was added
//! to a table that already had rows are the same as the ones recomputed on a later update.

use super::{Db, PendingFile};
use crate::error::{Error, Result};
use crate::model::Record;
use crate::security::expr::{self, Ctx};
use crate::types::convert::{row_view, uuid_v7};
use crate::types::key::encode_key;
use crate::types::row::{decode_row, encode_row};
use crate::types::schema::{
    Change, ColumnDef, Constraint, ConstraintKind, Effect, Generator, OnDelete, TableDef,
};
use crate::types::{FileValue, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// A row as a caller sees it: the table's columns in order, a null as `None`.
pub type Row = Vec<(String, Option<Value>)>;

/// One page of rows found through an index, and where to continue.
#[derive(Debug, Clone)]
pub struct IndexPage {
    pub rows: Vec<Row>,
    /// Pass back as `after` for the next page; `None` when this page was not full.
    pub next: Option<Vec<u8>>,
}

/// A row's values by column id, defaults included: what the current schema reads it as.
type Eff = Vec<(u32, Value)>;

const COUNTER_KEY: [u8; 1] = [0];
const DROP_BATCH: usize = 2000;
pub const MAX_SCAN: usize = 10_000;
/// A delete that would cascade to more rows than this is refused.
pub const MAX_CASCADE: usize = 10_000;

fn row_key(table_id: u32, key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(4 + key.len());
    k.extend_from_slice(&table_id.to_be_bytes());
    k.extend_from_slice(key);
    k
}

/// The files a value holds: itself if it is a file, each element if it is a list of files.
fn file_values(v: &Value) -> Vec<&FileValue> {
    match v {
        Value::File(f) => vec![f],
        Value::Array(_, items) => items
            .iter()
            .filter_map(|i| {
                if let Value::File(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    }
}

/// The ids of every file stored in these columns, one entry per reference (a file named twice counts twice). This counts what is physically in the row,
/// which is what the stored reference counts must equal: a column dropped from the schema keeps its value until the row is rewritten.
pub(crate) fn file_ids(cols: &[(u32, Value)]) -> Vec<[u8; 32]> {
    cols.iter()
        .flat_map(|(_, v)| file_values(v).into_iter().map(|f| f.id))
        .collect()
}

fn seq_key(table_id: u32, column_id: u32) -> Vec<u8> {
    let mut k = vec![1u8];
    k.extend_from_slice(&table_id.to_be_bytes());
    k.extend_from_slice(&column_id.to_be_bytes());
    k
}

fn index_prefix(table_id: u32, index_id: u32) -> Vec<u8> {
    let mut k = Vec::with_capacity(8);
    k.extend_from_slice(&table_id.to_be_bytes());
    k.extend_from_slice(&index_id.to_be_bytes());
    k
}

/// The row as the current schema reads it, as `(column id, value)` for every column that has a value (defaults included).
pub(crate) fn effective(def: &TableDef, raw: &[(u32, Value)]) -> Result<Vec<(u32, Value)>> {
    let row = def.project(raw)?;
    Ok(def
        .columns
        .iter()
        .zip(row)
        .filter_map(|(c, (_, v))| v.map(|v| (c.id, v)))
        .collect())
}

fn value_of(eff: &[(u32, Value)], col: u32) -> Option<&Value> {
    eff.iter().find(|(i, _)| *i == col).map(|(_, v)| v)
}

/// The key encoding of these columns of a row, or `None` if any is null.
pub(crate) fn columns_key(eff: &[(u32, Value)], columns: &[u32]) -> Result<Option<Vec<u8>>> {
    let mut key = Vec::new();
    for c in columns {
        match value_of(eff, *c) {
            Some(v) => encode_key(&mut key, Some(v))?,
            None => return Ok(None),
        }
    }
    Ok(Some(key))
}

struct Entry {
    key: Vec<u8>,
    value: Vec<u8>,
    unique: bool,
    constraint: String,
}

/// The index entries a row has: one per unique constraint and foreign key whose columns are all non-null.
pub(crate) fn index_entries(
    def: &TableDef,
    eff: &[(u32, Value)],
    pk: &[u8],
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    Ok(entries(def, eff, pk)?
        .into_iter()
        .map(|e| (e.key, e.value))
        .collect())
}

fn entries(def: &TableDef, eff: &[(u32, Value)], pk: &[u8]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for k in &def.constraints {
        let (columns, index, unique) = match &k.kind {
            ConstraintKind::Unique { columns, index } => (columns, *index, true),
            ConstraintKind::ForeignKey { columns, index, .. } => (columns, *index, false),
            ConstraintKind::Check { .. } => continue,
        };
        let Some(cols) = columns_key(eff, columns)? else {
            continue;
        };
        let mut key = index_prefix(def.id, index);
        key.extend_from_slice(&cols);
        if !unique {
            key.extend_from_slice(pk);
        }
        out.push(Entry {
            key,
            value: pk.to_vec(),
            unique,
            constraint: k.name.clone(),
        });
    }
    // indexes made on request: never unique (a unique one is a constraint), one entry per row whose columns are all non-null
    for x in &def.indexes {
        let Some(cols) = columns_key(eff, &x.columns)? else {
            continue;
        };
        let mut key = index_prefix(def.id, x.index);
        key.extend_from_slice(&cols);
        key.extend_from_slice(pk);
        out.push(Entry {
            key,
            value: pk.to_vec(),
            unique: false,
            constraint: x.name.clone(),
        });
    }
    Ok(out)
}

/// The foreign keys, in any table, that refer to the table with id `parent`.
fn referencing(defs: &HashMap<u32, TableDef>, parent: u32) -> Vec<(&TableDef, &Constraint)> {
    let mut v = Vec::new();
    for d in defs.values() {
        for k in &d.constraints {
            if matches!(&k.kind, ConstraintKind::ForeignKey { ref_table_id, .. } if *ref_table_id == parent)
            {
                v.push((d, k));
            }
        }
    }
    v
}

fn describe(def: &TableDef, eff: &[(u32, Value)], columns: &[u32]) -> String {
    columns
        .iter()
        .map(|c| {
            let name = def.column_by_id(*c).map(|c| c.name.as_str()).unwrap_or("?");
            format!(
                "{name} = {}",
                value_of(eff, *c).map(show).unwrap_or_else(|| "null".into())
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn show(v: &Value) -> String {
    match crate::types::convert::json_view(v) {
        serde_json::Value::Null => "(not shown)".into(),
        j => j.to_string(),
    }
}

fn seq_value(ty: &crate::types::Type, n: u64) -> Result<Value> {
    use crate::types::Type::*;
    let over =
        || Error::Invalid("a sequence ran past the largest value its column can hold".into());
    Ok(match ty {
        I8 => Value::I8(i8::try_from(n).map_err(|_| over())?),
        I16 => Value::I16(i16::try_from(n).map_err(|_| over())?),
        I32 => Value::I32(i32::try_from(n).map_err(|_| over())?),
        I64 => Value::I64(i64::try_from(n).map_err(|_| over())?),
        U8 => Value::U8(u8::try_from(n).map_err(|_| over())?),
        U16 => Value::U16(u16::try_from(n).map_err(|_| over())?),
        U32 => Value::U32(u32::try_from(n).map_err(|_| over())?),
        U64 => Value::U64(n),
        _ => return Err(Error::Invalid("a sequence needs an integer column".into())),
    })
}

impl Db {
    // ---- the catalog ----

    pub fn table(&self, name: &str) -> Result<TableDef> {
        match self.inner.ks.tables.get(name.as_bytes())? {
            Some(v) => TableDef::from_bytes(&v),
            None => Err(Error::NotFound(format!("table \"{name}\""))),
        }
    }

    pub fn tables(&self) -> Result<Vec<TableDef>> {
        let mut out = Vec::new();
        for g in self.inner.ks.tables.iter() {
            let (k, v) = g.into_inner()?;
            if k.first().is_none_or(|b| *b < b'a') {
                continue; // the table id counter and the sequences
            }
            out.push(TableDef::from_bytes(&v)?);
        }
        Ok(out)
    }

    /// Stores a new table. The definition needs a primary key and only the column types tables support; it gets a table id here.
    pub fn create_table(&self, mut def: TableDef) -> Result<TableDef> {
        def.validate_for_storage()?;
        if !def.constraints.is_empty() {
            return Err(Error::Invalid(
                "create the table first, then add constraints to it".into(),
            ));
        }
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        if ks.tables.contains_key(def.name.as_bytes())? {
            return Err(Error::Conflict(format!(
                "table \"{}\" already exists",
                def.name
            )));
        }
        let next = match ks.tables.get(COUNTER_KEY)? {
            Some(v) => u32::from_be_bytes(
                v.as_ref()
                    .try_into()
                    .map_err(|_| Error::Corrupt("the table id counter".into()))?,
            ),
            None => 1,
        };
        def.id = next;
        def.version = 1;
        let mut batch = self.inner.store.batch();
        batch.insert(
            &ks.tables,
            COUNTER_KEY.to_vec(),
            (next + 1).to_be_bytes().to_vec(),
        );
        batch.insert(&ks.tables, def.name.as_bytes().to_vec(), def.to_bytes()?);
        self.finish_batch(batch)?;
        Ok(def)
    }

    fn has_rows(&self, table_id: u32) -> Result<bool> {
        Ok(self
            .inner
            .ks
            .rows
            .prefix(table_id.to_be_bytes())
            .next()
            .is_some())
    }

    /// For a foreign key, the other table's id and primary-key types come from the catalog, whatever the caller put there.
    fn resolve_change(&self, table: &TableDef, change: &Change) -> Result<Change> {
        let Change::AddForeignKey {
            name,
            columns,
            ref_table,
            on_delete,
            ..
        } = change
        else {
            return Ok(change.clone());
        };
        let parent = if *ref_table == table.name {
            table.clone()
        } else {
            self.table(ref_table)
                .map_err(|_| Error::Invalid(format!("the table \"{ref_table}\" does not exist")))?
        };
        let mut types = Vec::new();
        for id in &parent.primary_key {
            types.push(
                parent
                    .column_by_id(*id)
                    .ok_or_else(|| Error::Corrupt("a primary key column is missing".into()))?
                    .ty
                    .clone(),
            );
        }
        Ok(Change::AddForeignKey {
            name: name.clone(),
            columns: columns.clone(),
            ref_table: ref_table.clone(),
            ref_table_id: parent.id,
            ref_pk_types: types,
            on_delete: *on_delete,
        })
    }

    /// Applies a schema change. A change that must look at the existing rows first (a new NOT NULL, unique constraint, check or foreign key) does so under
    /// the commit lock, so no row can be written between the check and the change; the index entries a new constraint needs are written in the same
    /// batch as the catalog entry. A change that needs no scan touches only the catalog entry.
    pub fn alter_table(&self, name: &str, change: &Change) -> Result<TableDef> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.table(name)?;
        let change = self.resolve_change(&old, change)?;
        let (new, effect) = old.apply(&change, !self.has_rows(old.id)?)?;
        let mut batch = self.inner.store.batch();
        // Dropping a file column releases its files: the value would otherwise sit in every row, keeping the file alive, with no column to remove it from.
        if let Change::DropColumn { name: cname } = &change
            && let Some(col) = old.column(cname)
            && col.ty.is_file_column()
        {
            let cid = col.id;
            let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
            for g in ks.rows.prefix(old.id.to_be_bytes()) {
                let (k, v) = g.into_inner()?;
                let mut raw = decode_row(&v)?;
                let held: Vec<[u8; 32]> = file_ids(
                    &raw.iter()
                        .filter(|(i, _)| *i == cid)
                        .cloned()
                        .collect::<Vec<_>>(),
                );
                if !held.is_empty() || raw.iter().any(|(i, _)| *i == cid) {
                    raw.retain(|(i, _)| *i != cid);
                    batch.insert(&ks.rows, k.to_vec(), encode_row(&raw)?);
                    for id in held {
                        *delta.entry(id).or_default() -= 1;
                    }
                }
            }
            self.apply_file_deltas(&mut batch, &delta, &[])?;
        }
        if let Effect::NeedsScan(why) = effect {
            let added = new.constraints.last();
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            for g in ks.rows.prefix(old.id.to_be_bytes()) {
                let (k, v) = g.into_inner()?;
                let pk = &k[4..];
                let eff = effective(&new, &decode_row(&v)?).map_err(|e| {
                    Error::Invalid(format!("the change cannot be made: {why} ({e})"))
                })?;
                match (&change, added) {
                    (Change::AddIndex { name: iname, .. }, _) => {
                        for e in entries(&new, &eff, pk)?
                            .into_iter()
                            .filter(|e| e.constraint == *iname)
                        {
                            batch.insert(&ks.indexes, e.key, e.value);
                        }
                    }
                    (Change::AddCheck { .. }, Some(c)) => {
                        if let ConstraintKind::Check { expr } = &c.kind {
                            self.check_one(&new, &c.name, expr, &eff).map_err(|e| {
                                Error::Invalid(format!("the change cannot be made: {why} ({e})"))
                            })?;
                        }
                    }
                    (Change::AddUnique { .. }, Some(c)) => {
                        for e in entries(&new, &eff, pk)?
                            .into_iter()
                            .filter(|e| e.constraint == c.name)
                        {
                            if !seen.insert(e.key.clone()) {
                                return Err(Error::Conflict(format!(
                                    "the change cannot be made: {why}; the rows already repeat values"
                                )));
                            }
                            batch.insert(&ks.indexes, e.key, e.value);
                        }
                    }
                    (Change::AddForeignKey { .. }, Some(c)) => {
                        self.check_fks(&new, &eff, Some(c.name.as_str()))
                            .map_err(|e| {
                                Error::Conflict(format!("the change cannot be made: {why} ({e})"))
                            })?;
                        for e in entries(&new, &eff, pk)?
                            .into_iter()
                            .filter(|e| e.constraint == c.name)
                        {
                            batch.insert(&ks.indexes, e.key, e.value);
                        }
                    }
                    _ => {} // NOT NULL: building the row above was the check
                }
            }
        }
        if let Change::DropConstraint { name: cname } = &change
            && let Some(k) = old.constraints.iter().find(|k| k.name == *cname)
        {
            let index = match &k.kind {
                ConstraintKind::Unique { index, .. } | ConstraintKind::ForeignKey { index, .. } => {
                    Some(*index)
                }
                ConstraintKind::Check { .. } => None,
            };
            if let Some(index) = index {
                for g in ks.indexes.prefix(index_prefix(old.id, index)) {
                    batch.remove(&ks.indexes, g.key()?.to_vec());
                }
            }
        }
        if let Change::DropIndex { name: iname } = &change
            && let Some(x) = old.indexes.iter().find(|x| x.name == *iname)
        {
            for g in ks.indexes.prefix(index_prefix(old.id, x.index)) {
                batch.remove(&ks.indexes, g.key()?.to_vec());
            }
        }
        batch.insert(&ks.tables, name.as_bytes().to_vec(), new.to_bytes()?);
        self.finish_batch(batch)?;
        Ok(new)
    }

    /// Removes a table, its rows and its index entries. Refused while another table has a foreign key to it. The rows go in batches, then the catalog entry,
    /// so a crash part-way leaves the table (with fewer rows) and the call can simply be repeated. Writers wait while it runs. Returns the rows removed.
    pub fn drop_table(&self, name: &str) -> Result<u64> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(name)?;
        for other in self.tables()? {
            if other.id != def.id
                && let Some(k) = other.constraints.iter().find(|k| matches!(&k.kind, ConstraintKind::ForeignKey { ref_table_id, .. } if *ref_table_id == def.id))
            {
                return Err(Error::Conflict(format!("table \"{}\" refers to \"{name}\" through the foreign key \"{}\"; drop that constraint first", other.name, k.name)));
            }
        }
        let mut removed = 0u64;
        for (space, prefix) in [
            (&ks.rows, def.id.to_be_bytes().to_vec()),
            (&ks.indexes, def.id.to_be_bytes().to_vec()),
        ] {
            loop {
                let is_rows = std::ptr::eq(space, &ks.rows);
                let mut keys = Vec::new();
                let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
                for g in space.prefix(&prefix).take(DROP_BATCH) {
                    let (k, v) = g.into_inner()?;
                    if is_rows {
                        // every file a row holds is released with it
                        for id in file_ids(&decode_row(&v)?) {
                            *delta.entry(id).or_default() -= 1;
                        }
                    }
                    keys.push(k.to_vec());
                }
                if keys.is_empty() {
                    break;
                }
                let mut batch = self.inner.store.batch();
                for k in &keys {
                    batch.remove(space, k.clone());
                }
                if is_rows {
                    removed += keys.len() as u64;
                    self.apply_file_deltas(&mut batch, &delta, &[])?;
                }
                self.finish_batch(batch)?;
            }
        }
        let mut batch = self.inner.store.batch();
        let mut seq_prefix = vec![1u8];
        seq_prefix.extend_from_slice(&def.id.to_be_bytes());
        for g in ks.tables.prefix(&seq_prefix) {
            batch.remove(&ks.tables, g.key()?.to_vec());
        }
        batch.remove(&ks.tables, name.as_bytes().to_vec());
        self.finish_batch(batch)?;
        Ok(removed)
    }

    // ---- constraint checks ----

    pub(crate) fn check_one(
        &self,
        def: &TableDef,
        name: &str,
        expression: &str,
        eff: &[(u32, Value)],
    ) -> Result<()> {
        let row: Row = def
            .columns
            .iter()
            .map(|c| (c.name.clone(), value_of(eff, c.id).cloned()))
            .collect();
        let rec = Record {
            id: String::new(),
            collection: def.name.clone(),
            created_at: 0,
            updated_at: 0,
            fields: row_view(&row),
            files: Default::default(),
        };
        let e = expr::parse(expression)?;
        if !e.matches(&Ctx {
            record: Some(&rec),
            ..Ctx::default()
        }) {
            return Err(Error::Invalid(format!(
                "the row violates the check \"{name}\" ({expression})"
            )));
        }
        Ok(())
    }

    pub(crate) fn check_constraints(&self, def: &TableDef, eff: &[(u32, Value)]) -> Result<()> {
        for k in &def.constraints {
            if let ConstraintKind::Check { expr } = &k.kind {
                self.check_one(def, &k.name, expr, eff)?;
            }
        }
        Ok(())
    }

    /// Every foreign key of the row (or just `only`) refers to a row that exists. A null in any of its columns means it refers to nothing.
    pub(crate) fn check_fks(
        &self,
        def: &TableDef,
        eff: &[(u32, Value)],
        only: Option<&str>,
    ) -> Result<()> {
        for k in &def.constraints {
            let ConstraintKind::ForeignKey {
                columns,
                ref_table_id,
                ref_table,
                ..
            } = &k.kind
            else {
                continue;
            };
            if only.is_some_and(|n| n != k.name) {
                continue;
            }
            let Some(key) = columns_key(eff, columns)? else {
                continue;
            };
            if !self
                .inner
                .ks
                .rows
                .contains_key(row_key(*ref_table_id, &key))?
            {
                return Err(Error::Conflict(format!(
                    "the foreign key \"{}\" fails: there is no row in \"{ref_table}\" with {}",
                    k.name,
                    describe(def, eff, columns)
                )));
            }
        }
        Ok(())
    }

    /// Writes the row and the difference between its old and new index entries into `batch`, refusing a unique value that another row holds.
    fn write_row(
        &self,
        batch: &mut fjall::OwnedWriteBatch,
        def: &TableDef,
        old: Option<&[(u32, Value)]>,
        new: &[(u32, Value)],
        raw_new: &[(u32, Value)],
        pk: &[u8],
    ) -> Result<()> {
        let ks = &self.inner.ks;
        let old_entries = match old {
            Some(o) => entries(def, o, pk)?,
            None => vec![],
        };
        let new_entries = entries(def, new, pk)?;
        let new_keys: HashSet<&Vec<u8>> = new_entries.iter().map(|e| &e.key).collect();
        let old_keys: HashSet<&Vec<u8>> = old_entries.iter().map(|e| &e.key).collect();
        for e in &old_entries {
            if !new_keys.contains(&e.key) {
                batch.remove(&ks.indexes, e.key.clone());
            }
        }
        for e in &new_entries {
            if old_keys.contains(&e.key) {
                continue;
            }
            if e.unique
                && let Some(existing) = ks.indexes.get(&e.key)?
                && existing.as_ref() != pk
            {
                let cols = match def
                    .constraints
                    .iter()
                    .find(|k| k.name == e.constraint)
                    .map(|k| &k.kind)
                {
                    Some(ConstraintKind::Unique { columns, .. }) => describe(def, new, columns),
                    _ => String::new(),
                };
                return Err(Error::Conflict(format!(
                    "the unique constraint \"{}\" fails: another row in \"{}\" already has {cols}",
                    e.constraint, def.name
                )));
            }
            batch.insert(&ks.indexes, e.key.clone(), e.value.clone());
        }
        batch.insert(&ks.rows, row_key(def.id, pk), encode_row(raw_new)?);
        Ok(())
    }

    // ---- rows ----

    /// Inserts a row, naming each value. Columns left out take their generated value or their default, or are null if the column allows it. Returns the primary key.
    pub fn insert_row(&self, table: &str, values: &[(&str, Value)]) -> Result<Vec<Value>> {
        self.insert_row_with(table, values, &[])
    }

    /// Stages a file for a row of `table` (encrypted if the table is). Nothing refers to it until a row that holds `PendingFile::file_value` is written
    /// with `insert_row_with` or `update_row_with`; if that never happens the bytes are garbage that compaction removes.
    pub fn stage_table_file(
        &self,
        table: &str,
        mime: &str,
        name: Option<&str>,
        reader: impl std::io::Read,
    ) -> Result<PendingFile> {
        let def = self.table(table)?;
        self.stage_with(def.encrypt, mime, name, reader)
    }

    /// The value that makes a row refer to the file an object holds (a version of it, or the newest). The row then keeps that file alive by itself: it does
    /// not depend on the object afterwards, and deleting the object does not touch the row.
    pub fn file_value_of_object(
        &self,
        bucket: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<FileValue> {
        let m = self.get_object(bucket, key, version)?;
        Ok(FileValue {
            id: crate::util::from_hex_32(&m.file_id)
                .ok_or_else(|| Error::Corrupt("an object has a bad file id".into()))?,
            size: m.size,
            mime: m.mime,
            name: m.filename,
        })
    }

    /// Every file value in `cols` must be a file that is stored (or staged with this write) and whose size is the stored size: a row cannot claim a
    /// size or refer to a file that is not there.
    fn check_files(&self, cols: &[(u32, Value)], staged: &[&PendingFile]) -> Result<()> {
        for (_, v) in cols {
            for f in file_values(v) {
                let size = match staged.iter().find(|p| p.file_id == f.id) {
                    Some(p) => p.size(),
                    None => match self.inner.ks.files.get(f.id)? {
                        Some(m) => crate::types::schema_manifest_size(&m)?,
                        None => {
                            return Err(Error::NotFound(
                                "a file this row refers to is not stored".into(),
                            ));
                        }
                    },
                };
                if size != f.size {
                    return Err(Error::Invalid(format!(
                        "a file value says {} bytes, but the stored file is {size}",
                        f.size
                    )));
                }
            }
        }
        Ok(())
    }

    /// Like `insert_row`, with files staged for it. A file value in `values` must be one of `staged` or a file that is already stored.
    pub fn insert_row_with(
        &self,
        table: &str,
        values: &[(&str, Value)],
        staged: &[&PendingFile],
    ) -> Result<Vec<Value>> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?; // read under the lock: the table cannot be altered or dropped between here and the write
        let mut counters: HashMap<u32, u64> = HashMap::new();
        let id = def.id;
        let row = def.build_row_with(values, &mut |c: &ColumnDef, g: Generator| match g {
            Generator::Uuid => Ok(Value::Uuid(uuid_v7())),
            Generator::Now => {
                let ms = crate::util::now_ms();
                match c.ty {
                    crate::types::Type::TimestampTz => Ok(Value::TimestampTz(ms * 1000)),
                    crate::types::Type::Timestamp => Ok(Value::Timestamp(ms * 1000)),
                    _ => Ok(Value::Date(ms.div_euclid(86_400_000) as i32)),
                }
            }
            Generator::Sequence => {
                let last = match counters.get(&c.id) {
                    Some(n) => *n,
                    None => match ks.tables.get(seq_key(id, c.id))? {
                        Some(v) => u64::from_be_bytes(
                            v.as_ref()
                                .try_into()
                                .map_err(|_| Error::Corrupt("a sequence counter".into()))?,
                        ),
                        None => 0,
                    },
                };
                let next = last + 1;
                counters.insert(c.id, next);
                seq_value(&c.ty, next)
            }
        })?;
        let eff = effective(&def, &row)?;
        self.check_constraints(&def, &eff)?;
        self.check_fks(&def, &eff, None)?;
        self.check_files(&row, staged)?;
        let pk = def.key_for_row(&row)?;
        if ks.rows.contains_key(row_key(def.id, &pk))? {
            return Err(Error::Conflict(format!(
                "a row with this primary key already exists in \"{table}\""
            )));
        }
        let mut batch = self.inner.store.batch();
        self.write_row(&mut batch, &def, None, &eff, &row, &pk)?;
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        for id in file_ids(&row) {
            *delta.entry(id).or_default() += 1;
        }
        self.apply_file_deltas(&mut batch, &delta, staged)?;
        for (col, n) in &counters {
            batch.insert(&ks.tables, seq_key(def.id, *col), n.to_be_bytes().to_vec());
        }
        self.finish_batch(batch)?;
        def.key_values(&row)
    }

    pub fn get_row(&self, table: &str, pk: &[Value]) -> Result<Option<Row>> {
        let def = self.table(table)?;
        let key = row_key(def.id, &def.key_for_values(pk)?);
        match self.inner.ks.rows.get(&key)? {
            Some(v) => Ok(Some(def.project(&decode_row(&v)?)?)),
            None => Ok(None),
        }
    }

    /// Changes some columns of a row (`Some` sets a value, `None` makes it null). A primary-key column may be named only with the value it has.
    pub fn update_row(
        &self,
        table: &str,
        pk: &[Value],
        patch: &[(&str, Option<Value>)],
    ) -> Result<Row> {
        self.update_row_with(table, pk, patch, &[])
    }

    /// Like `update_row`, with files staged for it. A file value in `patch` must be one of `staged` or a file that is already stored. A file the row no
    /// longer holds is released.
    pub fn update_row_with(
        &self,
        table: &str,
        pk: &[Value],
        patch: &[(&str, Option<Value>)],
        staged: &[&PendingFile],
    ) -> Result<Row> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?;
        let pk_bytes = def.key_for_values(pk)?;
        let key = row_key(def.id, &pk_bytes);
        let old = ks
            .rows
            .get(&key)?
            .ok_or_else(|| Error::NotFound(format!("that row in \"{table}\"")))?;
        // the stored row, read under today's schema: dropped columns gone, narrower values widened
        let old_eff = effective(&def, &decode_row(&old)?)?;
        let mut cols: Vec<(u32, Value)> = old_eff.clone();
        for (name, newv) in patch {
            let c = def.column(name).ok_or_else(|| {
                Error::Invalid(format!("the table \"{table}\" has no column \"{name}\""))
            })?;
            if def.primary_key.contains(&c.id) {
                let same = cols.iter().find(|(i, _)| *i == c.id).map(|(_, v)| v);
                if newv.as_ref() != same {
                    return Err(Error::Invalid(format!(
                        "\"{name}\" is part of the primary key and cannot be changed"
                    )));
                }
                continue;
            }
            // A null is an absent value, and a default fills in for an absent value, so a column with a default cannot hold a null: it would read back
            // as the default. Better refused than silently different.
            if newv.is_none() && (c.default.is_some() || !c.nullable) {
                return Err(Error::Invalid(if c.nullable {
                    format!(
                        "column \"{name}\" has a default, so it cannot be set to null; set the value you want"
                    )
                } else {
                    format!("column \"{name}\" is NOT NULL and cannot be set to null")
                }));
            }
            cols.retain(|(i, _)| *i != c.id);
            if let Some(v) = newv {
                if v.type_of() != c.ty {
                    return Err(Error::Invalid(format!(
                        "column \"{}\" holds {}, not {}",
                        c.name,
                        c.ty.name(),
                        v.type_of().name()
                    )));
                }
                cols.push((c.id, v.clone()));
            }
        }
        let after = def.project(&cols)?;
        let new_eff = effective(&def, &cols)?;
        self.check_constraints(&def, &new_eff)?;
        self.check_fks(&def, &new_eff, None)?;
        self.check_files(&cols, staged)?;
        let mut batch = self.inner.store.batch();
        self.write_row(
            &mut batch,
            &def,
            Some(old_eff.as_slice()),
            &new_eff,
            &cols,
            &pk_bytes,
        )?;
        // the files the row held as stored, and the files it holds now
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        for id in file_ids(&decode_row(&old)?) {
            *delta.entry(id).or_default() -= 1;
        }
        for id in file_ids(&cols) {
            *delta.entry(id).or_default() += 1;
        }
        self.apply_file_deltas(&mut batch, &delta, staged)?;
        self.finish_batch(batch)?;
        Ok(after)
    }

    /// The primary keys of the rows of `child` whose foreign key `fk` points at the row of the parent table with key encoding `parent_key`.
    fn children(
        &self,
        child: &TableDef,
        fk: &Constraint,
        parent_key: &[u8],
    ) -> Result<Vec<Vec<u8>>> {
        let ConstraintKind::ForeignKey { index, .. } = &fk.kind else {
            return Ok(vec![]);
        };
        let mut prefix = index_prefix(child.id, *index);
        prefix.extend_from_slice(parent_key);
        let mut out = Vec::new();
        for g in self.inner.ks.indexes.prefix(&prefix) {
            let (_, v) = g.into_inner()?;
            out.push(v.to_vec());
        }
        Ok(out)
    }

    /// Deletes a row. Rows that refer to it by foreign key are restricted, deleted or set to null as each constraint says; all of it is one atomic batch,
    /// or none of it happens.
    pub fn delete_row(&self, table: &str, pk: &[Value]) -> Result<bool> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?;
        let pk_bytes = def.key_for_values(pk)?;
        if !ks.rows.contains_key(row_key(def.id, &pk_bytes))? {
            return Ok(false);
        }
        let defs: HashMap<u32, TableDef> = self.tables()?.into_iter().map(|d| (d.id, d)).collect();
        let load = |d: &TableDef, key: &[u8]| -> Result<Vec<(u32, Value)>> {
            let raw = ks.rows.get(row_key(d.id, key))?.ok_or_else(|| {
                Error::Corrupt("an index points at a row that does not exist".into())
            })?;
            effective(d, &decode_row(&raw)?)
        };

        // Phase 1: everything that goes: the row, and whatever cascades from it.
        let mut doomed: BTreeMap<(u32, Vec<u8>), Eff> = BTreeMap::new();
        let mut queue: Vec<(u32, Vec<u8>)> = vec![(def.id, pk_bytes.clone())];
        while let Some((tid, key)) = queue.pop() {
            if doomed.contains_key(&(tid, key.clone())) {
                continue;
            }
            let d = defs
                .get(&tid)
                .ok_or_else(|| Error::Corrupt("a row belongs to a table that is gone".into()))?;
            doomed.insert((tid, key.clone()), load(d, &key)?);
            if doomed.len() > MAX_CASCADE {
                return Err(Error::Invalid(format!(
                    "deleting this row would cascade to more than {MAX_CASCADE} rows; delete them in smaller steps"
                )));
            }
            for (child, fk) in referencing(&defs, tid) {
                if matches!(
                    &fk.kind,
                    ConstraintKind::ForeignKey {
                        on_delete: OnDelete::Cascade,
                        ..
                    }
                ) {
                    for ck in self.children(child, fk, &key)? {
                        queue.push((child.id, ck));
                    }
                }
            }
        }
        // Phase 2: rows outside that set that refer to a doomed row: restrict refuses, set null changes them.
        let mut nulled: BTreeMap<(u32, Vec<u8>), (Eff, Eff)> = BTreeMap::new(); // old effective, new effective
        for (tid, key) in doomed.keys() {
            for (child, fk) in referencing(&defs, *tid) {
                let ConstraintKind::ForeignKey {
                    on_delete, columns, ..
                } = &fk.kind
                else {
                    continue;
                };
                for ck in self.children(child, fk, key)? {
                    if doomed.contains_key(&(child.id, ck.clone())) {
                        continue;
                    }
                    match on_delete {
                        OnDelete::Restrict => {
                            return Err(Error::Conflict(format!(
                                "the row cannot be deleted: a row in \"{}\" refers to it through the foreign key \"{}\"",
                                child.name, fk.name
                            )));
                        }
                        OnDelete::SetNull => {
                            let entry = match nulled.get(&(child.id, ck.clone())) {
                                Some((o, n)) => (o.clone(), n.clone()),
                                None => {
                                    let e = load(child, &ck)?;
                                    (e.clone(), e)
                                }
                            };
                            let (o, mut n) = entry;
                            n.retain(|(i, _)| !columns.contains(i));
                            nulled.insert((child.id, ck), (o, n));
                        }
                        OnDelete::Cascade => {} // already in `doomed`
                    }
                }
            }
        }
        for ((tid, _), (_, new_eff)) in &nulled {
            let d = &defs[tid];
            self.check_constraints(d, new_eff)?;
            d.project(new_eff).map_err(|e| {
                Error::Invalid(format!(
                    "setting the foreign key to null would leave a row invalid: {e}"
                ))
            })?;
        }
        // Apply, in one batch.
        let mut batch = self.inner.store.batch();
        for ((tid, key), eff) in &doomed {
            let d = &defs[tid];
            batch.remove(&ks.rows, row_key(*tid, key));
            for e in entries(d, eff, key)? {
                batch.remove(&ks.indexes, e.key);
            }
        }
        for ((tid, key), (old_eff, new_eff)) in &nulled {
            let d = &defs[tid];
            self.write_row(
                &mut batch,
                d,
                Some(old_eff.as_slice()),
                new_eff,
                new_eff,
                key,
            )?;
        }
        // the files: every doomed row releases what it holds as stored; a row that was set to null holds what it now holds instead of what it held
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        for (tid, key) in doomed.keys() {
            let raw = ks
                .rows
                .get(row_key(*tid, key))?
                .ok_or_else(|| Error::Corrupt("a row to delete is gone".into()))?;
            for id in file_ids(&decode_row(&raw)?) {
                *delta.entry(id).or_default() -= 1;
            }
        }
        for ((tid, key), (_, new_eff)) in &nulled {
            let raw = ks
                .rows
                .get(row_key(*tid, key))?
                .ok_or_else(|| Error::Corrupt("a row to change is gone".into()))?;
            for id in file_ids(&decode_row(&raw)?) {
                *delta.entry(id).or_default() -= 1;
            }
            for id in file_ids(new_eff) {
                *delta.entry(id).or_default() += 1;
            }
        }
        self.apply_file_deltas(&mut batch, &delta, &[])?;
        self.finish_batch(batch)?;
        Ok(true)
    }

    /// Rows in primary-key order, after the key `after` (exclusive), at most `limit` (1 to 10,000).
    pub fn scan_rows(
        &self,
        table: &str,
        after: Option<&[Value]>,
        limit: usize,
    ) -> Result<Vec<Row>> {
        let def = self.table(table)?;
        let limit = limit.clamp(1, MAX_SCAN);
        let prefix = def.id.to_be_bytes().to_vec();
        let (start, skip) = match after {
            Some(a) => (row_key(def.id, &def.key_for_values(a)?), true),
            None => (prefix.clone(), false),
        };
        let mut out = Vec::new();
        for g in self.inner.ks.rows.range(start.clone()..) {
            let (k, v) = g.into_inner()?;
            if !k.starts_with(&prefix) {
                break;
            }
            if skip && k.as_ref() == start.as_slice() {
                continue;
            }
            out.push(def.project(&decode_row(&v)?)?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Finds rows through an index (one made with `AddIndex`, or the index behind a unique constraint or foreign key, by its name): the rows whose leading
    /// indexed columns equal `prefix` (it may name fewer values than the index has columns, but not more), in index order, then primary-key order. A row with
    /// a null in any indexed column is not in the index. Pass the `next` of the previous page as `after` to continue.
    pub fn scan_index(
        &self,
        table: &str,
        index: &str,
        prefix: &[Value],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<IndexPage> {
        let def = self.table(table)?;
        let (index_id, columns): (u32, &[u32]) =
            if let Some(x) = def.indexes.iter().find(|x| x.name == index) {
                (x.index, &x.columns)
            } else if let Some(k) = def.constraints.iter().find(|k| k.name == index) {
                match &k.kind {
                    ConstraintKind::Unique { columns, index }
                    | ConstraintKind::ForeignKey { columns, index, .. } => (*index, columns),
                    ConstraintKind::Check { .. } => {
                        return Err(Error::Invalid(format!(
                            "\"{index}\" is a check, not an index"
                        )));
                    }
                }
            } else {
                return Err(Error::NotFound(format!(
                    "index \"{index}\" on table \"{table}\""
                )));
            };
        if prefix.len() > columns.len() {
            return Err(Error::Invalid(format!(
                "the index \"{index}\" has {} column(s); {} values given",
                columns.len(),
                prefix.len()
            )));
        }
        let mut start = index_prefix(def.id, index_id);
        for (v, col) in prefix.iter().zip(columns) {
            let c = def
                .column_by_id(*col)
                .ok_or_else(|| Error::Corrupt("an index column is missing".into()))?;
            if v.type_of() != c.ty {
                return Err(Error::Invalid(format!(
                    "column \"{}\" holds {}, not {}",
                    c.name,
                    c.ty.name(),
                    v.type_of().name()
                )));
            }
            encode_key(&mut start, Some(v))?;
        }
        let lower = after.map(|a| a.to_vec()).unwrap_or_else(|| start.clone());
        let limit = limit.clamp(1, MAX_SCAN);
        let mut rows = Vec::new();
        let mut last = None;
        for g in self.inner.ks.indexes.range(lower.clone()..) {
            let (k, v) = g.into_inner()?;
            if !k.starts_with(&start) {
                break;
            }
            if after.is_some() && k.as_ref() == lower.as_slice() {
                continue;
            }
            let raw = self
                .inner
                .ks
                .rows
                .get(row_key(def.id, &v))?
                .ok_or_else(|| {
                    Error::Corrupt("an index entry points at a row that does not exist".into())
                })?;
            rows.push(def.project(&decode_row(&raw)?)?);
            last = Some(k.to_vec());
            if rows.len() >= limit {
                break;
            }
        }
        let next = if rows.len() >= limit { last } else { None };
        Ok(IndexPage { rows, next })
    }

    pub fn count_rows(&self, table: &str) -> Result<u64> {
        let def = self.table(table)?;
        let mut n = 0;
        for g in self.inner.ks.rows.prefix(def.id.to_be_bytes()) {
            g.key()?;
            n += 1;
        }
        Ok(n)
    }
}
