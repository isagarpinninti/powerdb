//! Buckets and objects over HTTP: who may use them, uploads and downloads (ranges, validators), versions, listing, and the errors.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt;
use powerdb::api::{self, Config};
use powerdb::server;
use powerdb::util::new_id;
use powerdb::{Durability, Options};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct App {
    _dir: Dir,
    app: Router,
    admin: String,
}

struct Res {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}
impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not json ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("no {name} header"))
            .to_str()
            .unwrap()
    }
}

fn app() -> App {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-objapi-{}", new_id())));
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
    App {
        app: api::router(st),
        admin: boot.admin_key.unwrap(),
        _dir: dir,
    }
}

async fn call(
    a: &App,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Res {
    let mut b = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(format!("/api/v1{uri}"));
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let r = a
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

async fn json_call(a: &App, method: &str, uri: &str, token: Option<&str>, v: Value) -> Res {
    call(
        a,
        method,
        uri,
        token,
        &[("content-type", "application/json")],
        v.to_string().into_bytes(),
    )
    .await
}

fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

type Part<'a> = (&'a str, Option<(&'a str, &'a str)>, Vec<u8>);

fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let b = "XBOUNDARYX";
    let mut out = Vec::new();
    for (name, file, body) in parts {
        out.extend_from_slice(format!("--{b}\r\n").as_bytes());
        match file {
            Some((fname, mime)) => out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
            ),
            None => out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
        }
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{b}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={b}"), out)
}

async fn make(a: &App, spec: Value) -> Res {
    json_call(a, "POST", "/tables", Some(&a.admin), spec).await
}

fn people() -> Value {
    json!({
        "name": "people",
        "columns": [
            {"name": "id", "type": "uuid", "generate": "uuid"},
            {"name": "name", "type": "text", "nullable": false},
            {"name": "age", "type": "int32", "nullable": true},
            {"name": "balance", "type": "decimal", "nullable": true},
            {"name": "born", "type": "date", "nullable": true},
            {"name": "photo", "type": "file", "nullable": true}
        ],
        "primary_key": ["id"],
        "constraints": [{"name": "adult", "kind": "check", "expr": "age == null || age >= 18"}],
        "indexes": [{"name": "by_name", "columns": ["name"]}]
    })
}

#[tokio::test]
async fn only_administrators_can_use_tables() {
    let a = app();
    assert_eq!(
        call(&a, "GET", "/tables", None, &[], vec![]).await.status,
        401
    );
    let k = json_call(
        &a,
        "POST",
        "/keys",
        Some(&a.admin),
        json!({"name": "rw", "access": "read_write"}),
    )
    .await;
    let secret = k.json()["key"].as_str().unwrap().to_string();
    for (m, uri) in [
        ("GET", "/tables"),
        ("POST", "/tables"),
        ("GET", "/tables/x"),
        ("PATCH", "/tables/x"),
        ("DELETE", "/tables/x"),
        ("GET", "/tables/x/rows"),
        ("POST", "/tables/x/rows"),
        ("GET", "/tables/x/rows/1"),
        ("PATCH", "/tables/x/rows/1"),
        ("DELETE", "/tables/x/rows/1"),
        ("GET", "/tables/x/rows/1/files/c"),
        ("GET", "/tables/x/indexes/i/rows"),
    ] {
        let r = call(
            &a,
            m,
            uri,
            Some(&secret),
            &[("content-type", "application/json")],
            b"{}".to_vec(),
        )
        .await;
        assert_eq!(
            r.status, 403,
            "{m} {uri} must be refused to a non-administrator key"
        );
    }
}

