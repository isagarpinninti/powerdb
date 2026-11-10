//! Availability and transport hardening: one bad request must not take the server down, a client that stops sending must
//! not hold a connection for ever, and responses must carry the headers that stop caching and downgrades.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use powerdb::api::{self, Config};
use powerdb::server;
use powerdb::util::new_id;
use powerdb::{Durability, Options};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use tower_http::catch_panic::CatchPanicLayer;

fn state(cfg: Config) -> (api::AppState, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("powerdb-hard-{}", new_id()));
    let (st, _) = server::build_state(
        &dir,
        Options {
            durability: Durability::Never,
            ..Options::default()
        },
        cfg,
    )
    .unwrap();
    (st, dir)
}

async fn get_headers(app: &Router, uri: &str) -> axum::http::HeaderMap {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .headers()
        .clone()
}

#[tokio::test]
async fn a_panicking_handler_fails_one_request_and_the_server_keeps_serving() {
    // the same layer, with the same handler, that `api::router` applies to everything
    let app = Router::new()
        .route(
            "/boom",
            get(|| async {
                if true {
                    panic!("kaboom: a bug in a handler");
                }
                "unreachable"
            }),
        )
        .route("/ok", get(|| async { "fine" }))
        .layer(CatchPanicLayer::custom(api::panic_response as fn(_) -> _));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let get = |path: &'static str| async move {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    };
    let boom = get("/boom").await;
    assert!(boom.starts_with("HTTP/1.1 500"), "{boom}");
    assert!(
        boom.contains(r#""message":"internal error""#) && !boom.contains("kaboom"),
        "the cause is logged, never sent: {boom}"
    );
    for _ in 0..3 {
        assert!(
            get("/ok").await.contains("fine"),
            "the server must still answer after a panic"
        );
    }
}

#[test]
fn the_release_profile_unwinds_so_a_panic_cannot_abort_the_process() {
    let toml =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml")).unwrap();
    assert!(
        !toml.contains("panic = \"abort\""),
        "panic = \"abort\" would turn any handler bug into a dead server"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_sending_a_body_is_cut_off() {
    // (the environment variable lets the test be run with a huge timeout, to prove it fails when the protection is absent)
    let idle_ms: u64 = std::env::var("POWERDB_TEST_IDLE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(800);
    let (st, dir) = state(Config {
        body_idle_timeout: Duration::from_millis(idle_ms),
        ..Config::default()
    });
    let key = st
        .auth
        .create_key("t", powerdb::auth::Access::ReadWrite, vec![], true)
        .unwrap()
        .0;
    let app = api::router(st);
    // a real collection, so the handler goes on to read the body instead of answering 404 straight away
    let made = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/collections")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"x","fields":[{"name":"title","kind":"text"},{"name":"f","kind":"file"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(made.status(), StatusCode::CREATED);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let started = std::time::Instant::now();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    // promises a megabyte of upload, sends a few bytes, then goes quiet: the classic slow-body hold
    let head = format!(
        "POST /api/v1/collections/x/records HTTP/1.1\r\nHost: x\r\nauthorization: Bearer {key}\r\ncontent-type: multipart/form-data; boundary=zz\r\ncontent-length: 1000000\r\n\r\n--zz\r\n"
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    // it must end by itself (an error answer, or the connection closed), well before the client would give up
    let ended = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await;
    assert!(
        ended.is_ok(),
        "the server kept a stalled upload open for 10 s"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(idle_ms.min(600)),
        "it ended after {:?}: an immediate error, not the idle timeout cutting the stalled body off ({})",
        started.elapsed(),
        String::from_utf8_lossy(&buf).lines().next().unwrap_or("")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn transport_headers_hsts_only_with_tls_and_no_caching_of_api_answers() {
    let (plain, d1) = state(Config::default());
    let app = api::router(plain);
    let h = get_headers(&app, "/api/v1/healthz").await;
    assert_eq!(
        h.get(header::CACHE_CONTROL).unwrap(),
        "no-store",
        "API answers must not be cached"
    );
    assert!(
        h.get(header::STRICT_TRANSPORT_SECURITY).is_none(),
        "HSTS over plain HTTP would be meaningless, and risky behind a proxy"
    );
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");

    let (tls, d2) = state(Config {
        hsts: true,
        ..Config::default()
    });
    let app = api::router(tls);
    let h = get_headers(&app, "/api/v1/healthz").await;
    assert!(
        h.get(header::STRICT_TRANSPORT_SECURITY)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("max-age=")
    );
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert!(
        r.headers().get(header::STRICT_TRANSPORT_SECURITY).is_some(),
        "errors carry it too"
    );
    let _ = std::fs::remove_dir_all(d1);
    let _ = std::fs::remove_dir_all(d2);
}
