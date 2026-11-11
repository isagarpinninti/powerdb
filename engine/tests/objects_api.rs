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

async fn bucket(a: &App, name: &str, versioning: bool) {
    let r = json_call(
        a,
        "POST",
        "/buckets",
        Some(&a.admin),
        json!({"name": name, "versioning": versioning}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
}

async fn put(a: &App, bucket: &str, key: &str, mime: &str, body: Vec<u8>) -> Res {
    call(
        a,
        "PUT",
        &format!("/buckets/{bucket}/objects/{key}"),
        Some(&a.admin),
        &[("content-type", mime)],
        body,
    )
    .await
}

#[tokio::test]
async fn only_administrators_can_use_buckets() {
    let a = app();
    let r = call(&a, "GET", "/buckets", None, &[], vec![]).await;
    assert_eq!(r.status, 401);
    let r = call(
        &a,
        "GET",
        "/buckets",
        Some("pdb_live_nonsense"),
        &[],
        vec![],
    )
    .await;
    assert_eq!(r.status, 401);
    // a scoped, non-admin key
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
        ("GET", "/buckets"),
        ("POST", "/buckets"),
        ("GET", "/buckets/x/objects"),
        ("PUT", "/buckets/x/objects/k"),
        ("GET", "/buckets/x/objects/k"),
        ("DELETE", "/buckets/x/objects/k"),
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
    // a signed-in ordinary user
    let u = json_call(
        &a,
        "POST",
        "/users",
        Some(&a.admin),
        json!({"email": "u@example.com", "password": "a long enough passphrase"}),
    )
    .await;
    assert_eq!(u.status, 201);
    let l = json_call(
        &a,
        "POST",
        "/auth/login",
        None,
        json!({"email": "u@example.com", "password": "a long enough passphrase"}),
    )
    .await;
    let token = l.json()["token"].as_str().unwrap().to_string();
    let r = call(&a, "GET", "/buckets", Some(&token), &[], vec![]).await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn buckets_are_created_listed_changed_and_deleted() {
    let a = app();
    let r = json_call(
        &a,
        "POST",
        "/buckets",
        Some(&a.admin),
        json!({"name": "Bad Name"}),
    )
    .await;
    assert_eq!(r.status, 400);
    bucket(&a, "docs", false).await;
    let r = json_call(
        &a,
        "POST",
        "/buckets",
        Some(&a.admin),
        json!({"name": "docs"}),
    )
    .await;
    assert_eq!(r.status, 409);
    let r = call(&a, "GET", "/buckets", Some(&a.admin), &[], vec![]).await;
    assert_eq!(r.json()["items"][0]["name"], "docs");
    let r = call(&a, "GET", "/buckets/docs", Some(&a.admin), &[], vec![]).await;
    assert_eq!(r.json()["bucket"]["versioning"], false);
    let r = call(&a, "GET", "/buckets/nope", Some(&a.admin), &[], vec![]).await;
    assert_eq!(r.status, 404);
    let r = json_call(
        &a,
        "PATCH",
        "/buckets/docs",
        Some(&a.admin),
        json!({"versioning": true}),
    )
    .await;
    assert_eq!(r.json()["bucket"]["versioning"], true);
    let r = json_call(
        &a,
        "PATCH",
        "/buckets/docs",
        Some(&a.admin),
        json!({"versioning": false}),
    )
    .await;
    assert_eq!(r.status, 400, "versioning cannot be turned off");
    put(&a, "docs", "k", "text/plain", b"hello".to_vec()).await;
    let r = call(&a, "DELETE", "/buckets/docs", Some(&a.admin), &[], vec![]).await;
    assert_eq!(r.status, 409, "a bucket with objects is not deleted");
    call(
        &a,
        "DELETE",
        "/buckets/docs/objects/k?version=nope",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    let vs = call(
        &a,
        "GET",
        "/buckets/docs/versions?key=k",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await
    .json();
    for v in vs["items"].as_array().unwrap() {
        let id = v["version_id"].as_str().unwrap();
        let r = call(
            &a,
            "DELETE",
            &format!("/buckets/docs/objects/k?version={id}"),
            Some(&a.admin),
            &[],
            vec![],
        )
        .await;
        assert_eq!(r.status, 204);
    }
    let r = call(&a, "DELETE", "/buckets/docs", Some(&a.admin), &[], vec![]).await;
    assert_eq!(r.status, 204);
}

#[tokio::test]
async fn an_object_is_uploaded_and_downloaded_byte_for_byte_with_its_headers() {
    let a = app();
    bucket(&a, "docs", false).await;
    let bytes = data(3_000_000, 1); // several chunks
    let r = call(
        &a,
        "PUT",
        "/buckets/docs/objects/reports/2026/q1.bin?filename=Q1%20report.bin",
        Some(&a.admin),
        &[
            ("content-type", "application/octet-stream"),
            ("x-powerdb-meta-customer", "acme"),
            ("x-powerdb-meta-year", "2026"),
        ],
        bytes.clone(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let obj = r.json()["object"].clone();
    assert_eq!(obj["size"], 3_000_000);
    assert_eq!(obj["key"], "reports/2026/q1.bin");
    assert_eq!(obj["filename"], "Q1 report.bin");
    assert_eq!(obj["metadata"]["customer"], "acme");
    assert_eq!(
        obj["checksum"],
        powerdb::util::hex(blake3::hash(&bytes).as_bytes())
    );

    let g = call(
        &a,
        "GET",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    assert_eq!(g.status, 200);
    assert!(g.body == bytes, "the download differs from the upload");
    assert_eq!(g.header("content-type"), "application/octet-stream");
    assert_eq!(g.header("content-length"), "3000000");
    assert_eq!(
        g.header("etag"),
        format!("\"{}\"", obj["checksum"].as_str().unwrap())
    );
    assert_eq!(g.header("accept-ranges"), "bytes");
    assert_eq!(g.header("x-powerdb-meta-customer"), "acme");
    assert_eq!(
        g.header("x-powerdb-version-id"),
        obj["version_id"].as_str().unwrap()
    );
    assert!(g.header("content-disposition").contains("Q1%20report.bin"));
    assert!(g.header("content-security-policy").contains("sandbox"));

    // ranges, across a chunk boundary and from the end
    let r = call(
        &a,
        "GET",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[("range", "bytes=1000000-1999999")],
        vec![],
    )
    .await;
    assert_eq!(r.status, 206);
    assert_eq!(r.header("content-range"), "bytes 1000000-1999999/3000000");
    assert!(r.body == bytes[1_000_000..2_000_000]);
    let r = call(
        &a,
        "GET",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[("range", "bytes=-10")],
        vec![],
    )
    .await;
    assert_eq!(r.status, 206);
    assert!(r.body == bytes[2_999_990..]);
    let r = call(
        &a,
        "GET",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[("range", "bytes=9000000-")],
        vec![],
    )
    .await;
    assert_eq!(r.status, 416);

    // HEAD: the same headers, no body; a matching ETag: 304
    let h = call(
        &a,
        "HEAD",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    assert_eq!(h.status, 200);
    assert!(h.body.is_empty());
    assert_eq!(h.header("content-length"), "3000000");
    let etag = g.header("etag").to_string();
    let r = call(
        &a,
        "GET",
        "/buckets/docs/objects/reports/2026/q1.bin",
        Some(&a.admin),
        &[("if-none-match", &etag)],
        vec![],
    )
    .await;
    assert_eq!(r.status, 304);
}

#[tokio::test]
async fn uploads_are_checked_before_they_are_stored() {
    let a = app();
    bucket(&a, "docs", false).await;
    // a program claiming to be an image
    let exe = [b"MZ\x90\0\x03\0\0\0".as_slice(), &data(2000, 2)].concat();
    let r = put(&a, "docs", "photo.png", "image/png", exe).await;
    assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(
        call(
            &a,
            "GET",
            "/buckets/docs/objects/photo.png",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        404,
        "nothing was stored"
    );
    // an unknown type is just bytes
    let r = put(
        &a,
        "docs",
        "blob",
        "application/x-something-new",
        data(5000, 3),
    )
    .await;
    assert_eq!(r.status, 200);
    let r = put(&a, "docs", "empty", "text/plain", vec![]).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        call(
            &a,
            "GET",
            "/buckets/docs/objects/empty",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .body
        .len(),
        0
    );
    // keys
    for bad in ["%2E%2E/x", "a/%2E%2E/b", "a%00b", "%0Ax"] {
        let r = put(&a, "docs", bad, "text/plain", b"x".to_vec()).await;
        assert_eq!(r.status, 400, "{bad}");
    }
    // a missing bucket is answered without reading the body
    let r = put(&a, "nobucket", "k", "text/plain", b"x".to_vec()).await;
    assert_eq!(r.status, 404);
    // metadata names and values are checked
    let r = call(
        &a,
        "PUT",
        "/buckets/docs/objects/m",
        Some(&a.admin),
        &[
            ("content-type", "text/plain"),
            ("x-powerdb-meta-has.dot", "v"),
        ],
        b"x".to_vec(),
    )
    .await;
    assert_eq!(r.status, 200, "a dot is allowed in a metadata name");
    let long = "v".repeat(1025);
    let r = call(
        &a,
        "PUT",
        "/buckets/docs/objects/m2",
        Some(&a.admin),
        &[
            ("content-type", "text/plain"),
            ("x-powerdb-meta-big", &long),
        ],
        b"x".to_vec(),
    )
    .await;
    assert_eq!(r.status, 400, "a metadata value over 1024 bytes is refused");
}

#[tokio::test]
async fn versions_over_http() {
    let a = app();
    bucket(&a, "v", true).await;
    let v1 = put(&a, "v", "doc.txt", "text/plain", b"first".to_vec())
        .await
        .json()["object"]["version_id"]
        .as_str()
        .unwrap()
        .to_string();
    let v2 = put(&a, "v", "doc.txt", "text/plain", b"second".to_vec())
        .await
        .json()["object"]["version_id"]
        .as_str()
        .unwrap()
        .to_string();
    let get = |q: String| {
        let a = &a;
        async move {
            call(
                a,
                "GET",
                &format!("/buckets/v/objects/doc.txt{q}"),
                Some(&a.admin),
                &[],
                vec![],
            )
            .await
        }
    };
    assert_eq!(get(String::new()).await.body, b"second");
    assert_eq!(get(format!("?version={v1}")).await.body, b"first");
    let vs = call(
        &a,
        "GET",
        "/buckets/v/versions?key=doc.txt",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await
    .json();
    assert_eq!(vs["items"][0]["version_id"], v2.as_str());
    assert_eq!(vs["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(
            &a,
            "GET",
            "/buckets/v/versions",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400,
        "the key is required"
    );

    assert_eq!(
        call(
            &a,
            "DELETE",
            "/buckets/v/objects/doc.txt",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        204
    );
    assert_eq!(
        get(String::new()).await.status,
        404,
        "a delete marker hides the key"
    );
    assert_eq!(
        get(format!("?version={v1}")).await.body,
        b"first",
        "the data is still there"
    );
    assert_eq!(
        call(
            &a,
            "DELETE",
            "/buckets/v/objects/doc.txt",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        404,
        "already deleted"
    );
    let l = call(&a, "GET", "/buckets/v/objects", Some(&a.admin), &[], vec![])
        .await
        .json();
    assert_eq!(l["items"].as_array().unwrap().len(), 0);

    let r = json_call(
        &a,
        "POST",
        "/buckets/v/restore",
        Some(&a.admin),
        json!({"key": "doc.txt", "version": v1}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(get(String::new()).await.body, b"first");
    let r = json_call(
        &a,
        "POST",
        "/buckets/v/restore",
        Some(&a.admin),
        json!({"key": "doc.txt", "version": "nope"}),
    )
    .await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn listing_over_http_pages_with_a_cursor() {
    let a = app();
    bucket(&a, "l", false).await;
    for i in 0..12 {
        put(
            &a,
            "l",
            &format!("{}/f{i:02}", if i % 2 == 0 { "a" } else { "b" }),
            "text/plain",
            vec![b'x'; i + 1],
        )
        .await;
    }
    let mut seen = Vec::new();
    let mut after = String::new();
    loop {
        let q = if after.is_empty() {
            "?limit=5".to_string()
        } else {
            format!("?limit=5&after={after}")
        };
        let r = call(
            &a,
            "GET",
            &format!("/buckets/l/objects{q}"),
            Some(&a.admin),
            &[],
            vec![],
        )
        .await
        .json();
        seen.extend(
            r["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["key"].as_str().unwrap().to_string()),
        );
        match r["next"].as_str() {
            Some(n) => after = n.to_string(),
            None => break,
        }
    }
    assert_eq!(seen.len(), 12);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted);
    let r = call(
        &a,
        "GET",
        "/buckets/l/objects?prefix=b/",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await
    .json();
    assert_eq!(r["items"].as_array().unwrap().len(), 6);
    assert_eq!(
        call(
            &a,
            "GET",
            "/buckets/l/objects?limit=0",
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
            "/buckets/l/objects?limit=5000",
            Some(&a.admin),
            &[],
            vec![]
        )
        .await
        .status,
        400
    );
}

#[tokio::test]
async fn an_encrypted_bucket_round_trips() {
    let a = app();
    let r = json_call(
        &a,
        "POST",
        "/buckets",
        Some(&a.admin),
        json!({"name": "vault", "encrypt": true}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let bytes = data(400_000, 9);
    assert_eq!(
        put(
            &a,
            "vault",
            "secret",
            "application/octet-stream",
            bytes.clone()
        )
        .await
        .status,
        200
    );
    let g = call(
        &a,
        "GET",
        "/buckets/vault/objects/secret",
        Some(&a.admin),
        &[],
        vec![],
    )
    .await;
    assert!(g.body == bytes);
    let r = call(
        &a,
        "GET",
        "/buckets/vault/objects/secret",
        Some(&a.admin),
        &[("range", "bytes=100000-100099")],
        vec![],
    )
    .await;
    assert_eq!(r.status, 206);
    assert!(r.body == bytes[100_000..100_100]);
}
