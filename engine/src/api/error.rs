use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// An error as the client sees it:  {"error": {"code": "...", "message": "..."}}  Internal failures are logged in full and reported without detail.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn bad_request(m: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", m)
    }
    pub fn unauthorized(m: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", m)
    }
    pub fn forbidden(m: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", m)
    }
    pub fn not_found(m: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }
    pub fn too_large(m: &str) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", m)
    }
    pub fn too_many(m: &str) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", m)
    }
    pub fn internal(m: &str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", m)
    }
}

impl From<crate::Error> for ApiError {
    fn from(e: crate::Error) -> Self {
        use crate::Error::*;
        match e {
            NotFound(m) => Self::not_found(&format!("{m} was not found")),
            Invalid(m) => Self::bad_request(&m),
            Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            Denied(m) => Self::forbidden(&m),
            other => {
                tracing::error!("internal error: {other}");
                Self::internal("something went wrong on the server")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (
            self.status,
            Json(json!({"error": {"code": self.code, "message": self.message}})),
        )
            .into_response();
        r.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        r
    }
}
