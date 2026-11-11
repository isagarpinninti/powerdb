//! The frozen `/api/v1` contract.
//!
//! Every route of the public API is driven once, in a realistic order, and for each call the test pins down the status code and the *shape*
//! of the JSON answer (field names and types, not values). Changing any of them breaks this test, which is the point: an existing client
//! must keep working inside `/api/v1`. Adding a new field to an answer is allowed and does not break it (shapes are compared as "at least
//! these fields"); removing a field, renaming it, changing its type or changing a status code is a breaking change and needs `/api/v2`
//! (see docs/COMPATIBILITY.md).
//!
//! `POWERDB_CONTRACT_PRINT=1 cargo test --test contract -- --nocapture` prints what the server does now, for writing a new entry.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt;
use powerdb::api::{self, Config};
use powerdb::server;
use powerdb::util::new_id;
use powerdb::{Durability, Options};
use serde_json::Value;
use std::collections::BTreeMap;
use tower::ServiceExt;

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Res {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

struct Ctx {
    app: Router,
    printing: bool,
}

impl Ctx {
    async fn call(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        ct: Option<&str>,
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Res {
        let mut b = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).unwrap())
            .uri(format!("/api/v1{uri}"));
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(c) = ct {
            b = b.header(header::CONTENT_TYPE, c);
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let r = self
            .app
            .clone()
            .oneshot(b.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let (p, body) = r.into_parts();
        Res {
            status: p.status.as_u16(),
            headers: p.headers,
            body: body.collect().await.unwrap().to_bytes().to_vec(),
        }
    }

    async fn json(&self, method: &str, uri: &str, token: Option<&str>, v: Value) -> Res {
        self.call(
            method,
            uri,
            token,
            Some("application/json"),
            v.to_string().into_bytes(),
            &[],
        )
        .await
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> Res {
        self.call("GET", uri, token, None, vec![], &[]).await
    }

    /// Pins one call: the status, and the fields (name and type) the answer must contain.
    fn pin(&mut self, name: &str, r: &Res, status: u16, shape: &str) {
        let got = shape_of(&r.body);
        if self.printing {
            println!("{name:<38} {:>3}  {got}", r.status);
            return;
        }
        assert_eq!(r.status, status, "{name}: the status code changed");
        if shape.is_empty() {
            return;
        }
        let want = parse_shape(shape);
        let have = parse_shape(&got);
        for (k, t) in &want {
            match have.get(k) {
                None => panic!("{name}: the field \"{k}\" is gone from the answer (now: {got})"),
                Some(h) => assert_eq!(h, t, "{name}: the field \"{k}\" changed type"),
            }
        }
    }
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The fields of an answer as `path:type`, comma separated, one nesting level deep: `error.message:string`, `items[].id:string`.
/// A `null` is left out (a field that is sometimes a number and sometimes null is not a contract).
fn shape_of(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return String::from("(not json)");
    };
    fn walk(prefix: &str, v: &Value, depth: u8, out: &mut Vec<String>) {
        if let Value::Object(m) = v {
            for (k, v) in m {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match v {
                    Value::Null => {}
                    Value::Object(_) => {
                        out.push(format!("{path}:object"));
                        if depth > 0 {
                            walk(&path, v, depth - 1, out);
                        }
                    }
                    Value::Array(a) => {
                        out.push(format!("{path}:array"));
                        if depth > 0
                            && let Some(first @ Value::Object(_)) = a.first()
                        {
                            walk(&format!("{path}[]"), first, depth - 1, out);
                        }
                    }
                    other => out.push(format!("{path}:{}", type_of(other))),
                }
            }
        }
    }
    let mut out = Vec::new();
    walk("", &v, 1, &mut out);
    out.join(",")
}

fn parse_shape(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once(':') {
            Some((k, t)) => (k.to_string(), t.to_string()),
            None => (p.to_string(), String::new()),
        })
        .collect()
}

type Part<'a> = (&'a str, Option<(&'a str, &'a str)>, &'a [u8]);

fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let boundary = "----contract1234567890";
    let mut out = Vec::new();
    for (name, file, data) in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file {
            Some((fname, ct)) => out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\nContent-Type: {ct}\r\n\r\n").as_bytes(),
            ),
            None => out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
            ),
        }
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), out)
}

fn id_of(r: &Res) -> String {
    serde_json::from_slice::<Value>(&r.body).unwrap()["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {}", String::from_utf8_lossy(&r.body)))
        .to_string()
}

