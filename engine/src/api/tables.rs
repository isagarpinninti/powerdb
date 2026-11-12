//! Typed tables over HTTP. Like buckets, these are for administrators in this release: collections have rules, and tables get their own permissions later.
//!
//! * `GET|POST /tables`, `GET|PATCH|DELETE /tables/{name}`: the catalog. `PATCH` takes one schema change, `{"op": "add_column", ...}`.
//! * `GET|POST /tables/{name}/rows`, `GET|PATCH|DELETE /tables/{name}/rows/{key}`: rows. `{key}` is the primary key for a one-column key; for any table
//!   `?key=[...]` (a JSON array of the key's values) is accepted instead. A row is a JSON object of column values; see `types::json` for each type's form.
//!   `POST` and `PATCH` take JSON, or `multipart/form-data` whose first part is `data` (the JSON object) followed by a part per file, named after its column.
//! * `GET /tables/{name}/rows/{key}/files/{column}` (`?index=n` for a list of files): a stored file, with ranges.
//! * `GET /tables/{name}/indexes/{index}/rows?values=[...]`: rows found through an index, by the leading values.

use super::admin::{admin, blocking};
use super::files::serve_file;
use super::upload::{clean_name, stage_table_part};
use super::{ApiError, AppState, Caller};
use crate::storage::PendingFile;
use crate::storage::tables::Row;
use crate::types::json::{file_json, from_json, to_json};
use crate::types::schema::{Change, ConstraintKind, Generator, OnDelete, TableDef};
use crate::types::{FileValue, Type, Value as V};
use crate::util::{from_hex_32, hex};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value as J, json};
use std::collections::HashMap;

const MAX_LIST: usize = 1000;

// ---- the catalog as JSON ----

fn names(def: &TableDef, ids: &[u32]) -> Vec<String> {
    ids.iter()
        .map(|i| {
            def.column_by_id(*i)
                .map(|c| c.name.clone())
                .unwrap_or_default()
        })
        .collect()
}

pub fn table_json(d: &TableDef) -> J {
    let columns: Vec<J> = d
        .columns
        .iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("name".into(), json!(c.name));
            m.insert("type".into(), json!(c.ty.name()));
            m.insert("nullable".into(), json!(c.nullable));
            if let Some(v) = &c.default {
                m.insert("default".into(), to_json(v));
            }
            if let Some(g) = c.generate {
                m.insert("generate".into(), json!(g.name()));
            }
            J::Object(m)
        })
        .collect();
    let constraints: Vec<J> = d
        .constraints
        .iter()
        .map(|k| match &k.kind {
            ConstraintKind::Unique { columns, .. } => json!({"name": k.name, "kind": "unique", "columns": names(d, columns)}),
            ConstraintKind::Check { expr } => json!({"name": k.name, "kind": "check", "expr": expr}),
            ConstraintKind::ForeignKey { columns, ref_table, on_delete, .. } => {
                json!({"name": k.name, "kind": "foreign_key", "columns": names(d, columns), "ref_table": ref_table, "on_delete": on_delete.name()})
            }
        })
        .collect();
    let indexes: Vec<J> = d
        .indexes
        .iter()
        .map(|x| json!({"name": x.name, "columns": names(d, &x.columns)}))
        .collect();
    json!({
        "name": d.name, "version": d.version, "encrypt": d.encrypt,
        "columns": columns, "primary_key": names(d, &d.primary_key),
        "constraints": constraints, "indexes": indexes,
    })
}

fn text_of<'a>(o: &'a Map<String, J>, k: &str) -> Result<&'a str, ApiError> {
    o.get(k)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request(&format!("\"{k}\" is required and must be a string")))
}

fn names_of(o: &Map<String, J>, k: &str) -> Result<Vec<String>, ApiError> {
    o.get(k)
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            ApiError::bad_request(&format!(
                "\"{k}\" is required and must be a list of column names"
            ))
        })?
        .iter()
        .map(|v| {
            v.as_str().map(str::to_string).ok_or_else(|| {
                ApiError::bad_request(&format!("\"{k}\" must be a list of column names"))
            })
        })
        .collect()
}

