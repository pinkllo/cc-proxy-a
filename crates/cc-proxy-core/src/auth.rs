use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use serde_json::json;

use crate::server::AppState;

/// Middleware: validate client API key if ANTHROPIC_API_KEY is configured.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    let expected_key = state.runtime.current_auth_key();

    // If no expected key configured, skip validation
    let Some(ref expected) = expected_key else {
        return Ok(next.run(request).await);
    };

    // Extract client key from x-api-key header or Authorization: Bearer
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
        Some(ref key) if key == expected => Ok(next.run(request).await),
        _ => {
            tracing::warn!("Invalid API key from client");
            Err((
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({
                    "type": "error",
                    "error": {
                        "type": "authentication_error",
                        "message": "Invalid API key."
                    }
                })),
            ))
        }
    }
}
