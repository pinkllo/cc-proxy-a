use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use tokio::time::timeout;

use crate::config::ProxyConfig;
use crate::convert::stream::{OpenAiSseEvent, StreamError};
use crate::error::ProxyError;
use crate::types::openai::{ResponseObject, ResponseRequest, ResponseStreamEvent};


/// HTTP client for upstream OpenAI-compatible API.
#[derive(Clone)]
pub struct UpstreamClient {
    client: reqwest::Client,
    base_url: String,
}

impl UpstreamClient {
    pub fn new(config: &ProxyConfig) -> Result<Self, ProxyError> {
        let mut default_headers = HeaderMap::new();
        default_headers.insert("content-type", HeaderValue::from_static("application/json"));

        for (key, value) in &config.custom_headers {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                default_headers.insert(name, val);
            }
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout))
            .connect_timeout(Duration::from_secs(config.connect_timeout))
            .tcp_keepalive(Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .default_headers(default_headers)
            .build()
            .map_err(|error| {
                ProxyError::Internal(format!("Failed to create HTTP client: {error}"))
            })?;

        Ok(Self {
            client,
            base_url: config.openai_base_url.trim_end_matches('/').to_string(),
        })
    }

    pub async fn create_response(
        &self,
        request: &ResponseRequest,
        api_key: &str,
    ) -> Result<ResponseObject, ProxyError> {
        let response = self.send_response_request(request, api_key).await?;

        response.json().await.map_err(Into::into)
    }

    pub async fn create_response_stream(
        &self,
        request: &ResponseRequest,
        api_key: &str,
        first_byte_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send, ProxyError> {
        let response = self.send_response_request(request, api_key).await?;

        Ok(parse_sse_stream(
            response.bytes_stream(),
            first_byte_timeout,
            idle_timeout,
        ))
    }

    async fn send_response_request(
        &self,
        request: &ResponseRequest,
        api_key: &str,
    ) -> Result<reqwest::Response, ProxyError> {
        let mut retries = 0u32;
        loop {
            match self.send_response_once(request, api_key).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    retries = retries.saturating_add(1);
                    log_retry(retries, &error);
                }
            }
        }
    }

    async fn send_response_once(
        &self,
        request: &ResponseRequest,
        api_key: &str,
    ) -> Result<reqwest::Response, ProxyError> {
        let response = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(api_key)
            .json(request)
            .send()
            .await?;
        ensure_success(response).await
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, ProxyError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let request_id = header_value(response.headers(), "x-request-id");
    let body = response.text().await.unwrap_or_default();
    Err(ProxyError::from_upstream_response(
        status, &body, request_id,
    ))
}

fn header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn log_retry(attempt: u32, error: &ProxyError) {
    tracing::warn!(
        retry_attempt = attempt,
        error = %error,
        "重试上游请求"
    );
}

const MAX_SSE_BUFFER: usize = 4 * 1024 * 1024;

fn parse_sse_stream(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    first_byte_timeout: Duration,
    idle_timeout: Duration,
) -> impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send {
    async_stream::stream! {
        let mut raw_buffer = Vec::<u8>::new();
        let mut is_first_chunk = true;

        tokio::pin!(byte_stream);
        loop {
            let timeout_duration = if is_first_chunk {
                first_byte_timeout
            } else {
                idle_timeout
            };

            let next_chunk = if timeout_duration.is_zero() {
                byte_stream.next().await
            } else {
                match timeout(timeout_duration, byte_stream.next()).await {
                    Ok(result) => result,
                    Err(_) => {
                        let kind = if is_first_chunk { "first-byte" } else { "idle" };
                        yield Err(StreamError::Connection(format!(
                            "stream {kind} timeout ({}s)",
                            timeout_duration.as_secs()
                        )));
                        return;
                    }
                }
            };

            match next_chunk {
                Some(Ok(bytes)) => {
                    is_first_chunk = false;
                    raw_buffer.extend_from_slice(&bytes);

                    if raw_buffer.len() > MAX_SSE_BUFFER {
                        yield Err(StreamError::Connection("SSE buffer overflow".into()));
                        return;
                    }

                    while let Some(pos) = raw_buffer.iter().position(|&byte| byte == b'\n') {
                        let mut line_bytes = raw_buffer[..pos].to_vec();
                        raw_buffer = raw_buffer[pos + 1..].to_vec();

                        if line_bytes.last() == Some(&b'\r') {
                            line_bytes.pop();
                        }

                        if line_bytes.is_empty() {
                            continue;
                        }

                        let line = String::from_utf8_lossy(&line_bytes);
                        let Some(data) = line.strip_prefix("data: ") else {
                            continue;
                        };
                        let data = data.trim();

                        if data == "[DONE]" {
                            yield Ok(OpenAiSseEvent::Done);
                            return;
                        }

                        match serde_json::from_str::<ResponseStreamEvent>(data) {
                            Ok(event) => yield Ok(OpenAiSseEvent::Event(event)),
                            Err(error) => tracing::warn!("Failed to parse SSE event: {error}"),
                        }
                    }
                }
                Some(Err(error)) => {
                    yield Err(StreamError::Connection(error.to_string()));
                    return;
                }
                None => return,
            }
        }
    }
}