#[tokio::test]
async fn the_v1_contract_is_unchanged() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-contract-{}", new_id())));
    let (st, boot) = server::build_state(
        &dir.0,
        Options {
            durability: Durability::Never,
            ..Options::default()
        },
        Config {
            login_attempts_per_minute: 1000,
            ..Config::default()
        },
    )
    .unwrap();
    let admin = boot
        .admin_key
        .expect("the first run mints an administrator key");
    let admin = admin.as_str();
    let mut c = Ctx {
        app: api::router(st),
        printing: std::env::var("POWERDB_CONTRACT_PRINT").is_ok(),
    };

    // ---- public
    let r = c.get("/healthz", None).await;
    c.pin("GET /healthz", &r, 200, "");
    assert_eq!(r.body, b"ok");
    let r = c.get("/auth/oauth/providers", None).await;
    c.pin("GET /auth/oauth/providers", &r, 200, "providers:array");
    let r = c.get("/collections", None).await;
    c.pin(
        "GET /collections (no credentials)",
        &r,
        401,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/collections", Some("pdb_live_notakey")).await;
    c.pin(
        "GET /collections (bad key)",
        &r,
        401,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/nope", Some(admin)).await;
    c.pin(
        "GET /nope",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.call("DELETE", "/healthz", None, None, vec![], &[]).await;
    c.pin("DELETE /healthz", &r, 405, "");

    // ---- collections
    let schema = serde_json::json!({"name": "articles", "fields": [
        {"name": "title", "kind": "text", "required": true},
        {"name": "views", "kind": "number"},
        {"name": "cover", "kind": "file"}]});
    let r = c
        .json("POST", "/collections", Some(admin), schema.clone())
        .await;
    c.pin("POST /collections", &r, 201, "collection:object,collection.created_at:number,collection.encrypt:bool,collection.fields:array,collection.name:string,collection.rules:object");
    let r = c.json("POST", "/collections", Some(admin), schema).await;
    c.pin(
        "POST /collections (duplicate)",
        &r,
        409,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "POST",
            "/collections",
            Some(admin),
            serde_json::json!({"name": "Bad Name", "fields": []}),
        )
        .await;
    c.pin(
        "POST /collections (bad name)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/collections", Some(admin)).await;
    c.pin("GET /collections", &r, 200, "items:array,items[].created_at:number,items[].encrypt:bool,items[].fields:array,items[].name:string,items[].rules:object");
    let r = c.get("/collections/articles", Some(admin)).await;
    c.pin("GET /collections/{name}", &r, 200, "collection:object,collection.created_at:number,collection.encrypt:bool,collection.fields:array,collection.name:string,collection.rules:object,records:number");
    let r = c.get("/collections/nope", Some(admin)).await;
    c.pin(
        "GET /collections/{name} (missing)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "PATCH",
            "/collections/articles",
            Some(admin),
            serde_json::json!({"fields": [
        {"name": "title", "kind": "text", "required": true},
        {"name": "views", "kind": "number"},
        {"name": "cover", "kind": "file"}],
        "rules": {"list": "@request.auth.id != \"\"", "view": "@request.auth.id != \"\""}}),
        )
        .await;
    c.pin("PATCH /collections/{name}", &r, 200, "collection:object,collection.created_at:number,collection.encrypt:bool,collection.fields:array,collection.name:string,collection.rules:object");
    let r = c
        .json(
            "PATCH",
            "/collections/articles",
            Some(admin),
            serde_json::json!({"rules": {}}),
        )
        .await;
    c.pin(
        "PATCH /collections/{name} (no fields)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );

    // ---- records
    let r = c
        .json(
            "POST",
            "/collections/articles/records",
            Some(admin),
            serde_json::json!({"title": "plain", "views": 3}),
        )
        .await;
    c.pin(
        "POST records (json)",
        &r,
        201,
        "collection:string,created_at:number,id:string,title:string,updated_at:number,views:number",
    );
    let plain = id_of(&r);
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0\x90wS\xde".to_vec();
    let (ct, body) = multipart(&[
        ("data", None, br#"{"title":"with file","views":5}"#),
        ("cover", Some(("c.png", "image/png")), &png),
    ]);
    let r = c
        .call(
            "POST",
            "/collections/articles/records",
            Some(admin),
            Some(&ct),
            body,
            &[],
        )
        .await;
    c.pin("POST records (multipart)", &r, 201, "collection:string,cover:object,cover.mime:string,cover.name:string,cover.size:number,cover.url:string,created_at:number,id:string,title:string,updated_at:number,views:number");
    let with_file = id_of(&r);
    let r = c
        .json(
            "POST",
            "/collections/articles/records",
            Some(admin),
            serde_json::json!({"views": 1}),
        )
        .await;
    c.pin(
        "POST records (required missing)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/collections/articles/records", Some(admin)).await;
    c.pin("GET records", &r, 200, "has_more:bool,items:array,items[].collection:string,items[].cover:object,items[].created_at:number,items[].id:string,items[].title:string,items[].updated_at:number,items[].views:number,limit:number,offset:number,total:number");
    let r = c
        .get(
            "/collections/articles/records?limit=1&sort=-created_at&filter=views%20%3E%201",
            Some(admin),
        )
        .await;
    c.pin("GET records (limit, sort, filter)", &r, 200, "has_more:bool,items:array,items[].collection:string,items[].cover:object,items[].created_at:number,items[].id:string,items[].title:string,items[].updated_at:number,items[].views:number,limit:number,offset:number");
    let r = c
        .get(
            "/collections/articles/records?filter=nonsense%20%3E",
            Some(admin),
        )
        .await;
    c.pin(
        "GET records (bad filter)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .get(
            &format!("/collections/articles/records/{plain}"),
            Some(admin),
        )
        .await;
    c.pin(
        "GET record",
        &r,
        200,
        "collection:string,created_at:number,id:string,title:string,updated_at:number,views:number",
    );
    let r = c
        .get("/collections/articles/records/nope", Some(admin))
        .await;
    c.pin(
        "GET record (missing)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "PATCH",
            &format!("/collections/articles/records/{plain}"),
            Some(admin),
            serde_json::json!({"views": 9}),
        )
        .await;
    c.pin(
        "PATCH record",
        &r,
        200,
        "collection:string,created_at:number,id:string,title:string,updated_at:number,views:number",
    );

    // ---- files
    let r = c
        .get(&format!("/files/articles/{with_file}/cover"), Some(admin))
        .await;
    c.pin("GET file", &r, 200, "");
    assert_eq!(r.body, png, "the file comes back byte for byte");
    assert!(
        r.headers.contains_key(header::CONTENT_TYPE),
        "a file has a content type"
    );
    let r = c
        .call(
            "GET",
            &format!("/files/articles/{with_file}/cover"),
            Some(admin),
            None,
            vec![],
            &[("range", "bytes=0-3")],
        )
        .await;
    c.pin("GET file (range)", &r, 206, "");
    assert_eq!(r.body, &png[..4]);
    let r = c
        .get(&format!("/files/articles/{plain}/cover"), Some(admin))
        .await;
    c.pin(
        "GET file (none attached)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );

    // ---- keys
    let r = c
        .json(
            "POST",
            "/keys",
            Some(admin),
            serde_json::json!({"name": "reader", "access": "read", "collections": ["articles"]}),
        )
        .await;
    c.pin("POST /keys", &r, 201, "key:string,meta:object,meta.access:string,meta.admin:bool,meta.collections:array,meta.created_at:number,meta.id:string,meta.name:string,note:string");
    let key_id = serde_json::from_slice::<Value>(&r.body).unwrap()["meta"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = serde_json::from_slice::<Value>(&r.body).unwrap()["key"]
        .as_str()
        .map(str::to_string);
    let r = c.get("/keys", Some(admin)).await;
    c.pin("GET /keys", &r, 200, "items:array,items[].access:string,items[].admin:bool,items[].collections:array,items[].created_at:number,items[].id:string,items[].name:string");
    if let Some(secret) = secret {
        let r = c.get("/collections/articles/records", Some(&secret)).await;
        c.pin("GET records (scoped key)", &r, 200, "has_more:bool,items:array,items[].collection:string,items[].cover:object,items[].created_at:number,items[].id:string,items[].title:string,items[].updated_at:number,items[].views:number,limit:number,offset:number,total:number");
        let r = c
            .json(
                "POST",
                "/collections/articles/records",
                Some(&secret),
                serde_json::json!({"title": "x"}),
            )
            .await;
        c.pin(
            "POST records (read-only key)",
            &r,
            403,
            "error:object,error.code:string,error.message:string",
        );
        let r = c.get("/keys", Some(&secret)).await;
        c.pin(
            "GET /keys (non-admin key)",
            &r,
            403,
            "error:object,error.code:string,error.message:string",
        );
    }
    let r = c
        .call(
            "DELETE",
            &format!("/keys/{key_id}"),
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("DELETE /keys/{id}", &r, 204, "");

    // ---- users and sessions
    let r = c
        .json(
            "POST",
            "/users",
            Some(admin),
            serde_json::json!({"email": "a@example.com", "password": "correct horse battery"}),
        )
        .await;
    c.pin(
        "POST /users",
        &r,
        201,
        "user:object,user.created_at:number,user.email:string,user.id:string,user.role:string",
    );
    let user_id = serde_json::from_slice::<Value>(&r.body).unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c.get("/users", Some(admin)).await;
    c.pin("GET /users", &r, 200, "items:array,items[].created_at:number,items[].email:string,items[].id:string,items[].role:string");
    let r = c
        .json(
            "POST",
            "/auth/login",
            None,
            serde_json::json!({"email": "a@example.com", "password": "correct horse battery"}),
        )
        .await;
    c.pin("POST /auth/login", &r, 200, "expires_in:number,token:string,user:object,user.created_at:number,user.email:string,user.id:string,user.role:string");
    let token = serde_json::from_slice::<Value>(&r.body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .json(
            "POST",
            "/auth/login",
            None,
            serde_json::json!({"email": "a@example.com", "password": "wrong"}),
        )
        .await;
    c.pin(
        "POST /auth/login (wrong password)",
        &r,
        401,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/auth/me", Some(&token)).await;
    c.pin("GET /auth/me", &r, 200, "kind:string,user:object,user.created_at:number,user.email:string,user.id:string,user.role:string");
    let r = c
        .call("POST", "/auth/refresh", Some(&token), None, vec![], &[])
        .await;
    c.pin(
        "POST /auth/refresh",
        &r,
        200,
        "expires_in:number,token:string",
    );
    let r = c.json("POST", "/auth/password", Some(&token), serde_json::json!({"current_password": "correct horse battery", "new_password": "another long passphrase"})).await;
    c.pin("POST /auth/password", &r, 200, "note:string,ok:bool");
    let r = c
        .json(
            "POST",
            "/auth/register",
            None,
            serde_json::json!({"email": "b@example.com", "password": "correct horse battery"}),
        )
        .await;
    c.pin(
        "POST /auth/register (closed)",
        &r,
        403,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .call(
            "DELETE",
            &format!("/users/{user_id}"),
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("DELETE /users/{id}", &r, 204, "");

    // ---- operations
    let r = c.get("/stats", Some(admin)).await;
    c.pin(
        "GET /stats",
        &r,
        200,
        "dead_bytes:number,gc_pending:number,log_bytes:number,segments:number",
    );
    let r = c.get("/metrics", Some(admin)).await;
    c.pin("GET /metrics", &r, 200, "");
    let r = c
        .call("POST", "/maintenance/gc", Some(admin), None, vec![], &[])
        .await;
    c.pin(
        "POST /maintenance/gc",
        &r,
        200,
        "bytes_reclaimed:number,files_released:number,segments_compacted:number",
    );
    let r = c
        .json(
            "POST",
            "/maintenance/backups",
            Some(admin),
            serde_json::json!({"name": "contract"}),
        )
        .await;
    c.pin(
        "POST /maintenance/backups",
        &r,
        201,
        "bytes:number,files:number,includes_secret:bool,name:string",
    );
    let r = c.get("/maintenance/backups", Some(admin)).await;
    c.pin("GET /maintenance/backups", &r, 200, "backups:array,backups[].bytes:number,backups[].created_at:number,backups[].files:number,backups[].includes_secret:bool,backups[].name:string,backups[].powerdb_version:string");

    // ---- deletes
    let r = c
        .call(
            "DELETE",
            &format!("/collections/articles/records/{plain}"),
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("DELETE record", &r, 204, "");
    let r = c
        .call(
            "DELETE",
            &format!("/collections/articles/records/{plain}"),
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin(
        "DELETE record (again)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .call(
            "DELETE",
            "/collections/articles",
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin(
        "DELETE /collections/{name}",
        &r,
        200,
        "deleted_records:number",
    );
}

#[tokio::test]
async fn the_bucket_and_object_routes_are_unchanged() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-contract-{}", new_id())));
    let (st, boot) = server::build_state(
        &dir.0,
        Options {
            durability: Durability::Never,
            ..Options::default()
        },
        Config::default(),
    )
    .unwrap();
    let admin = boot
        .admin_key
        .expect("the first run mints an administrator key");
    let admin = admin.as_str();
    let mut c = Ctx {
        app: api::router(st),
        printing: std::env::var("POWERDB_CONTRACT_PRINT").is_ok(),
    };
    let r = c.get("/buckets", None).await;
    c.pin(
        "GET /buckets (no credentials)",
        &r,
        401,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "POST",
            "/buckets",
            Some(admin),
            serde_json::json!({"name": "docs"}),
        )
        .await;
    c.pin("POST /buckets", &r, 201, "bucket:object,bucket.created_at:number,bucket.encrypt:bool,bucket.name:string,bucket.versioning:bool");
    let r = c
        .json(
            "POST",
            "/buckets",
            Some(admin),
            serde_json::json!({"name": "docs"}),
        )
        .await;
    c.pin(
        "POST /buckets (duplicate)",
        &r,
        409,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "POST",
            "/buckets",
            Some(admin),
            serde_json::json!({"name": "Bad Name"}),
        )
        .await;
    c.pin(
        "POST /buckets (bad name)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/buckets", Some(admin)).await;
    c.pin("GET /buckets", &r, 200, "items:array,items[].created_at:number,items[].encrypt:bool,items[].name:string,items[].versioning:bool");
    let r = c.get("/buckets/docs", Some(admin)).await;
    c.pin("GET /buckets/{name}", &r, 200, "bucket:object,bucket.created_at:number,bucket.encrypt:bool,bucket.name:string,bucket.versioning:bool");
    let r = c.get("/buckets/nope", Some(admin)).await;
    c.pin(
        "GET /buckets/{name} (missing)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "PATCH",
            "/buckets/docs",
            Some(admin),
            serde_json::json!({"versioning": true}),
        )
        .await;
    c.pin("PATCH /buckets/{name}", &r, 200, "bucket:object,bucket.created_at:number,bucket.encrypt:bool,bucket.name:string,bucket.versioning:bool");
    let r = c
        .json(
            "PATCH",
            "/buckets/docs",
            Some(admin),
            serde_json::json!({"versioning": false}),
        )
        .await;
    c.pin(
        "PATCH /buckets/{name} (versioning off)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );

    let r = c
        .call(
            "PUT",
            "/buckets/docs/objects/a/b.txt?filename=b.txt",
            Some(admin),
            Some("text/plain"),
            b"hello".to_vec(),
            &[("x-powerdb-meta-note", "hi")],
        )
        .await;
    c.pin("PUT object", &r, 200, "object:object,object.bucket:string,object.checksum:string,object.created_at:number,object.delete_marker:bool,object.file_id:string,object.filename:string,object.id:string,object.key:string,object.metadata:object,object.mime:string,object.size:number,object.version_id:string");
    let first = serde_json::from_slice::<Value>(&r.body).unwrap()["object"]["version_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .call(
            "PUT",
            "/buckets/docs/objects/a/b.txt",
            Some(admin),
            Some("text/plain"),
            b"hello again".to_vec(),
            &[],
        )
        .await;
    c.pin("PUT object (new version)", &r, 200, "object:object,object.bucket:string,object.checksum:string,object.created_at:number,object.delete_marker:bool,object.file_id:string,object.id:string,object.key:string,object.metadata:object,object.mime:string,object.size:number,object.version_id:string");
    let r = c
        .call(
            "PUT",
            "/buckets/docs/objects/..%2Fx",
            Some(admin),
            Some("text/plain"),
            b"x".to_vec(),
            &[],
        )
        .await;
    c.pin(
        "PUT object (bad key)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .call(
            "PUT",
            "/buckets/nope/objects/k",
            Some(admin),
            Some("text/plain"),
            b"x".to_vec(),
            &[],
        )
        .await;
    c.pin(
        "PUT object (missing bucket)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/buckets/docs/objects/a/b.txt", Some(admin)).await;
    c.pin("GET object", &r, 200, "");
    assert_eq!(r.body, b"hello again");
    for h in [
        "etag",
        "content-type",
        "content-length",
        "accept-ranges",
        "x-powerdb-version-id",
        "x-powerdb-object-id",
        "x-powerdb-meta-note",
    ] {
        if h != "x-powerdb-meta-note" {
            assert!(
                r.headers.contains_key(h),
                "GET object: the {h} header is gone"
            );
        }
    }
    let r = c
        .call(
            "GET",
            &format!("/buckets/docs/objects/a/b.txt?version={first}"),
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("GET object (version)", &r, 200, "");
    assert_eq!(r.body, b"hello");
    assert_eq!(
        r.headers["x-powerdb-meta-note"], "hi",
        "metadata comes back as X-PowerDB-Meta-* headers"
    );
    let r = c
        .call(
            "GET",
            "/buckets/docs/objects/a/b.txt",
            Some(admin),
            None,
            vec![],
            &[("range", "bytes=0-1")],
        )
        .await;
    c.pin("GET object (range)", &r, 206, "");
    let r = c
        .call(
            "HEAD",
            "/buckets/docs/objects/a/b.txt",
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("HEAD object", &r, 200, "");
    assert!(r.body.is_empty());
    let r = c.get("/buckets/docs/objects/missing", Some(admin)).await;
    c.pin(
        "GET object (missing)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c.get("/buckets/docs/objects", Some(admin)).await;
    c.pin("GET objects", &r, 200, "items:array,items[].bucket:string,items[].checksum:string,items[].created_at:number,items[].delete_marker:bool,items[].file_id:string,items[].id:string,items[].key:string,items[].metadata:object,items[].mime:string,items[].size:number,items[].version_id:string,limit:number");
    let r = c
        .get("/buckets/docs/objects?prefix=a/&limit=1", Some(admin))
        .await;
    c.pin("GET objects (prefix, limit)", &r, 200, "items:array,items[].bucket:string,items[].checksum:string,items[].created_at:number,items[].delete_marker:bool,items[].file_id:string,items[].id:string,items[].key:string,items[].metadata:object,items[].mime:string,items[].size:number,items[].version_id:string,limit:number");
    let r = c.get("/buckets/docs/objects?limit=0", Some(admin)).await;
    c.pin(
        "GET objects (bad limit)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .get("/buckets/docs/versions?key=a/b.txt", Some(admin))
        .await;
    c.pin("GET versions", &r, 200, "items:array,items[].bucket:string,items[].checksum:string,items[].created_at:number,items[].delete_marker:bool,items[].file_id:string,items[].id:string,items[].key:string,items[].metadata:object,items[].mime:string,items[].size:number,items[].version_id:string");
    let r = c.get("/buckets/docs/versions", Some(admin)).await;
    c.pin(
        "GET versions (no key)",
        &r,
        400,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .json(
            "POST",
            "/buckets/docs/restore",
            Some(admin),
            serde_json::json!({"key": "a/b.txt", "version": first}),
        )
        .await;
    c.pin("POST restore", &r, 200, "object:object,object.bucket:string,object.checksum:string,object.created_at:number,object.delete_marker:bool,object.file_id:string,object.filename:string,object.id:string,object.key:string,object.metadata:object,object.mime:string,object.size:number,object.version_id:string");
    let r = c
        .json(
            "POST",
            "/buckets/docs/restore",
            Some(admin),
            serde_json::json!({"key": "a/b.txt", "version": "nope"}),
        )
        .await;
    c.pin(
        "POST restore (missing version)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .call(
            "DELETE",
            "/buckets/docs/objects/a/b.txt",
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin("DELETE object", &r, 204, "");
    let r = c
        .call(
            "DELETE",
            "/buckets/docs/objects/a/b.txt",
            Some(admin),
            None,
            vec![],
            &[],
        )
        .await;
    c.pin(
        "DELETE object (again)",
        &r,
        404,
        "error:object,error.code:string,error.message:string",
    );
    let r = c
        .call("DELETE", "/buckets/docs", Some(admin), None, vec![], &[])
        .await;
    c.pin(
        "DELETE /buckets/{name} (not empty)",
        &r,
        409,
        "error:object,error.code:string,error.message:string",
    );
    let vs = serde_json::from_slice::<Value>(
        &c.get("/buckets/docs/versions?key=a/b.txt", Some(admin))
            .await
            .body,
    )
    .unwrap();
    for v in vs["items"].as_array().unwrap() {
        let id = v["version_id"].as_str().unwrap();
        let r = c
            .call(
                "DELETE",
                &format!("/buckets/docs/objects/a/b.txt?version={id}"),
                Some(admin),
                None,
                vec![],
                &[],
            )
            .await;
        c.pin("DELETE object (version)", &r, 204, "");
    }
    let r = c
        .call("DELETE", "/buckets/docs", Some(admin), None, vec![], &[])
        .await;
    c.pin("DELETE /buckets/{name}", &r, 204, "");
}