#[tokio::test]
async fn create_describe_alter_and_drop() {
    let a = app();
    let r = make(&a, people()).await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let t = &r.json()["table"];
    assert_eq!(t["primary_key"], json!(["id"]));
    assert_eq!(
        t["columns"][0]["nullable"],
        json!(false),
        "a primary key column is NOT NULL by default"
    );
    assert_eq!(t["constraints"][0]["kind"], "check");
    assert_eq!(t["indexes"][0]["name"], "by_name");
    // twice is a conflict
    assert_eq!(make(&a, people()).await.status, 409);
    // a bad constraint leaves nothing behind
    let mut bad = people();
    bad["name"] = json!("other");
    bad["constraints"] = json!([{"name": "c", "kind": "check", "expr": "nope nope"}]);
    let r = make(&a, bad).await;
    assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(
        call(&a, "GET", "/tables/other", Some(&a.admin), &[], vec![])
            .await
            .status,
        404
    );
    let l = call(&a, "GET", "/tables", Some(&a.admin), &[], vec![])
        .await
        .json();
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    // alter
    let r = json_call(
        &a,
        "PATCH",
        "/tables/people",
        Some(&a.admin),
        json!({"op": "add_column", "name": "nick", "type": "text"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let r = json_call(
        &a,
        "PATCH",
        "/tables/people",
        Some(&a.admin),
        json!({"op": "teleport"}),
    )
    .await;
    assert_eq!(r.status, 400);
    let r = json_call(
        &a,
        "PATCH",
        "/tables/people",
        Some(&a.admin),
        json!({"op": "drop_index", "name": "by_name"}),
    )
    .await;
    assert_eq!(r.status, 200);
    let g = call(&a, "GET", "/tables/people", Some(&a.admin), &[], vec![])
        .await
        .json();
    assert_eq!(g["rows"], 0);
    assert_eq!(g["table"]["indexes"], json!([]));
    // drop
    let d = call(&a, "DELETE", "/tables/people", Some(&a.admin), &[], vec![]).await;
    assert_eq!(d.status, 200);
    assert_eq!(d.json()["deleted_rows"], 0);
    assert_eq!(
        call(&a, "GET", "/tables/people", Some(&a.admin), &[], vec![])
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn rows_crud_with_typed_values_and_errors() {
    let a = app();
    assert_eq!(make(&a, people()).await.status, 201);
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "Asha", "age": 30, "balance": "12.50", "born": "1994-05-06"}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let row = r.json()["row"].clone();
    let id = row["id"].as_str().unwrap().to_string();
    assert_eq!(row["balance"], "12.50");
    assert_eq!(row["born"], "1994-05-06");
    assert_eq!(row["photo"], Value::Null);
    // read
    let g = call(
        &a,
        "GET",
        &format!("/tables/people/rows/{id}"),
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    assert_eq!(g.status, 200);
    assert_eq!(g.json()["row"], row);
    // update (null clears a nullable column)
    let u = json_call(
        &a,
        "PATCH",
        &format!("/tables/people/rows/{id}"),
        Some(&a.admin),
        json!({"age": null, "name": "Asha K"}),
    )
    .await;
    assert_eq!(u.status, 200, "{}", String::from_utf8_lossy(&u.body));
    assert_eq!(u.json()["row"]["age"], Value::Null);
    assert_eq!(u.json()["row"]["name"], "Asha K");
    // errors
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "Kid", "age": 5}),
    )
    .await;
    assert_eq!(r.status, 400, "a CHECK violation is a validation error");
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"age": 40}),
    )
    .await;
    assert_eq!(r.status, 400, "a required column is missing");
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "x", "nope": 1}),
    )
    .await;
    assert_eq!(r.status, 400, "an unknown column");
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "x", "age": "old"}),
    )
    .await;
    assert_eq!(r.status, 400, "a wrong type");
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "x", "photo": {"id": "zz", "size": 1}}),
    )
    .await;
    assert_eq!(r.status, 400);
    let r = call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        &[("content-type", "application/json")],
        b"[1]".to_vec(),
    )
    .await;
    assert_eq!(r.status, 400);
    let r = call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        &[("content-type", "text/plain")],
        b"x".to_vec(),
    )
    .await;
    assert_eq!(r.status, 415);
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/people/rows/not-a-uuid",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400
    );
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/people/rows/018f0000-0000-7000-8000-000000000000",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        404
    );
    assert_eq!(
        call(&a, "GET", "/tables/nope/rows", Some(&a.admin), &[], vec![])
            .await
            .status,
        404
    );
    // delete
    assert_eq!(
        call(
            &a,
            "DELETE",
            &format!("/tables/people/rows/{id}"),
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        204
    );
    assert_eq!(
        call(
            &a,
            "DELETE",
            &format!("/tables/people/rows/{id}"),
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn pagination_composite_keys_and_index_lookups() {
    let a = app();
    let spec = json!({
        "name": "events",
        "columns": [
            {"name": "org", "type": "text"},
            {"name": "n", "type": "int64"},
            {"name": "kind", "type": "text"}
        ],
        "primary_key": ["org", "n"],
        "indexes": [{"name": "by_kind", "columns": ["kind"]}]
    });
    assert_eq!(make(&a, spec).await.status, 201);
    for i in 0..25 {
        let r = json_call(&a, "POST", "/tables/events/rows", Some(&a.admin), json!({"org": if i % 2 == 0 {"a"} else {"b"}, "n": i, "kind": if i % 5 == 0 {"five"} else {"other"}})).await;
        assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    }
    // page through in key order
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let uri = match &after {
            Some(c) => format!("/tables/events/rows?limit=10&after={c}"),
            None => "/tables/events/rows?limit=10".to_string(),
        };
        let p = call(&a, "GET", &uri, Some(&a.admin), &[], vec![])
            .await
            .json();
        for it in p["items"].as_array().unwrap() {
            seen.push((
                it["org"].as_str().unwrap().to_string(),
                it["n"].as_str().unwrap().to_string(),
            ));
        }
        match p["next"].as_str() {
            Some(c) => after = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 25);
    let mut sorted = seen.clone();
    sorted.sort_by(|x, y| {
        x.0.cmp(&y.0).then(
            x.1.parse::<i64>()
                .unwrap()
                .cmp(&y.1.parse::<i64>().unwrap()),
        )
    });
    assert_eq!(seen, sorted, "rows come back in primary key order");
    // a composite key is addressed with ?key=
    let g = call(
        &a,
        "GET",
        "/tables/events/rows/_?key=%5B%22a%22%2C4%5D",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    assert_eq!(g.status, 200, "{}", String::from_utf8_lossy(&g.body));
    assert_eq!(g.json()["row"]["n"], "4");
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/events/rows/a",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400
    );
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/events/rows?limit=0",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400
    );
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/events/rows?after=%21%21",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400
    );
    // index lookups, paged
    let mut found = 0;
    let mut after: Option<String> = None;
    loop {
        let uri = match &after {
            Some(c) => format!(
                "/tables/events/indexes/by_kind/rows?values=%5B%22five%22%5D&limit=2&after={c}"
            ),
            None => {
                "/tables/events/indexes/by_kind/rows?values=%5B%22five%22%5D&limit=2".to_string()
            }
        };
        let r = call(&a, "GET", &uri, Some(&a.admin), &[], vec![]).await;
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let p = r.json();
        found += p["items"].as_array().unwrap().len();
        match p["next"].as_str() {
            Some(c) => after = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(found, 5);
    assert_eq!(
        call(
            &a,
            "GET",
            "/tables/events/indexes/nope/rows",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn files_upload_download_and_attach_an_object() {
    let a = app();
    assert_eq!(make(&a, people()).await.status, 201);
    let mut png = data(70_000, 7);
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let (ct, body) = multipart(&[
        ("data", None, br#"{"name": "Pic"}"#.to_vec()),
        ("photo", Some(("me.png", "image/png")), png.clone()),
    ]);
    let r = call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        &[("content-type", &ct)],
        body,
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let row = r.json()["row"].clone();
    assert_eq!(row["photo"]["size"], 70_000);
    assert_eq!(row["photo"]["mime"], "image/png");
    assert_eq!(row["photo"]["name"], "me.png");
    let url = row["photo"]["url"]
        .as_str()
        .unwrap()
        .strip_prefix("/api/v1")
        .unwrap()
        .to_string();
    let d = call(&a, "GET", &url, Some(&a.admin), &[], vec![]).await;
    assert_eq!(d.status, 200);
    assert_eq!(d.body, png);
    assert_eq!(d.header("content-type"), "image/png");
    let rg = call(
        &a,
        "GET",
        &url,
        Some(&a.admin),
        &[("range", "bytes=10-19")],
        vec![],
    )
    .await;
    assert_eq!(rg.status, 206);
    assert_eq!(rg.body, png[10..20]);
    // a file part for a column that is not a file column is refused
    let (ct, body) = multipart(&[
        ("data", None, br#"{"name": "x"}"#.to_vec()),
        ("name", Some(("a.txt", "text/plain")), b"hi".to_vec()),
    ]);
    let r = call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        &[("content-type", &ct)],
        body,
    )
    .await;
    assert_eq!(r.status, 400);
    // a file before data is refused
    let (ct, body) = multipart(&[
        ("photo", Some(("a.txt", "text/plain")), b"hi".to_vec()),
        ("data", None, b"{}".to_vec()),
    ]);
    let r = call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        &[("content-type", &ct)],
        body,
    )
    .await;
    assert_eq!(r.status, 400);
    // attach an object by reference
    assert_eq!(
        json_call(
            &a,
            "POST",
            "/buckets",
            Some(&a.admin),
            json!({"name": "pics"})
        )
        .await
        .status,
        201
    );
    let mut obj = data(5000, 9);
    obj[..3].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
    assert_eq!(
        call(
            &a,
            "PUT",
            "/buckets/pics/objects/cat.jpg",
            Some(&a.admin),
            &[("content-type", "image/jpeg")],
            obj.clone()
        )
        .await
        .status,
        200
    );
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "Cat", "photo": {"object": {"bucket": "pics", "key": "cat.jpg"}}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let url = r.json()["row"]["photo"]["url"]
        .as_str()
        .unwrap()
        .strip_prefix("/api/v1")
        .unwrap()
        .to_string();
    assert_eq!(
        call(&a, "GET", &url, Some(&a.admin), &[], vec![])
            .await
            .body,
        obj
    );
    // the file outlives the object it came from
    assert_eq!(
        call(
            &a,
            "DELETE",
            "/buckets/pics/objects/cat.jpg",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        204
    );
    assert_eq!(
        call(&a, "GET", &url, Some(&a.admin), &[], vec![])
            .await
            .body,
        obj
    );
    // an object that does not exist
    let r = json_call(
        &a,
        "POST",
        "/tables/people/rows",
        Some(&a.admin),
        json!({"name": "Ghost", "photo": {"object": {"bucket": "pics", "key": "gone.jpg"}}}),
    )
    .await;
    assert_eq!(r.status, 404, "{}", String::from_utf8_lossy(&r.body));
}
