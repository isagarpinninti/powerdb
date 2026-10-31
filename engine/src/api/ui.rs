//! The dashboard: static files compiled into the binary (`engine/ui`, built from `/dashboard`), so `powerdb serve` is one file.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::Response;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Assets;

const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return Response::builder().status(StatusCode::NOT_FOUND).header(header::CONTENT_TYPE, "application/json").body(Body::from(r#"{"error":{"code":"not_found","message":"no such endpoint"}}"#)).unwrap_or_default();
    }
    let (file, immutable) = match Assets::get(if path.is_empty() { "index.html" } else { path }) {
        Some(f) => (f, path.starts_with("assets/")),
        None => match Assets::get("index.html") {
            Some(f) => (f, false), // single-page app: unknown paths show the app
            None => return Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap_or_default(),
        },
    };
    let mime = mime_guess::from_path(if Assets::get(path).is_some() { path } else { "index.html" }).first_or_octet_stream();
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap_or(HeaderValue::from_static("application/octet-stream")))
        .header(header::CACHE_CONTROL, if immutable { "public, max-age=31536000, immutable" } else { "no-cache" })
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .body(Body::from(file.data.into_owned()))
        .unwrap_or_default()
}
