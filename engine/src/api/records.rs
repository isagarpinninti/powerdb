use super::query::{self, ListQuery};
use super::upload::{self, Upload};
use super::{ApiError, AppState, Caller, Gate, authorize, gate};
use crate::auth::{Action, Principal};
use crate::model::{CollectionDef, Record};
use crate::util::now_ms;
use axum::Json;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, crate::Error> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("task failed"))?
        .map_err(ApiError::from)
}

async fn collection(st: &AppState, name: &str) -> Result<CollectionDef, ApiError> {
    let db = st.db.clone();
    let n = name.to_string();
    blocking(move || db.collection(&n)).await
}

/// A record as the client sees it: system fields and the record's own fields side by side, and for each file its metadata plus, when the caller may view the
/// record, a link that works for `url_ttl_secs`.
pub fn render(
    st: &AppState,
    who: &Principal,
    def: &CollectionDef,
    view: &Gate,
    r: &Record,
) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(r.id));
    m.insert("collection".into(), json!(r.collection));
    m.insert("created_at".into(), json!(r.created_at));
    m.insert("updated_at".into(), json!(r.updated_at));
    for (k, v) in &r.fields {
        m.insert(k.clone(), v.clone());
    }
    if !r.files.is_empty() {
        let can_view = view.allows(who, Action::View, Some(r), None);
        let exp = now_ms() / 1000 + st.cfg.url_ttl_secs;
        for (field, f) in &r.files {
            let mut o = json!({"name": f.name, "size": f.size, "mime": f.mime});
            if can_view {
                let token = st
                    .signer
                    .sign(&format!("{}/{}/{}/{}", def.name, r.id, field, f.id), exp);
                o["url"] = json!(format!(
                    "/api/v1/files/{}/{}/{}?token={}",
                    def.name, r.id, field, token
                ));
            }
            m.insert(field.clone(), o);
        }
    }
    Value::Object(m)
}

fn view_gate(who: &Principal, def: &CollectionDef) -> Gate {
    // a caller who cannot view gets records without links; the list/get rule decides whether they see the record at all
    gate(who, def, Action::View).unwrap_or(Gate::Rule(crate::security::expr::Expr::Lit(
        Value::Bool(false),
    )))
}