fn parse_type(s: &str) -> Result<Type, ApiError> {
    Type::parse_column_type(s).ok_or_else(|| {
        ApiError::bad_request(&format!(
            "\"{s}\" is not a column type (bool, int8 to int64, uint8 to uint64, float32, float64, decimal, text, uuid, date, time, timestamp, timestamptz, json, binary, file, or array<...> of these)"
        ))
    })
}

fn json_object(bytes: &Bytes) -> Result<Map<String, J>, ApiError> {
    match serde_json::from_slice::<J>(bytes) {
        Ok(J::Object(o)) => Ok(o),
        Ok(_) => Err(ApiError::bad_request("the body must be a JSON object")),
        Err(e) => Err(ApiError::bad_request(&format!(
            "the body is not valid JSON: {e}"
        ))),
    }
}

/// One schema change from its JSON form (`{"op": "add_column", "name": ..., ...}`).
fn change_from(op: &Map<String, J>) -> Result<Change, ApiError> {
    let kind = text_of(op, "op")?;
    Ok(match kind {
        "add_column" | "add_generated_column" => {
            let name = text_of(op, "name")?.to_string();
            let ty = parse_type(text_of(op, "type")?)?;
            let nullable = op.get("nullable").and_then(|v| v.as_bool()).unwrap_or(true);
            if kind == "add_generated_column" {
                let g = Generator::parse(text_of(op, "generate")?).ok_or_else(|| {
                    ApiError::bad_request("\"generate\" must be uuid, now or sequence")
                })?;
                Change::AddGeneratedColumn {
                    name,
                    ty,
                    nullable,
                    generate: g,
                }
            } else {
                let default = match op.get("default") {
                    None | Some(J::Null) => None,
                    Some(d) => Some(from_json(&ty, d).map_err(|e| {
                        ApiError::bad_request(&format!("default of \"{name}\": {e}"))
                    })?),
                };
                Change::AddColumn {
                    name,
                    ty,
                    nullable,
                    default,
                }
            }
        }
        "drop_column" => Change::DropColumn {
            name: text_of(op, "name")?.into(),
        },
        "rename_column" => Change::RenameColumn {
            from: text_of(op, "from")?.into(),
            to: text_of(op, "to")?.into(),
        },
        "widen_column" => Change::WidenColumn {
            name: text_of(op, "name")?.into(),
            to: parse_type(text_of(op, "to")?)?,
        },
        "change_type" => Change::ChangeType {
            name: text_of(op, "name")?.into(),
            to: parse_type(text_of(op, "to")?)?,
        },
        "set_nullable" => Change::SetNullable {
            name: text_of(op, "name")?.into(),
        },
        "set_not_null" => Change::SetNotNull {
            name: text_of(op, "name")?.into(),
        },
        "add_unique" => Change::AddUnique {
            name: text_of(op, "name")?.into(),
            columns: names_of(op, "columns")?,
        },
        "add_check" => Change::AddCheck {
            name: text_of(op, "name")?.into(),
            expr: text_of(op, "expr")?.into(),
        },
        "add_foreign_key" => Change::AddForeignKey {
            name: text_of(op, "name")?.into(),
            columns: names_of(op, "columns")?,
            ref_table: text_of(op, "ref_table")?.into(),
            ref_table_id: 0,
            ref_pk_types: vec![],
            on_delete: match op.get("on_delete").and_then(|v| v.as_str()) {
                None => OnDelete::Restrict,
                Some(s) => OnDelete::parse(s).ok_or_else(|| {
                    ApiError::bad_request("\"on_delete\" must be restrict, cascade or set_null")
                })?,
            },
        },
        "drop_constraint" => Change::DropConstraint {
            name: text_of(op, "name")?.into(),
        },
        "add_index" => Change::AddIndex {
            name: text_of(op, "name")?.into(),
            columns: names_of(op, "columns")?,
        },
        "drop_index" => Change::DropIndex {
            name: text_of(op, "name")?.into(),
        },
        other => {
            return Err(ApiError::bad_request(&format!(
                "\"{other}\" is not a schema change (add_column, add_generated_column, drop_column, rename_column, widen_column, change_type, set_nullable, set_not_null, add_unique, add_check, add_foreign_key, drop_constraint, add_index, drop_index)"
            )));
        }
    })
}

pub async fn list_tables(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let items = blocking(move || db.tables()).await?;
    Ok(Json(
        json!({ "items": items.iter().map(table_json).collect::<Vec<_>>() }),
    ))
}

