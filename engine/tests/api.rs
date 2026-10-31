use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use powerdb::api::{self, AppState, Config};
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
    st: AppState,
    admin: String,
}

fn app_with(cfg: Config) -> App {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-api-{}", new_id())));
    let opts = Options {
        durability: Durability::Never,
        segment_bytes: 8 << 20,
        chunk: powerdb::storage::chunker::ChunkSizes {
            min: 16 << 10,
            avg: 64 << 10,
            max: 256 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    };
    let (st, boot) = server::build_state(&dir.0, opts, cfg).unwrap();
    App {
        app: api::router(st.clone()),
        st,
        admin: boot.admin_key.expect("first run mints an admin key"),
        _dir: dir,
    }
}
fn app() -> App {
    app_with(Config {
        login_attempts_per_minute: 1000,
        ..Config::default()
    })
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}
impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not json ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
}

async fn send(
    a: &App,
    method: Method,
    uri: &str,
    token: Option<&str>,
    ct: Option<&str>,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> Res {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(c) = ct {
        b = b.header(header::CONTENT_TYPE, c);
    }
    for (k, v) in extra {
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
        status: p.status,
        headers: p.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}
async fn get(a: &App, uri: &str, t: Option<&str>) -> Res {
    send(a, Method::GET, uri, t, None, vec![], &[]).await
}
async fn json(a: &App, m: Method, uri: &str, t: Option<&str>, v: Value) -> Res {
    send(
        a,
        m,
        uri,
        t,
        Some("application/json"),
        v.to_string().into_bytes(),
        &[],
    )
    .await
}

type Part<'a> = (&'a str, Option<(&'a str, &'a str)>, &'a [u8]);

fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let boundary = "----pdbtest1234567890";
    let mut out = Vec::new();
    for (name, file, data) in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file {
            Some((fname, ct)) => out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\nContent-Type: {ct}\r\n\r\n").as_bytes()),
            None => out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\nContent-Type: application/json\r\n\r\n").as_bytes()),
        }
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), out)
}