pub async fn list(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let def = collection(&st, &name).await?;
    let g = gate(&who, &def, Action::List)?;
    // a rule that never looks at the record is one yes-or-no for this caller: say no now (401 for an anonymous caller) instead of returning an empty list
    if matches!(&g, Gate::Rule(e) if !e.uses_record()) {
        authorize(&who, &def, Action::List, None, None)?;
    }
    let q: ListQuery = query::parse(&params, &def)?;
    let vg = view_gate(&who, &def);
    let (st2, who2, def2) = (st.clone(), who.clone(), def.clone());
    let body = blocking(move || {
        let (limit, offset) = (q.limit, q.offset);
        let mut items: Vec<Record> = Vec::new();
        let mut complete = true;
        let mut matched = 0usize;
        let visible = |r: &Record| g.allows(&who2, Action::List, Some(r), None) && q.filter.as_ref().is_none_or(|f| f.matches(&crate::security::expr::Ctx { record: Some(r), auth: None, body: None, method: "GET" }));
        match q.fast_order() {
            Some(newest_first) => {
                st2.db.scan(&def2.name, newest_first, |r| {
                    if !visible(&r) {
                        return Ok(true);
                    }
                    matched += 1;
                    if matched > offset {
                        items.push(r);
                    }
                    if items.len() > limit {
                        complete = false;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                if items.len() > limit {
                    items.truncate(limit);
                }
            }
            None => {
                let mut all: Vec<Record> = Vec::new();
                st2.db.scan(&def2.name, true, |r| {
                    if visible(&r) {
                        all.push(r);
                        if all.len() > query::MAX_SORT_SCAN {
                            return Err(crate::Error::Invalid(format!("more than {} records match; narrow the filter or sort by created_at", query::MAX_SORT_SCAN)));
                        }
                    }
                    Ok(true)
                })?;
                all.sort_by(|a, b| query::compare(a, b, &q.sort));
                matched = all.len();
                items = all.into_iter().skip(offset).take(limit + 1).collect();
                complete = items.len() <= limit;
                items.truncate(limit);
            }
        }
        let has_more = !complete || (offset + items.len() < matched && q.fast_order().is_none());
        let rendered: Vec<Value> = items.iter().map(|r| render(&st2, &who2, &def2, &vg, r)).collect();
        Ok(json!({"items": rendered, "limit": limit, "offset": offset, "has_more": has_more, "total": if complete { json!(matched) } else { Value::Null }}))
    })
    .await?;
    Ok(Json(body))
}

pub async fn get(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let def = collection(&st, &name).await?;
    let db = st.db.clone();
    let (n, i) = (name.clone(), id.clone());
    let rec = blocking(move || db.get(&n, &i))
        .await?
        .ok_or_else(|| ApiError::not_found(&format!("record {id}")))?;
    authorize(&who, &def, Action::View, Some(&rec), None)?;
    let vg = view_gate(&who, &def);
    Ok(Json(render(&st, &who, &def, &vg, &rec)))
}

async fn read_input(
    st: &AppState,
    def: &CollectionDef,
    req: Request,
    check: impl Fn(&Map<String, Value>) -> Result<(), ApiError>,
) -> Result<Upload, ApiError> {
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.starts_with("multipart/form-data") {
        let mp = Multipart::from_request(req, st).await.map_err(|e| {
            ApiError::bad_request(&format!("bad multipart request: {}", e.body_text()))
        })?;
        return upload::read_multipart(st, def, mp, check).await;
    }
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
    let (fields, remove) = upload::split_body(upload::parse_json_object(&bytes)?)?;
    check(&fields)?;
    Ok(Upload {
        fields,
        files: vec![],
        remove,
    })
}

pub async fn create(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    req: Request,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let def = collection(&st, &name).await?;
    let g = gate(&who, &def, Action::Create)?; // refuses at once, before reading a byte, when nobody is allowed to create
    let up = read_input(&st, &def, req, |fields| {
        let body = Value::Object(fields.clone());
        if g.allows(&who, Action::Create, None, Some(&body)) {
            Ok(())
        } else {
            Err(
                super::authorize(&who, &def, Action::Create, None, Some(&body))
                    .err()
                    .unwrap_or_else(|| ApiError::forbidden("not allowed")),
            )
        }
    })
    .await?;
    let (db, n) = (st.db.clone(), name.clone());
    let rec = blocking(move || db.create(&n, up.fields, up.files)).await?;
    let vg = view_gate(&who, &def);
    Ok((
        StatusCode::CREATED,
        Json(render(&st, &who, &def, &vg, &rec)),
    ))
}

pub async fn update(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, id)): Path<(String, String)>,
    req: Request,
) -> Result<Json<Value>, ApiError> {
    let def = collection(&st, &name).await?;
    let g = gate(&who, &def, Action::Update)?;
    let db = st.db.clone();
    let (n, i) = (name.clone(), id.clone());
    let old = blocking(move || db.get(&n, &i))
        .await?
        .ok_or_else(|| ApiError::not_found(&format!("record {id}")))?;
    let up = read_input(&st, &def, req, |fields| {
        let body = Value::Object(fields.clone());
        if g.allows(&who, Action::Update, Some(&old), Some(&body)) {
            Ok(())
        } else {
            Err(
                super::authorize(&who, &def, Action::Update, Some(&old), Some(&body))
                    .err()
                    .unwrap_or_else(|| ApiError::forbidden("not allowed")),
            )
        }
    })
    .await?;
    let (db, n, i) = (st.db.clone(), name.clone(), old.id.clone());
    let rec = blocking(move || db.update(&n, &i, up.fields, up.files, &up.remove)).await?;
    let vg = view_gate(&who, &def);
    Ok(Json(render(&st, &who, &def, &vg, &rec)))
}

pub async fn delete(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((name, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let def = collection(&st, &name).await?;
    let db = st.db.clone();
    let (n, i) = (name.clone(), id.clone());
    let rec = blocking(move || db.get(&n, &i))
        .await?
        .ok_or_else(|| ApiError::not_found(&format!("record {id}")))?;
    authorize(&who, &def, Action::Delete, Some(&rec), None)?;
    let db = st.db.clone();
    blocking(move || db.delete(&name, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
