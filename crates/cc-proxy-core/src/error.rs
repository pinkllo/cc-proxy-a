use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpstreamErrorInfo {
    pub status: u16,
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub message: String,
    pub request_id: Option<String>,
}

impl std::fmt::Display for UpstreamErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code}): {}", self.status, self.message),
            None => write!(f, "{}: {}", self.status, self.message),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ProxyError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Upstream error: {0}")]
    Upstream(#[from] reqwest::Error),

    #[error("Upstream response error: {0}")]
    UpstreamStatus(UpstreamErrorInfo),

    #[error("Request conversion error: {0}")]
    Conversion(String),

    #[error("Streaming error: {0}")]
    Streaming(String),

    #[error("Timeout")]
    Timeout,

    #[error("Client disconnected")]
    ClientDisconnected,

    #[error("{0}")]
    Internal(String),
}

impl ProxyError {
    pub fn from_upstream_response(
        status: StatusCode,
        body: &str,
        request_id: Option<String>,
    ) -> Self {
        Self::UpstreamStatus(parse_upstream_error(status, body, request_id))
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Auth(_) => StatusCode::UNAUTHORIZED,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ClientDisconnected => {
                StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST)
            }
            Self::UpstreamStatus(info) => {
                StatusCode::from_u16(info.status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            Self::Upstream(e) => {
                if e.is_timeout() {
                    StatusCode::GATEWAY_TIMEOUT
                } else if e.is_connect() {
                    StatusCode::BAD_GATEWAY
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn upstream_status(&self) -> Option<u16> {
        match self {
            Self::UpstreamStatus(info) => Some(info.status),
            _ => None,
        }
    }

    pub fn upstream_error_code(&self) -> Option<&str> {
        match self {
            Self::UpstreamStatus(info) => info.code.as_deref(),
            _ => None,
        }
    }

    pub fn message_text(&self) -> String {
        match self {
            Self::UpstreamStatus(info) => info.message.clone(),
            _ => self.to_string(),
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let (body, info) = match &self {
            Self::UpstreamStatus(info) => (
                json!({
                    "type": "error",
                    "error": {
                        "type": proxy_error_type(info.status),
                        "message": info.message,
                        "upstream_status": info.status,
                        "upstream_code": info.code,
                        "request_id": info.request_id,
                    }
                }),
                Some(info.clone()),
            ),
            _ => (
                json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": self.to_string()
                    }
                }),
                None,
            ),
        };

        let mut response = (status, axum::Json(body)).into_response();
        if let Some(info) = info {
            insert_header(
                &mut response,
                "x-cc-proxy-upstream-status",
                &info.status.to_string(),
            );
            if let Some(code) = info.code.as_deref() {
                insert_header(&mut response, "x-cc-proxy-upstream-code", code);
            }
            if let Some(request_id) = info.request_id.as_deref() {
                insert_header(&mut response, "x-cc-proxy-upstream-request-id", request_id);
            }
        }
        response
    }
}

fn parse_upstream_error(
    status: StatusCode,
    body: &str,
    request_id: Option<String>,
) -> UpstreamErrorInfo {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let empty = serde_json::Value::Null;
    let root = parsed.as_ref().unwrap_or(&empty);
    let error_node = root.get("error").unwrap_or(root);
    let message = extract_string(error_node, "message")
        .or_else(|| extract_string(root, "message"))
        .or_else(|| extract_string(root, "error"))
        .unwrap_or_else(|| fallback_message(status, body));

    UpstreamErrorInfo {
        status: status.as_u16(),
        code: extract_string(error_node, "code").or_else(|| extract_string(root, "code")),
        error_type: extract_string(error_node, "type").or_else(|| extract_string(root, "type")),
        message,
        request_id,
    }
}

fn extract_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn fallback_message(status: StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        trimmed.to_string()
    } else {
        status
            .canonical_reason()
            .unwrap_or("Upstream request failed")
            .to_string()
    }
}

fn proxy_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 | 403 => "authentication_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    }
}

fn insert_header(response: &mut Response, key: &str, value: &str) {
    let Ok(name) = axum::http::HeaderName::from_bytes(key.as_bytes()) else {
        return;
    };
    let Ok(value) = axum::http::HeaderValue::from_str(value) else {
        return;
    };
    response.headers_mut().insert(name, value);
}