async fn upload(a: &App, method: Method, uri: &str, t: &str, parts: &[Part<'_>]) -> Res {
    let (ct, body) = multipart(parts);
    send(a, method, uri, Some(t), Some(&ct), body, &[]).await
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

fn png(len: usize, seed: u64) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend(data(len, seed));
    v
}

async fn mk_user(a: &App, email: &str) -> String {
    let r = json(
        a,
        Method::POST,
        "/api/v1/users",
        Some(&a.admin),
        json!({"email": email, "password": "correct horse battery"}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let l = json(
        a,
        Method::POST,
        "/api/v1/auth/login",
        None,
        json!({"email": email, "password": "correct horse battery"}),
    )
    .await;
    assert_eq!(l.status, StatusCode::OK);
    l.json()["token"].as_str().unwrap().to_string()
}

fn invoices(rules: Value, encrypt: bool) -> Value {
    json!({"name": "invoices", "encrypt": encrypt, "rules": rules, "fields": [
        {"name": "client", "kind": "text", "required": true},
        {"name": "amount", "kind": "number"},
        {"name": "status", "kind": "text"},
        {"name": "owner_id", "kind": "text"},
        {"name": "document", "kind": "file", "max": 2000000, "mime": ["image/*", "application/pdf"]}
    ]})
}
async fn mk_collection(a: &App, def: Value) {
    let r = json(a, Method::POST, "/api/v1/collections", Some(&a.admin), def).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

fn owner_rules() -> Value {
    json!({"list": "@request.auth.id != \"\"", "view": "@request.auth.id != \"\"",
           "create": "@request.auth.id != \"\" && @request.body.owner_id == @request.auth.id",
           "update": "@request.auth.id == owner_id", "delete": "@request.auth.id == owner_id"})
}

#[tokio::test]
async fn collections_are_locked_until_a_rule_opens_them() {
    let a = app();
    mk_collection(&a, invoices(json!({}), false)).await;
    let u = mk_user(&a, "alice@example.com").await;
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED,
        "anonymous"
    );
    let r = get(&a, "/api/v1/collections/invoices/records", Some(&u)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "signed in but locked");
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("locked")
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            Some(&u),
            json!({"client": "x"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(&a.admin))
            .await
            .status,
        StatusCode::OK,
        "administrators always can"
    );
    assert_eq!(
        get(&a, "/api/v1/collections/nope/records", Some(&u))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_empty_rule_makes_an_action_public() {
    let a = app();
    mk_collection(&a, invoices(json!({"list": "", "view": ""}), false)).await;
    let r = json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&a.admin),
        json!({"client": "pub", "amount": 5}),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get(
            &a,
            &format!("/api/v1/collections/invoices/records/{id}"),
            None
        )
        .await
        .json()["client"],
        "pub"
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            None,
            json!({"client": "x"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED,
        "create is still locked"
    );
}

#[tokio::test]
async fn only_the_owner_may_change_or_delete_and_nobody_may_forge_ownership() {
    let a = app();
    mk_collection(&a, invoices(owner_rules(), false)).await;
    let (alice, bob) = (
        mk_user(&a, "alice@example.com").await,
        mk_user(&a, "bob@example.com").await,
    );
    let alice_id = get(&a, "/api/v1/auth/me", Some(&alice)).await.json()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bob_id = get(&a, "/api/v1/auth/me", Some(&bob)).await.json()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let forged = json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&alice),
        json!({"client": "x", "owner_id": bob_id}),
    )
    .await;
    assert_eq!(
        forged.status,
        StatusCode::FORBIDDEN,
        "alice cannot create a record owned by bob"
    );
    let none = json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&alice),
        json!({"client": "x"}),
    )
    .await;
    assert_eq!(
        none.status,
        StatusCode::FORBIDDEN,
        "an ownerless record is not allowed either"
    );
    let ok = json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&alice),
        json!({"client": "Acme", "amount": 10, "owner_id": alice_id}),
    )
    .await;
    assert_eq!(ok.status, StatusCode::CREATED);
    let id = ok.json()["id"].as_str().unwrap().to_string();
    let url = format!("/api/v1/collections/invoices/records/{id}");

    assert_eq!(
        get(&a, &url, Some(&bob)).await.status,
        StatusCode::OK,
        "bob may view"
    );
    assert_eq!(get(&a, &url, None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(
            &a,
            Method::PATCH,
            &url,
            Some(&bob),
            json!({"status": "hacked"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&a, Method::DELETE, &url, Some(&bob), None, vec![], &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        json(
            &a,
            Method::PATCH,
            &url,
            Some(&alice),
            json!({"status": "paid"})
        )
        .await
        .json()["status"],
        "paid"
    );
    assert_eq!(get(&a, &url, Some(&bob)).await.json()["status"], "paid");
    assert_eq!(
        send(&a, Method::DELETE, &url, Some(&alice), None, vec![], &[])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get(&a, &url, Some(&alice)).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_list_rule_that_ignores_the_record_is_decided_up_front() {
    let a = app();
    mk_collection(
        &a,
        invoices(
            json!({"list": "@request.auth.id != \"\"", "create": ""}),
            false,
        ),
    )
    .await;
    let u = mk_user(&a, "alice@example.com").await;
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED,
        "anonymous is told so, not shown an empty list"
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(&u))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_list_rule_hides_records_instead_of_failing() {
    let a = app();
    let rules = json!({"list": "@request.auth.id == owner_id", "view": "@request.auth.id == owner_id", "create": ""});
    mk_collection(&a, invoices(rules, false)).await;
    let (alice, bob) = (
        mk_user(&a, "alice@example.com").await,
        mk_user(&a, "bob@example.com").await,
    );
    let ids: Vec<String> = ["alice@example.com", "bob@example.com"]
        .iter()
        .map(|_| String::new())
        .collect();
    let _ = ids;
    let me = |t: &str| {
        let t = t.to_string();
        async move { t }
    };
    let _ = me;
    let aid = get(&a, "/api/v1/auth/me", Some(&alice)).await.json()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bid = get(&a, "/api/v1/auth/me", Some(&bob)).await.json()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (i, o) in [(1, &aid), (2, &aid), (3, &bid)] {
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            Some(&a.admin),
            json!({"client": format!("c{i}"), "owner_id": o}),
        )
        .await;
    }
    json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&a.admin),
        json!({"client": "orphan"}),
    )
    .await;
    let mine = get(&a, "/api/v1/collections/invoices/records", Some(&alice))
        .await
        .json();
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", None)
            .await
            .status,
        StatusCode::OK,
        "a rule about the record filters: anonymous sees an empty list, not an error"
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", None)
            .await
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(&bob))
            .await
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "bob does not see ownerless records either"
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(&a.admin))
            .await
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[tokio::test]
async fn filter_sort_and_pagination() {
    let a = app();
    mk_collection(&a, invoices(json!({"list": "", "view": ""}), false)).await;
    for i in 0..25 {
        let st = if i % 2 == 0 { "paid" } else { "open" };
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            Some(&a.admin),
            json!({"client": format!("client {i:02}"), "amount": i * 100, "status": st}),
        )
        .await;
    }
    let q = |extra: &str| format!("/api/v1/collections/invoices/records?{extra}");
    let r = get(
        &a,
        &q("filter=status%20%3D%3D%20%22paid%22%20%26%26%20amount%20%3E%3D%201000&sort=-amount"),
        None,
    )
    .await
    .json();
    let amounts: Vec<i64> = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["amount"].as_i64().unwrap())
        .collect();
    assert_eq!(
        amounts,
        vec![2400, 2200, 2000, 1800, 1600, 1400, 1200, 1000],
        "paid and at least 1000 (1000 itself included), biggest first"
    );
    assert_eq!(r["total"], 8);
    // the REST spelling
    let r2 = get(
        &a,
        &q("filter=(status%3D'paid'%20and%20amount%3E1000)&sort=amount"),
        None,
    )
    .await
    .json();
    assert_eq!(r2["items"].as_array().unwrap().len(), 7);
    assert_eq!(r2["items"][0]["amount"], 1200);
    // newest first by default, paged
    let p1 = get(&a, &q("limit=10"), None).await.json();
    assert_eq!(p1["items"].as_array().unwrap().len(), 10);
    assert_eq!(p1["items"][0]["client"], "client 24");
    assert_eq!(p1["has_more"], true);
    let p3 = get(&a, &q("limit=10&offset=20"), None).await.json();
    assert_eq!(p3["items"].as_array().unwrap().len(), 5);
    assert_eq!(p3["has_more"], false);
    assert_eq!(p3["total"], 25);
    let old = get(&a, &q("sort=created_at&limit=2"), None).await.json();
    assert_eq!(old["items"][0]["client"], "client 00");
    // contains, case-insensitive
    assert_eq!(
        get(&a, &q("filter=client%20~%20%22CLIENT%200%22"), None)
            .await
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
}

