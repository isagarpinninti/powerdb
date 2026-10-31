//! The list query: `filter`, `sort`, `limit`, `offset` from the URL, checked against the collection's schema before anything runs.

use super::ApiError;
use crate::model::{CollectionDef, Record};
use crate::security::expr::{self, Expr};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;

pub const DEFAULT_LIMIT: usize = 30;
pub const MAX_LIMIT: usize = 500;
/// Sorting by anything but creation time needs every match in memory; refuse past this many rather than exhaust it.
pub const MAX_SORT_SCAN: usize = 100_000;

pub struct ListQuery {
    pub filter: Option<Expr>,
    pub sort: Vec<(String, bool)>, // (field, descending)
    pub limit: usize,
    pub offset: usize,
}

const SYSTEM: [&str; 4] = ["id", "collection", "created_at", "updated_at"];

fn known(def: &CollectionDef, name: &str) -> bool {
    SYSTEM.contains(&name) || def.fields.iter().any(|f| f.name == name)
}

fn check_paths(e: &Expr, def: &CollectionDef) -> Result<(), ApiError> {
    match e {
        Expr::Path { at: false, parts } => {
            let head = if parts.first().map(String::as_str) == Some("record") { parts.get(1) } else { parts.first() };
            match head {
                Some(h) if known(def, h) => Ok(()),
                Some(h) => Err(ApiError::bad_request(&format!("filter: collection \"{}\" has no field \"{h}\"", def.name))),
                None => Ok(()),
            }
        }
        Expr::Not(a) => check_paths(a, def),
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Cmp(_, a, b) => {
            check_paths(a, def)?;
            check_paths(b, def)
        }
        _ => Ok(()),
    }
}

pub fn parse(params: &HashMap<String, String>, def: &CollectionDef) -> Result<ListQuery, ApiError> {
    let filter = match params.get("filter").map(|f| f.trim()).filter(|f| !f.is_empty()) {
        Some(f) => {
            let e = expr::parse(f).map_err(|e| ApiError::bad_request(&e.to_string()))?;
            check_paths(&e, def)?;
            Some(e)
        }
        None => None,
    };
    let mut sort = Vec::new();
    if let Some(s) = params.get("sort") {
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (desc, name) = match part.strip_prefix('-') {
                Some(n) => (true, n),
                None => (false, part.strip_prefix('+').unwrap_or(part)),
            };
            if !known(def, name) {
                return Err(ApiError::bad_request(&format!("sort: collection \"{}\" has no field \"{name}\"", def.name)));
            }
            sort.push((name.to_string(), desc));
        }
        if sort.len() > 4 {
            return Err(ApiError::bad_request("sort: at most four fields"));
        }
    }
    let num = |k: &str, d: usize| -> Result<usize, ApiError> {
        match params.get(k) {
            None => Ok(d),
            Some(v) => v.parse::<usize>().map_err(|_| ApiError::bad_request(&format!("{k} must be a whole number"))),
        }
    };
    let limit = num("limit", DEFAULT_LIMIT)?;
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_request(&format!("limit must be between 1 and {MAX_LIMIT}")));
    }
    Ok(ListQuery { filter, sort, limit, offset: num("offset", 0)? })
}

impl ListQuery {
    /// Records come out of storage in creation order, so sorting by creation time (or id) needs no sort at all.
    pub fn fast_order(&self) -> Option<bool> {
        match self.sort.as_slice() {
            [] => Some(true),
            [(f, desc)] if f == "created_at" || f == "id" => Some(*desc),
            _ => None,
        }
    }
}

fn field(r: &Record, name: &str) -> Value {
    match name {
        "id" => Value::String(r.id.clone()),
        "collection" => Value::String(r.collection.clone()),
        "created_at" => Value::from(r.created_at),
        "updated_at" => Value::from(r.updated_at),
        n => r.fields.get(n).cloned().unwrap_or(Value::Null),
    }
}

fn cmp_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater, // missing values sort last, whichever the direction
        (_, Value::Null) => Ordering::Less,
        (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.to_lowercase().cmp(&y.to_lowercase()).then_with(|| x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

pub fn compare(a: &Record, b: &Record, sort: &[(String, bool)]) -> Ordering {
    for (name, desc) in sort {
        let (x, y) = (field(a, name), field(b, name));
        let o = cmp_values(&x, &y);
        let o = if x.is_null() || y.is_null() { o } else if *desc { o.reverse() } else { o };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.id.cmp(&b.id)
}
