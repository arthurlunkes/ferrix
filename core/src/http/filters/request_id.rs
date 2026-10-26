//! `X-Request-Id` tagging (`http.request_id`): keeps the client's ID or creates one, and
//! sends it upstream and back to the client.

use axum::http::{HeaderName, HeaderValue};

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub fn new_request_id() -> HeaderValue {
    let id = uuid::Uuid::new_v4();
    HeaderValue::from_str(id.simple().encode_lower(&mut uuid::Uuid::encode_buffer()))
        .unwrap_or_else(|_| HeaderValue::from_static("0"))
}