pub async fn create_table(
    State(st): State<AppState>,
    Caller(who): Caller,
    body: Bytes,
) -> Result<(StatusCode, Json<J>), ApiError> {
    admin(&who)?;
    let spec = json_object(&body)?;
    let name = text_of(&spec, "name")?.to_string();
    let pk = names_of(&spec, "primary_key")?;
    let mut t = TableDef::new(&name).map_err(ApiError::from)?;
    for c in spec
        .get("columns")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::bad_request("\"columns\" is required and must be a list"))?
    {
        let c = c
            .as_object()
            .ok_or_else(|| ApiError::bad_request("each column must be an object"))?;
        let cname = text_of(c, "name")?.to_string();
        let mut op = c.clone();
        op.insert(
            "op".into(),
            json!(if c.contains_key("generate") {
                "add_generated_column"
            } else {
                "add_column"
            }),
        );
        // a primary key column is NOT NULL unless it says otherwise
        if !op.contains_key("nullable") {
            op.insert("nullable".into(), json!(!pk.contains(&cname)));
        }
        t = t.apply(&change_from(&op)?, true).map_err(ApiError::from)?.0;
    }
    t.encrypt = spec
        .get("encrypt")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pk_refs: Vec<&str> = pk.iter().map(String::as_str).collect();
    t.set_primary_key(&pk_refs).map_err(ApiError::from)?;
    let mut later = Vec::new();
    for (key, kind) in [("constraints", None), ("indexes", Some("add_index"))] {
        for item in spec
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let mut o = item
                .as_object()
                .ok_or_else(|| {
                    ApiError::bad_request(&format!("each of \"{key}\" must be an object"))
                })?
                .clone();
            let op = match kind {
                Some(k) => k.to_string(),
                None => match o.get("kind").and_then(|v| v.as_str()) {
                    Some("unique") => "add_unique".into(),
                    Some("check") => "add_check".into(),
                    Some("foreign_key") => "add_foreign_key".into(),
                    _ => {
                        return Err(ApiError::bad_request(
                            "a constraint's \"kind\" must be unique, check or foreign_key",
                        ));
                    }
                },
            };
            o.insert("op".into(), json!(op));
            later.push(change_from(&o)?);
        }
    }
    let db = st.db.clone();
    let made = blocking(move || {
        let made = db.create_table(t)?;
        // constraints and indexes go on one at a time; if one fails the table is removed again, so a refused request leaves nothing behind
        for ch in &later {
            if let Err(e) = db.alter_table(&made.name, ch) {
                let _ = db.drop_table(&made.name);
                return Err(e);
            }
        }
        db.table(&made.name)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "table": table_json(&made) })),
    ))
}

pub async fn get_table(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let t = blocking(move || db.table(&name)).await?;
    let db = st.db.clone();
    let tn = t.name.clone();
    let rows = blocking(move || db.count_rows(&tn)).await?;
    Ok(Json(json!({ "table": table_json(&t), "rows": rows })))
}

pub async fn alter_table(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let change = change_from(&json_object(&body)?)?;
    let db = st.db.clone();
    let t = blocking(move || db.alter_table(&name, &change)).await?;
    Ok(Json(json!({ "table": table_json(&t) })))
}

pub async fn drop_table(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let n = blocking(move || db.drop_table(&name)).await?;
    Ok(Json(json!({ "deleted_rows": n })))
}

// ---- rows as JSON ----

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The primary key of a row as the values in key order.
fn key_values(def: &TableDef, row: &Row) -> Vec<V> {
    def.primary_key
        .iter()
        .filter_map(|id| def.column_by_id(*id))
        .filter_map(|c| {
            row.iter()
                .find(|(n, _)| *n == c.name)
                .and_then(|(_, v)| v.clone())
        })
        .collect()
}

/// `(path segment, query)` that address a row.
fn address(def: &TableDef, key: &[V]) -> (String, String) {
    if let [V::Text(s)] = key {
        return (enc(s), String::new());
    }
    if let [single] = key {
        if let J::String(s) = to_json(single) {
            return (enc(&s), String::new());
        }
        return (enc(&to_json(single).to_string()), String::new());
    }
    let _ = def;
    (
        "_".into(),
        format!(
            "key={}",
            enc(&J::Array(key.iter().map(to_json).collect()).to_string())
        ),
    )
}

