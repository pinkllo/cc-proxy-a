use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use serde_json::json;

/// Middleware: validate client API key if ANTHROPIC_API_KEY is configured
pub async fn auth_middleware(
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    // Get expected key from app state
    let expected_key = request
        .extensions()
        .get::<Option<String>>()
        .cloned()
        .flatten();

    // If no expected key configured, skip validation
    let Some(expected) = expected_key else {
        return Ok(next.run(request).await);
    };

    // Extract client key from headers
    let client_key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| {
            request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(String::from)
        });

    match client_key {
        Some(key) if key == expected => Ok(next.run(request).await),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "type": "error",
                "error": {
                    "type": "authentication_error",
                    "message": "Invalid API key."
                }
            })),
        )),
    }
}
