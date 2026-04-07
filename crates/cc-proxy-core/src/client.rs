use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use tokio::time::timeout;

use crate::config::ProxyConfig;
use crate::convert::stream::{OpenAiSseEvent, StreamError};
use crate::error::ProxyError;
use crate::types::openai::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse};

/// HTTP client for upstream OpenAI-compatible API
#[derive(Clone)]
pub struct UpstreamClient {
    client: reqwest::Client,
    base_url: String,
}

impl UpstreamClient {
    pub fn new(config: &ProxyConfig) -> Result<Self, ProxyError> {
        let mut default_headers = HeaderMap::new();
        default_headers.insert("content-type", HeaderValue::from_static("application/json"));

        // Custom headers
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
            .map_err(|e| ProxyError::Internal(format!("Failed to create HTTP client: {e}")))?;

        // Normalize base URL
        let base_url = config.openai_base_url.trim_end_matches('/').to_string();

        Ok(Self { client, base_url })
    }

    /// Send non-streaming chat completion
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProxyError> {
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let msg = ProxyError::classify_upstream(&body);
            return Err(ProxyError::Internal(msg));
        }

        let resp: ChatCompletionResponse = response.json().await?;
        Ok(resp)
    }

    /// Send streaming chat completion — returns a parsed SSE event stream
    pub async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
        first_byte_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send, ProxyError> {
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let msg = ProxyError::classify_upstream(&body);
            return Err(ProxyError::Internal(msg));
        }

        // Parse the SSE byte stream into OpenAiSseEvent with per-chunk timeouts
        let byte_stream = response.bytes_stream();
        let event_stream = parse_sse_stream(byte_stream, first_byte_timeout, idle_timeout);

        Ok(event_stream)
    }
}

/// Maximum SSE buffer size (4 MB) to prevent unbounded memory growth.
const MAX_SSE_BUFFER: usize = 4 * 1024 * 1024;

/// Parse a raw byte stream (from reqwest) into OpenAiSseEvent items.
///
/// Each chunk read is wrapped in `tokio::time::timeout` to prevent indefinite
/// blocking when the upstream pauses (e.g. during extended thinking).
fn parse_sse_stream(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    first_byte_timeout: Duration,
    idle_timeout: Duration,
) -> impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send {
    // Accumulate raw bytes to avoid UTF-8 boundary corruption (F17)
    let line_stream = async_stream::stream! {
        let mut raw_buffer = Vec::<u8>::new();
        let mut is_first_chunk = true;

        tokio::pin!(byte_stream);
        loop {
            // Pick the right timeout: first-byte for the initial chunk, idle for subsequent ones.
            let timeout_dur = if is_first_chunk { first_byte_timeout } else { idle_timeout };

            let chunk_result = if timeout_dur.is_zero() {
                // Timeout of 0 = disabled, wait indefinitely (backward-compat).
                byte_stream.next().await
            } else {
                match timeout(timeout_dur, byte_stream.next()).await {
                    Ok(result) => result,
                    Err(_) => {
                        let kind = if is_first_chunk { "first-byte" } else { "idle" };
                        tracing::error!(
                            "SSE stream {kind} timeout after {}s",
                            timeout_dur.as_secs()
                        );
                        yield Err(StreamError::Connection(
                            format!("stream {kind} timeout ({}s)", timeout_dur.as_secs()),
                        ));
                        return;
                    }
                }
            };

            match chunk_result {
                Some(Ok(bytes)) => {
                    is_first_chunk = false;
                    raw_buffer.extend_from_slice(&bytes);

                    // Buffer overflow protection
                    if raw_buffer.len() > MAX_SSE_BUFFER {
                        tracing::error!("SSE buffer exceeded {} bytes, aborting stream", MAX_SSE_BUFFER);
                        yield Err(StreamError::Connection("SSE buffer overflow".into()));
                        return;
                    }

                    // Process complete lines (delimited by \n)
                    while let Some(pos) = raw_buffer.iter().position(|&b| b == b'\n') {
                        let mut line_bytes = raw_buffer[..pos].to_vec();
                        raw_buffer = raw_buffer[pos + 1..].to_vec();

                        // Trim \r
                        if line_bytes.last() == Some(&b'\r') {
                            line_bytes.pop();
                        }

                        let line = String::from_utf8_lossy(&line_bytes).to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                yield Ok(OpenAiSseEvent::Done);
                                return;
                            }
                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => yield Ok(OpenAiSseEvent::Chunk(chunk)),
                                Err(e) => {
                                    tracing::warn!("Failed to parse SSE chunk: {e}");
                                    // Skip unparseable chunks rather than failing
                                }
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    yield Err(StreamError::Connection(e.to_string()));
                    return;
                }
                None => {
                    // Stream ended normally.
                    return;
                }
            }
        }
    };

    line_stream
}