fn row_json(def: &TableDef, row: &Row) -> J {
    let key = key_values(def, row);
    let (seg, q) = address(def, &key);
    let url = |col: &str, idx: Option<usize>| -> String {
        let mut parts = Vec::new();
        if !q.is_empty() {
            parts.push(q.clone());
        }
        if let Some(i) = idx {
            parts.push(format!("index={i}"));
        }
        let query = if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        };
        format!(
            "/api/v1/tables/{}/rows/{seg}/files/{}{query}",
            def.name,
            enc(col)
        )
    };
    let mut m = Map::new();
    for (name, v) in row {
        let j = match v {
            None => J::Null,
            Some(V::File(f)) => {
                let mut o = file_json(f);
                o["url"] = json!(url(name, None));
                o
            }
            Some(V::Array(_, items))
                if items.iter().all(|i| matches!(i, V::File(_))) && !items.is_empty() =>
            {
                J::Array(
                    items
                        .iter()
                        .enumerate()
                        .map(|(n, i)| {
                            let V::File(f) = i else { unreachable!() };
                            let mut o = file_json(f);
                            o["url"] = json!(url(name, Some(n)));
                            o
                        })
                        .collect(),
                )
            }
            Some(v) => to_json(v),
        };
        m.insert(name.clone(), j);
    }
    J::Object(m)
}

/// The key of a row from the address: `?key=[...]` if given, otherwise the path segment (one-column keys only).
fn parse_key(def: &TableDef, seg: &str, q: &HashMap<String, String>) -> Result<Vec<V>, ApiError> {
    let cols: Vec<_> = def
        .primary_key
        .iter()
        .filter_map(|i| def.column_by_id(*i))
        .collect();
    if let Some(k) = q.get("key") {
        let arr: Vec<J> = serde_json::from_str(k)
            .map_err(|_| ApiError::bad_request("key must be a JSON array of the key's values"))?;
        if arr.len() != cols.len() {
            return Err(ApiError::bad_request(&format!(
                "the primary key of \"{}\" has {} column(s); {} given",
                def.name,
                cols.len(),
                arr.len()
            )));
        }
        return cols
            .iter()
            .zip(&arr)
            .map(|(c, j)| {
                from_json(&c.ty, j)
                    .map_err(|e| ApiError::bad_request(&format!("key column \"{}\": {e}", c.name)))
            })
            .collect();
    }
    let [c] = cols.as_slice() else {
        return Err(ApiError::bad_request(&format!(
            "the primary key of \"{}\" has {} columns; send them as ?key=[...]",
            def.name,
            cols.len()
        )));
    };
    let j = match c.ty {
        Type::Bool => J::Bool(match seg {
            "true" => true,
            "false" => false,
            _ => return Err(ApiError::bad_request("a boolean key is true or false")),
        }),
        _ => J::String(seg.to_string()),
    };
    Ok(vec![from_json(&c.ty, &j).map_err(|e| {
        ApiError::bad_request(&format!("key column \"{}\": {e}", c.name))
    })?])
}

fn cursor_of(key: &[V]) -> String {
    URL_SAFE_NO_PAD.encode(J::Array(key.iter().map(to_json).collect()).to_string())
}

fn cursor_to(def: &TableDef, s: &str) -> Result<Vec<V>, ApiError> {
    let raw = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| ApiError::bad_request("that is not a cursor from a previous page"))?;
    let arr: Vec<J> = serde_json::from_slice(&raw)
        .map_err(|_| ApiError::bad_request("that is not a cursor from a previous page"))?;
    let q: HashMap<String, String> = [("key".to_string(), J::Array(arr).to_string())].into();
    parse_key(def, "", &q)
}

fn limit_of(q: &HashMap<String, String>) -> Result<usize, ApiError> {
    match q.get("limit") {
        None => Ok(100),
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIST).contains(n))
            .ok_or_else(|| {
                ApiError::bad_request(&format!("limit must be a number from 1 to {MAX_LIST}"))
            }),
    }
}

async fn table(st: &AppState, name: &str) -> Result<TableDef, ApiError> {
    let db = st.db.clone();
    let n = name.to_string();
    blocking(move || db.table(&n)).await
}