#[tokio::test]
async fn bad_queries_are_400_and_hostile_text_is_just_data() {
    let a = app();
    mk_collection(&a, invoices(json!({"list": "", "view": ""}), false)).await;
    json(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        Some(&a.admin),
        json!({"client": "x\" || true || \"", "status": "paid"}),
    )
    .await;
    let q = |extra: &str| format!("/api/v1/collections/invoices/records?{extra}");
    for bad in [
        "filter=statuss%20%3D%3D%201",
        "filter=status%20%3D%3D",
        "filter=1%3B%20DROP%20TABLE%20users",
        "sort=nope",
        "limit=0",
        "limit=100000",
        "limit=abc",
        "offset=-1",
        "filter=((((((((((((((((((((((((((((((((((((((((a))))))))))))))))))))))))))))))))))))))))",
    ] {
        let r = get(&a, &q(bad), None).await;
        assert_eq!(
            r.status,
            StatusCode::BAD_REQUEST,
            "{bad}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert!(r.json()["error"]["message"].is_string());
    }
    let r = get(
        &a,
        &q("filter=client%20%3D%3D%20%22x%5C%22%20%7C%7C%20true%20%7C%7C%20%5C%22%22"),
        None,
    )
    .await
    .json();
    assert_eq!(
        r["items"].as_array().unwrap().len(),
        1,
        "a quote inside the value stays inside the value"
    );
    let none = get(
        &a,
        &q("filter=client%20%3D%3D%20%22x%22%20%7C%7C%20status%20%3D%3D%20%22void%22"),
        None,
    )
    .await
    .json();
    assert_eq!(none["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn files_upload_download_range_and_revocation() {
    let a = app();
    mk_collection(
        &a,
        invoices(
            json!({"list": "", "view": "", "create": "", "delete": ""}),
            false,
        ),
    )
    .await;
    let body = png(900_000, 1);
    let r = upload(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        &a.admin,
        &[
            ("data", None, br#"{"client":"Acme","amount":4500}"#),
            ("document", Some(("invoice.png", "image/png")), &body),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let rec = r.json();
    let id = rec["id"].as_str().unwrap().to_string();
    assert_eq!(rec["document"]["size"], body.len());
    assert_eq!(rec["document"]["mime"], "image/png");
    let url = rec["document"]["url"].as_str().unwrap().to_string();
    assert!(url.contains("token="));

    let d = get(&a, &url, None).await;
    assert_eq!(d.status, StatusCode::OK);
    assert_eq!(d.body, body, "the downloaded bytes are the uploaded bytes");
    assert_eq!(d.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(d.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        d.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert!(
        d.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("inline")
    );

    let r1 = send(
        &a,
        Method::GET,
        &url,
        None,
        None,
        vec![],
        &[("range", "bytes=100-199")],
    )
    .await;
    assert_eq!(r1.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r1.body, &body[100..200]);
    assert_eq!(
        r1.headers[header::CONTENT_RANGE],
        format!("bytes 100-199/{}", body.len())
    );
    let tail = send(
        &a,
        Method::GET,
        &url,
        None,
        None,
        vec![],
        &[("range", "bytes=-50")],
    )
    .await;
    assert_eq!(tail.body, &body[body.len() - 50..]);
    let open = send(
        &a,
        Method::GET,
        &url,
        None,
        None,
        vec![],
        &[("range", &format!("bytes={}-", body.len() - 10))],
    )
    .await;
    assert_eq!(open.body, &body[body.len() - 10..]);
    let bad = send(
        &a,
        Method::GET,
        &url,
        None,
        None,
        vec![],
        &[("range", "bytes=99999999-")],
    )
    .await;
    assert_eq!(bad.status, StatusCode::RANGE_NOT_SATISFIABLE);
    let etag = d.headers[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(
        send(
            &a,
            Method::GET,
            &url,
            None,
            None,
            vec![],
            &[("if-none-match", &etag)]
        )
        .await
        .status,
        StatusCode::NOT_MODIFIED
    );

    // links cannot be altered, moved or kept past their expiry
    let tampered = url.replace("token=", "token=A");
    assert_eq!(get(&a, &tampered, None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get(&a, &url.replace("/document?", "/nothing?"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let fid = &rec["document"]["url"];
    let _ = fid;
    let rec_full = a.st.db.get("invoices", &id).unwrap().unwrap();
    let past = a.st.signer.sign(
        &format!("invoices/{id}/document/{}", rec_full.files["document"].id),
        powerdb::util::now_ms() / 1000 - 1,
    );
    assert_eq!(
        get(
            &a,
            &format!("/api/v1/files/invoices/{id}/document?token={past}"),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN,
        "expired"
    );
    let other = a.st.signer.sign(
        "invoices/someone-else/document/x",
        powerdb::util::now_ms() / 1000 + 60,
    );
    assert_eq!(
        get(
            &a,
            &format!("/api/v1/files/invoices/{id}/document?token={other}"),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN,
        "a link for another file"
    );

    // deleting the record ends access to its file immediately, even with a link that is still within its minute
    assert_eq!(
        send(
            &a,
            Method::DELETE,
            &format!("/api/v1/collections/invoices/records/{id}"),
            Some(&a.admin),
            None,
            vec![],
            &[]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get(&a, &url, None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disguised_oversized_and_misplaced_uploads_are_refused_before_anything_is_stored() {
    let a = app();
    mk_collection(&a, invoices(json!({"create": ""}), false)).await;
    let before = a.st.db.stats().unwrap().log_bytes;
    let uri = "/api/v1/collections/invoices/records";
    let mut exe = b"MZ\x90\x00\x03\x00".to_vec();
    exe.extend(data(500_000, 2));
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("document", Some(("photo.png", "image/png")), &exe),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("executable")
    );
    let pdf_as_png = [b"%PDF-1.7\n".to_vec(), data(300_000, 3)].concat();
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("document", Some(("x.png", "image/png")), &pdf_as_png),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("looks like application/pdf")
    );
    let big = png(2_500_000, 4);
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("document", Some(("big.png", "image/png")), &big),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let html = b"<html><script>alert(1)</script></html>".to_vec();
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("document", Some(("x.html", "text/html")), &html),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "type not in the field's allowed list"
    );
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("document", Some(("x.png", "image/png")), &png(1000, 5)),
            ("data", None, br#"{"client":"a"}"#),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "the data part must come first"
    );
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("client", Some(("x.png", "image/png")), &png(1000, 5)),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "client is not a file field"
    );
    let r = upload(
        &a,
        Method::POST,
        uri,
        &a.admin,
        &[("data", None, b"not json")],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        a.st.db.count("invoices").unwrap(),
        0,
        "no record was created by any of those"
    );
    // the pieces the oversized upload wrote before it was cut off are garbage the log can drop; the disguised ones never reached it
    let after = a.st.db.stats().unwrap().log_bytes;
    assert!(
        after - before < 3_000_000,
        "only the oversized file may have left bytes behind ({} bytes)",
        after - before
    );
}

#[tokio::test]
async fn update_replaces_and_removes_files() {
    let a = app();
    mk_collection(&a, invoices(json!({"view": "", "update": ""}), false)).await;
    let base = png(400_000, 6);
    let r = upload(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        &a.admin,
        &[
            ("data", None, br#"{"client":"a"}"#),
            ("document", Some(("a.png", "image/png")), &base),
        ],
    )
    .await
    .json();
    let id = r["id"].as_str().unwrap();
    let uri = format!("/api/v1/collections/invoices/records/{id}");
    let newer = png(450_000, 7);
    let r2 = upload(
        &a,
        Method::PATCH,
        &uri,
        &a.admin,
        &[
            ("data", None, br#"{"status":"paid"}"#),
            ("document", Some(("b.png", "image/png")), &newer),
        ],
    )
    .await;
    assert_eq!(
        r2.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r2.body)
    );
    let j = r2.json();
    assert_eq!(j["status"], "paid");
    assert_eq!(
        get(&a, j["document"]["url"].as_str().unwrap(), None)
            .await
            .body,
        newer
    );
    let r3 = json(
        &a,
        Method::PATCH,
        &uri,
        Some(&a.admin),
        json!({"_remove_files": ["document"]}),
    )
    .await
    .json();
    assert!(r3.get("document").is_none());
    assert_eq!(r3["client"], "a");
}

#[tokio::test]
async fn private_collections_work_through_the_api_and_store_no_plaintext() {
    let a = app();
    mk_collection(&a, invoices(json!({"view": "", "create": ""}), true)).await;
    let mut body = png(300_000, 8);
    body.extend_from_slice(&b"SECRET-NEEDLE-SECRET-NEEDLE".repeat(2000));
    let r = upload(
        &a,
        Method::POST,
        "/api/v1/collections/invoices/records",
        &a.admin,
        &[
            ("data", None, br#"{"client":"vault"}"#),
            ("document", Some(("v.png", "image/png")), &body),
        ],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let url = r.json()["document"]["url"].as_str().unwrap().to_string();
    assert_eq!(get(&a, &url, None).await.body, body);
    let part = send(
        &a,
        Method::GET,
        &url,
        None,
        None,
        vec![],
        &[("range", "bytes=300100-300199")],
    )
    .await;
    assert_eq!(
        part.body,
        &body[300_100..300_200],
        "ranges work through decryption"
    );
    a.st.db.flush().unwrap();
    for e in std::fs::read_dir(a._dir.0.join("blobs")).unwrap() {
        let raw = std::fs::read(e.unwrap().path()).unwrap();
        assert!(
            !raw.windows(20).any(|w| w == b"SECRET-NEEDLE-SECRET"),
            "plaintext found in the blob log"
        );
    }
}

#[tokio::test]
async fn api_keys_are_limited_by_their_scope() {
    let a = app();
    mk_collection(
        &a,
        invoices(
            json!({"list": "", "view": "", "create": "", "update": "", "delete": ""}),
            false,
        ),
    )
    .await;
    mk_collection(
        &a,
        json!({"name": "other", "rules": {"list": ""}, "fields": []}),
    )
    .await;
    let mk = |name: &str, access: &str, cols: Value| {
        let (n, ac) = (name.to_string(), access.to_string());
        let a = &a;
        async move {
            let r = json(
                a,
                Method::POST,
                "/api/v1/keys",
                Some(&a.admin),
                json!({"name": n, "access": ac, "collections": cols}),
            )
            .await;
            assert_eq!(r.status, StatusCode::CREATED);
            r.json()
        }
    };
    let ro = mk("ro", "read", json!(["invoices"])).await;
    let wo = mk("wo", "write", json!([])).await;
    let (rok, wok) = (ro["key"].as_str().unwrap(), wo["key"].as_str().unwrap());
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(rok))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            Some(rok),
            json!({"client": "x"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN,
        "read-only"
    );
    assert_eq!(
        get(&a, "/api/v1/collections/other/records", Some(rok))
            .await
            .status,
        StatusCode::FORBIDDEN,
        "other collection"
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/collections/invoices/records",
            Some(wok),
            json!({"client": "x"})
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(wok))
            .await
            .status,
        StatusCode::FORBIDDEN,
        "write-only"
    );
    assert_eq!(
        get(&a, "/api/v1/keys", Some(rok)).await.status,
        StatusCode::FORBIDDEN,
        "no admin rights"
    );
    // revocation takes effect at once
    let keys = get(&a, "/api/v1/keys", Some(&a.admin)).await.json();
    let kid = keys["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "ro")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(
            &a,
            Method::DELETE,
            &format!("/api/v1/keys/{kid}"),
            Some(&a.admin),
            None,
            vec![],
            &[]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get(&a, "/api/v1/collections/invoices/records", Some(rok))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(
            &a,
            "/api/v1/collections/invoices/records",
            Some("pdb_live_forged")
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn admin_endpoints_need_an_administrator_and_rules_must_parse() {
    let a = app();
    let u = mk_user(&a, "alice@example.com").await;
    for (m, uri) in [
        (Method::GET, "/api/v1/collections"),
        (Method::GET, "/api/v1/users"),
        (Method::GET, "/api/v1/keys"),
        (Method::GET, "/api/v1/stats"),
        (Method::GET, "/api/v1/metrics"),
        (Method::POST, "/api/v1/maintenance/gc"),
    ] {
        assert_eq!(
            send(&a, m.clone(), uri, None, None, vec![], &[])
                .await
                .status,
            StatusCode::UNAUTHORIZED,
            "{uri} anonymous"
        );
        assert_eq!(
            send(&a, m, uri, Some(&u), None, vec![], &[]).await.status,
            StatusCode::FORBIDDEN,
            "{uri} user"
        );
    }
    let bad = json(
        &a,
        Method::POST,
        "/api/v1/collections",
        Some(&a.admin),
        json!({"name": "c", "rules": {"list": "status =="}, "fields": []}),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert!(
        bad.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("list rule")
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/collections",
            Some(&a.admin),
            json!({"name": "Bad Name", "fields": []})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&a, "/api/v1/stats", Some(&a.admin)).await.status,
        StatusCode::OK
    );
    let m = get(&a, "/api/v1/metrics", Some(&a.admin)).await;
    assert_eq!(m.status, StatusCode::OK);
    let text = String::from_utf8(m.body.to_vec()).unwrap();
    assert!(
        text.contains("powerdb_blob_log_bytes ")
            && text.contains("# TYPE powerdb_gc_pending gauge")
    );
}

#[tokio::test]
async fn sign_in_is_throttled_and_registration_is_closed_by_default() {
    let a = app_with(Config {
        login_attempts_per_minute: 5,
        ..Config::default()
    });
    json(
        &a,
        Method::POST,
        "/api/v1/users",
        Some(&a.admin),
        json!({"email": "a@x.io", "password": "password one"}),
    )
    .await;
    let mut last = StatusCode::OK;
    for _ in 0..8 {
        last = json(
            &a,
            Method::POST,
            "/api/v1/auth/login",
            None,
            json!({"email": "a@x.io", "password": "wrong password"}),
        )
        .await
        .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let ok = json(
        &a,
        Method::POST,
        "/api/v1/auth/login",
        None,
        json!({"email": "a@x.io", "password": "password one"}),
    )
    .await;
    assert_eq!(
        ok.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the right password does not get past the throttle either"
    );
    assert_eq!(
        json(
            &a,
            Method::POST,
            "/api/v1/auth/register",
            None,
            json!({"email": "n@x.io", "password": "password one"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let open = app_with(Config {
        open_registration: true,
        ..Config::default()
    });
    let r = json(
        &open,
        Method::POST,
        "/api/v1/auth/register",
        None,
        json!({"email": "n@x.io", "password": "password one"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(
        r.json()["user"]["role"],
        "user",
        "registration never makes an administrator"
    );
}

#[tokio::test]
async fn errors_are_json_and_leak_nothing_and_the_dashboard_is_served() {
    let a = app();
    let r = get(&a, "/api/v1/collections/nope/records", Some(&a.admin)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"]["code"], "not_found");
    assert_eq!(r.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(r.headers["referrer-policy"], "no-referrer");
    let bad = send(
        &a,
        Method::POST,
        "/api/v1/collections/x/records",
        Some(&a.admin),
        Some("application/json"),
        b"{".to_vec(),
        &[],
    )
    .await;
    assert!(bad.status.is_client_error());
    let unk = get(&a, "/api/v1/nothing-here", None).await;
    assert_eq!(unk.status, StatusCode::NOT_FOUND);
    assert_eq!(unk.json()["error"]["code"], "not_found");
    let ui = get(&a, "/", None).await;
    assert_eq!(ui.status, StatusCode::OK);
    assert!(
        ui.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(
        ui.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(
        get(&a, "/some/client/route", None).await.status,
        StatusCode::OK,
        "single-page app fallback"
    );
    assert_eq!(get(&a, "/api/v1/healthz", None).await.body, b"ok");
}

#[tokio::test]
async fn a_changed_password_signs_every_other_session_out() {
    let a = app();
    let t = mk_user(&a, "alice@example.com").await;
    let t2 = json(
        &a,
        Method::POST,
        "/api/v1/auth/login",
        None,
        json!({"email": "alice@example.com", "password": "correct horse battery"}),
    )
    .await
    .json()["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        get(&a, "/api/v1/auth/me", Some(&t)).await.status,
        StatusCode::OK
    );
    let r = json(
        &a,
        Method::POST,
        "/api/v1/auth/password",
        Some(&t),
        json!({"current_password": "wrong", "new_password": "a brand new password"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = json(&a, Method::POST, "/api/v1/auth/password", Some(&t), json!({"current_password": "correct horse battery", "new_password": "a brand new password"})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        get(&a, "/api/v1/auth/me", Some(&t)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&a, "/api/v1/auth/me", Some(&t2)).await.status,
        StatusCode::UNAUTHORIZED
    );
}
