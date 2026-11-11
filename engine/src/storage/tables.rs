//! Typed tables in storage (see docs/TYPES_AND_TABLES.md).
//!
//! * `tables`: table name -> the table's definition (`TableDef::to_bytes`). One reserved key, `[0]`, holds the next table id: names start with a
//!   lowercase letter, so no table can collide with it.
//! * `rows`: `table id (4 bytes, big-endian)` + the primary key's key encoding -> the row (`types::row`).
//!
//! Tables live beside collections: collections and their JSON records are untouched, and nothing here rewrites them. A row names its columns by
//! id, so the schema changes that touch only the catalog (add, drop, rename, widen) leave stored rows as they are; reading a row applies the
//! current schema to it (`TableDef::project`).
//!
//! Every write is one atomic batch under the commit lock, the same as a record's or an object's, so the failure points and the crash tests cover
//! them too. In this slice a table has no secondary indexes, unique constraints or foreign keys (slices 2d and 2e), and no files.

use super::Db;
use crate::error::{Error, Result};
use crate::types::Value;
use crate::types::row::{decode_row, encode_row};
use crate::types::schema::{Change, Effect, TableDef};

/// A row as a caller sees it: the table's columns in order, a null as `None`.
pub type Row = Vec<(String, Option<Value>)>;

const COUNTER_KEY: [u8; 1] = [0];
const DROP_BATCH: usize = 2000;
pub const MAX_SCAN: usize = 10_000;

fn row_key(table_id: u32, key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(4 + key.len());
    k.extend_from_slice(&table_id.to_be_bytes());
    k.extend_from_slice(key);
    k
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
            if k.as_ref() == COUNTER_KEY {
                continue;
            }
            out.push(TableDef::from_bytes(&v)?);
        }
        Ok(out)
    }

    /// Stores a new table. The definition needs a primary key and only the column types tables support; it gets a table id here.
    pub fn create_table(&self, mut def: TableDef) -> Result<TableDef> {
        def.validate_for_storage()?;
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

    /// Applies a schema change. A change that must look at the existing rows first (a new NOT NULL) does so under the commit lock, so no row can be
    /// written between the check and the change; a change that does not touches only the catalog entry.
    pub fn alter_table(&self, name: &str, change: &Change) -> Result<TableDef> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(name)?;
        let (new, effect) = def.apply(change, !self.has_rows(def.id)?)?;
        if let Effect::NeedsScan(why) = effect {
            for g in ks.rows.prefix(def.id.to_be_bytes()) {
                let (_, v) = g.into_inner()?;
                new.project(&decode_row(&v)?).map_err(|e| {
                    Error::Invalid(format!("the change cannot be made: {why} ({e})"))
                })?;
            }
        }
        let mut batch = self.inner.store.batch();
        batch.insert(&ks.tables, name.as_bytes().to_vec(), new.to_bytes()?);
        self.finish_batch(batch)?;
        Ok(new)
    }

    /// Removes a table and all its rows. The rows go in batches, then the catalog entry, so a crash part-way leaves the table (with fewer rows)
    /// and the call can simply be repeated. Writers wait while it runs. Returns the number of rows removed.
    pub fn drop_table(&self, name: &str) -> Result<u64> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(name)?;
        let mut removed = 0u64;
        loop {
            let mut keys = Vec::new();
            for g in ks.rows.prefix(def.id.to_be_bytes()).take(DROP_BATCH) {
                keys.push(g.key()?.to_vec());
            }
            if keys.is_empty() {
                break;
            }
            let mut batch = self.inner.store.batch();
            for k in &keys {
                batch.remove(&ks.rows, k.clone());
            }
            removed += keys.len() as u64;
            self.finish_batch(batch)?;
        }
        let mut batch = self.inner.store.batch();
        batch.remove(&ks.tables, name.as_bytes().to_vec());
        self.finish_batch(batch)?;
        Ok(removed)
    }

    // ---- rows ----

    /// Inserts a row, naming each value. Columns left out take their default, or are null if the column allows it. Returns the primary key.
    pub fn insert_row(&self, table: &str, values: &[(&str, Value)]) -> Result<Vec<Value>> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?; // read under the lock: the table cannot be altered or dropped between here and the write
        let row = def.build_row(values)?;
        let key = row_key(def.id, &def.key_for_row(&row)?);
        if ks.rows.contains_key(&key)? {
            return Err(Error::Conflict(format!(
                "a row with this primary key already exists in \"{table}\""
            )));
        }
        let mut batch = self.inner.store.batch();
        batch.insert(&ks.rows, key, encode_row(&row)?);
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
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?;
        let key = row_key(def.id, &def.key_for_values(pk)?);
        let old = ks
            .rows
            .get(&key)?
            .ok_or_else(|| Error::NotFound(format!("that row in \"{table}\"")))?;
        // the stored row, read under today's schema: dropped columns gone, narrower values widened
        let current = def.project(&decode_row(&old)?)?;
        let mut cols: Vec<(u32, Value)> = Vec::new();
        for (c, (_, v)) in def.columns.iter().zip(&current) {
            if let Some(v) = v {
                cols.push((c.id, v.clone()));
            }
        }
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
        let mut batch = self.inner.store.batch();
        batch.insert(&ks.rows, key, encode_row(&cols)?);
        self.finish_batch(batch)?;
        Ok(after)
    }

    pub fn delete_row(&self, table: &str, pk: &[Value]) -> Result<bool> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.table(table)?;
        let key = row_key(def.id, &def.key_for_values(pk)?);
        if !ks.rows.contains_key(&key)? {
            return Ok(false);
        }
        let mut batch = self.inner.store.batch();
        batch.remove(&ks.rows, key);
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