pub async fn list_rows(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let limit = limit_of(&q)?;
    let after = q.get("after").map(|a| cursor_to(&def, a)).transpose()?;
    let db = st.db.clone();
    let n = name.clone();
    let rows = blocking(move || db.scan_rows(&n, after.as_deref(), limit)).await?;
    let next = if rows.len() >= limit {
        rows.last().map(|r| cursor_of(&key_values(&def, r)))
    } else {
        None
    };
    Ok(Json(
        json!({ "items": rows.iter().map(|r| row_json(&def, r)).collect::<Vec<_>>(), "next": next, "limit": limit }),
    ))
}

/// What a write sends: the JSON fields, and the files uploaded with them (each with the column it is for).
struct Input {
    fields: Map<String, J>,
    files: Vec<(String, PendingFile)>,
}

async fn read_input(st: &AppState, def: &TableDef, req: Request) -> Result<Input, ApiError> {
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ct.starts_with("multipart/form-data") {
        if !ct.is_empty() && !ct.starts_with("application/json") {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "invalid",
                "send application/json, or multipart/form-data with a \"data\" part",
            ));
        }
        let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
            .await
            .map_err(|_| {
                ApiError::too_large(
                    "a JSON body is limited to 1 MiB; send files as multipart/form-data",
                )
            })?;
        return Ok(Input {
            fields: json_object(&bytes)?,
            files: vec![],
        });
    }
    let mut mp = Multipart::from_request(req, st)
        .await
        .map_err(|e| ApiError::bad_request(&format!("bad multipart request: {}", e.body_text())))?;
    let mut input = Input {
        fields: Map::new(),
        files: vec![],
    };
    let mut got_data = false;
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(&format!("bad multipart request: {}", e.body_text())))?
    {
        let name = field
            .name()
            .ok_or_else(|| ApiError::bad_request("a part has no name"))?
            .to_string();
        if name == "data" {
            if got_data {
                return Err(ApiError::bad_request("the \"data\" part appears twice"));
            }
            let mut body = Vec::new();
            while let Some(c) = field.chunk().await.map_err(|e| {
                ApiError::bad_request(&format!("bad multipart request: {}", e.body_text()))
            })? {
                body.extend_from_slice(&c);
                if body.len() > 1 << 20 {
                    return Err(ApiError::too_large(
                        "the \"data\" part is larger than 1 MiB",
                    ));
                }
            }
            input.fields = json_object(&Bytes::from(body))?;
            got_data = true;
            continue;
        }
        if !got_data {
            return Err(ApiError::bad_request(
                "send the \"data\" part (the row's values as JSON, {} if none) before any file",
            ));
        }
        let col = def
            .column(&name)
            .filter(|c| c.ty.is_file_column())
            .ok_or_else(|| {
                ApiError::bad_request(&format!(
                    "the table \"{}\" has no file column \"{name}\"",
                    def.name
                ))
            })?;
        let single = col.ty == Type::File;
        if single && input.files.iter().any(|(n, _)| *n == name) {
            return Err(ApiError::bad_request(&format!(
                "\"{name}\" holds one file; it was sent twice"
            )));
        }
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let filename = field.file_name().and_then(clean_name);
        let p = stage_table_part(st, &def.name, &name, mime, filename, &mut field).await?;
        input.files.push((name, p));
    }
    if !got_data {
        return Err(ApiError::bad_request("the request has no \"data\" part"));
    }
    Ok(input)
}

/// A file as JSON in a request: `{"object": {"bucket", "key", "version"?}}` attaches an object's file; the object `{"id", "size", "mime"?, "name"?}` (as a
/// row returns it) names a file that is already stored, which is how a list keeps the files it already has.
fn file_item(db: &crate::Db, j: &J) -> crate::Result<FileValue> {
    let o = j.as_object().ok_or_else(|| {
        crate::Error::Invalid(
            "a file is {\"object\": {\"bucket\": ..., \"key\": ...}} or a file a row already holds"
                .into(),
        )
    })?;
    if let Some(obj) = o.get("object").and_then(|v| v.as_object()) {
        let s = |k: &str| obj.get(k).and_then(|v| v.as_str());
        let (Some(bucket), Some(key)) = (s("bucket"), s("key")) else {
            return Err(crate::Error::Invalid(
                "an attached object needs a \"bucket\" and a \"key\"".into(),
            ));
        };
        return db.file_value_of_object(bucket, key, s("version"));
    }
    let id = o
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(from_hex_32)
        .ok_or_else(|| {
            crate::Error::Invalid("a file needs an \"id\" (64 hex digits) or an \"object\"".into())
        })?;
    let size = o
        .get("size")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| crate::Error::Invalid("a file needs its \"size\"".into()))?;
    Ok(FileValue {
        id,
        size,
        mime: o
            .get("mime")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string(),
        name: o.get("name").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// The JSON fields and uploads as engine values: `(column, Some(value) | None for null)`, and the files staged with them.
type Converted = (Vec<(String, Option<V>)>, Vec<PendingFile>);

fn convert(
    db: &crate::Db,
    def: &TableDef,
    fields: &Map<String, J>,
    files: Vec<(String, PendingFile)>,
) -> crate::Result<Converted> {
    let mut values: Vec<(String, Option<V>)> = Vec::new();
    for (name, j) in fields {
        let col = def.column(name).ok_or_else(|| {
            crate::Error::Invalid(format!(
                "the table \"{}\" has no column \"{name}\"",
                def.name
            ))
        })?;
        if j.is_null() {
            values.push((name.clone(), None));
            continue;
        }
        let v = if col.ty == Type::File {
            V::File(file_item(db, j)?)
        } else if col.ty.is_file_column() {
            let items = j.as_array().ok_or_else(|| {
                crate::Error::Invalid(format!("column \"{name}\" holds a list of files"))
            })?;
            V::Array(
                Type::File,
                items
                    .iter()
                    .map(|i| file_item(db, i).map(V::File))
                    .collect::<crate::Result<_>>()?,
            )
        } else {
            from_json(&col.ty, j)
                .map_err(|e| crate::Error::Invalid(format!("column \"{name}\": {e}")))?
        };
        values.push((name.clone(), Some(v)));
    }
    let mut staged = Vec::new();
    for (name, p) in files {
        let col = def
            .column(&name)
            .ok_or_else(|| crate::Error::Invalid(format!("no column \"{name}\"")))?;
        let fv = V::File(p.file_value());
        if col.ty == Type::File {
            if values.iter().any(|(n, v)| *n == name && v.is_some()) {
                return Err(crate::Error::Invalid(format!(
                    "column \"{name}\" holds one file: it was given both as a value and as an upload"
                )));
            }
            values.retain(|(n, _)| *n != name);
            values.push((name, Some(fv)));
        } else {
            // a list: the files already named in the data, then the uploads in the order they came
            match values.iter_mut().find(|(n, _)| *n == name) {
                Some((_, Some(V::Array(_, items)))) => items.push(fv),
                Some(slot) => *slot = (name.clone(), Some(V::Array(Type::File, vec![fv]))),
                None => values.push((name, Some(V::Array(Type::File, vec![fv])))),
            }
        }
        staged.push(p);
    }
    Ok((values, staged))
}

pub async fn insert_row(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    req: Request,
) -> Result<(StatusCode, Json<J>), ApiError> {
    admin(&who)?; // before a byte of the body is read
    let def = table(&st, &name).await?;
    let input = read_input(&st, &def, req).await?;
    let db = st.db.clone();
    let d2 = def.clone();
    let row = blocking(move || {
        let (values, staged) = convert(&db, &d2, &input.fields, input.files)?;
        let given: Vec<(&str, V)> = values
            .iter()
            .filter_map(|(n, v)| v.clone().map(|v| (n.as_str(), v)))
            .collect();
        let refs: Vec<&PendingFile> = staged.iter().collect();
        let pk = db.insert_row_with(&d2.name, &given, &refs)?;
        db.get_row(&d2.name, &pk)?
            .ok_or_else(|| crate::Error::Corrupt("a row just inserted is not there".into()))
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "row": row_json(&def, &row) })),
    ))
}

pub async fn get_row(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, seg)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let key = parse_key(&def, &seg, &q)?;
    let db = st.db.clone();
    let row = blocking(move || db.get_row(&name, &key))
        .await?
        .ok_or_else(|| ApiError::not_found("the row"))?;
    Ok(Json(json!({ "row": row_json(&def, &row) })))
}

pub async fn update_row(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, seg)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let key = parse_key(&def, &seg, &q)?;
    let input = read_input(&st, &def, req).await?;
    let db = st.db.clone();
    let d2 = def.clone();
    let row = blocking(move || {
        let (values, staged) = convert(&db, &d2, &input.fields, input.files)?;
        let patch: Vec<(&str, Option<V>)> = values
            .iter()
            .map(|(n, v)| (n.as_str(), v.clone()))
            .collect();
        let refs: Vec<&PendingFile> = staged.iter().collect();
        db.update_row_with(&d2.name, &key, &patch, &refs)
    })
    .await?;
    Ok(Json(json!({ "row": row_json(&def, &row) })))
}

pub async fn delete_row(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, seg)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let key = parse_key(&def, &seg, &q)?;
    let db = st.db.clone();
    if blocking(move || db.delete_row(&name, &key)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("the row"))
    }
}

pub async fn download(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, seg, column)): Path<(String, String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let key = parse_key(&def, &seg, &q)?;
    let db = st.db.clone();
    let row = blocking(move || db.get_row(&name, &key))
        .await?
        .ok_or_else(|| ApiError::not_found("the row"))?;
    let value = row
        .iter()
        .find(|(n, _)| *n == column)
        .and_then(|(_, v)| v.clone())
        .ok_or_else(|| ApiError::not_found("a file in that column"))?;
    let f = match value {
        V::File(f) => f,
        V::Array(_, items) => {
            let i: usize = q
                .get("index")
                .map(|s| s.parse())
                .transpose()
                .map_err(|_| ApiError::bad_request("index must be a number"))?
                .unwrap_or(0);
            match items.get(i) {
                Some(V::File(f)) => f.clone(),
                _ => return Err(ApiError::not_found("a file at that index")),
            }
        }
        _ => {
            return Err(ApiError::bad_request(&format!(
                "column \"{column}\" is not a file column"
            )));
        }
    };
    serve_file(
        &st,
        f.id,
        &f.mime,
        f.name.as_deref(),
        format!("\"{}\"", hex(&f.id)),
        &headers,
        vec![],
    )
    .await
}

pub async fn index_rows(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, index)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<J>, ApiError> {
    admin(&who)?;
    let def = table(&st, &name).await?;
    let columns: Vec<u32> = if let Some(x) = def.indexes.iter().find(|x| x.name == index) {
        x.columns.clone()
    } else {
        match def
            .constraints
            .iter()
            .find(|k| k.name == index)
            .map(|k| &k.kind)
        {
            Some(ConstraintKind::Unique { columns, .. })
            | Some(ConstraintKind::ForeignKey { columns, .. }) => columns.clone(),
            _ => return Err(ApiError::not_found(&format!("index \"{index}\""))),
        }
    };
    let prefix: Vec<V> = match q.get("values") {
        None => vec![],
        Some(v) => {
            let arr: Vec<J> = serde_json::from_str(v).map_err(|_| {
                ApiError::bad_request("values must be a JSON array of the leading values")
            })?;
            if arr.len() > columns.len() {
                return Err(ApiError::bad_request(&format!(
                    "the index has {} column(s); {} values given",
                    columns.len(),
                    arr.len()
                )));
            }
            arr.iter()
                .zip(&columns)
                .map(|(j, id)| {
                    let c = def
                        .column_by_id(*id)
                        .ok_or_else(|| ApiError::internal("an index column is missing"))?;
                    from_json(&c.ty, j)
                        .map_err(|e| ApiError::bad_request(&format!("column \"{}\": {e}", c.name)))
                })
                .collect::<Result<_, _>>()?
        }
    };
    let limit = limit_of(&q)?;
    let after = q
        .get("after")
        .map(|a| {
            URL_SAFE_NO_PAD
                .decode(a)
                .map_err(|_| ApiError::bad_request("that is not a cursor from a previous page"))
        })
        .transpose()?;
    let db = st.db.clone();
    let page =
        blocking(move || db.scan_index(&name, &index, &prefix, after.as_deref(), limit)).await?;
    Ok(Json(json!({
        "items": page.rows.iter().map(|r| row_json(&def, r)).collect::<Vec<_>>(),
        "next": page.next.as_ref().map(|n| URL_SAFE_NO_PAD.encode(n)),
        "limit": limit,
    })))
}
