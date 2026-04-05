//! SSE streaming converter: OpenAI ChatCompletion chunks -> Claude SSE events.
//!
//! This is the core streaming pipeline of the proxy. It consumes an upstream
//! stream of OpenAI server-sent events and re-emits them as Claude-compatible
//! SSE events, maintaining a small state machine to track text blocks, tool
//! call blocks, and the final message envelope.

use std::collections::HashMap;
use std::pin::Pin;

use axum::response::sse::Event;
use futures::stream::Stream;
use futures::StreamExt;
use serde_json::json;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::types::claude::{sse, stop_reason, Usage};
use crate::types::openai::ChatCompletionChunk;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Parsed upstream event from the OpenAI SSE stream.
#[derive(Debug)]
pub enum OpenAiSseEvent {
    /// A parsed chunk from `data: {...}`.
    Chunk(ChatCompletionChunk),
    /// The terminal `data: [DONE]` sentinel.
    Done,
}

/// Errors that can occur while reading the upstream SSE stream.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("upstream connection error: {0}")]
    Connection(String),
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("upstream closed unexpectedly")]
    UnexpectedEof,
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Tracks a single in-progress tool call being assembled from streamed deltas.
#[derive(Debug)]
struct ToolCallAccumulator {
    /// OpenAI tool call ID (e.g. "call_abc123").
    id: Option<String>,
    /// Function name.
    name: Option<String>,
    /// Accumulated raw JSON argument fragments.
    args_buffer: String,
    /// Whether we already emitted the complete JSON delta.
    json_sent: bool,
    /// The Claude content-block index assigned to this tool call.
    claude_index: Option<usize>,
    /// Whether we already emitted `content_block_start` for this tool call.
    started: bool,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self {
            id: None,
            name: None,
            args_buffer: String::new(),
            json_sent: false,
            claude_index: None,
            started: false,
        }
    }
}

/// Full converter state threaded through the stream via `futures::stream::unfold`.
struct ConverterState<S> {
    upstream: Pin<Box<S>>,
    original_model: String,
    message_id: String,

    /// Index of the initial text content block (always 0).
    text_block_index: usize,
    /// Counter for tool-call blocks appended after the text block.
    tool_block_counter: usize,
    /// Tool calls keyed by their OpenAI delta index.
    tool_calls: HashMap<usize, ToolCallAccumulator>,
    /// Final stop reason collected from finish_reason.
    final_stop_reason: String,
    /// Usage data captured from the final chunk.
    usage: Usage,

    /// Phases: Prologue -> Streaming -> Epilogue -> Done
    phase: Phase,
}

#[derive(Debug, PartialEq)]
enum Phase {
    /// Emit the three opening events (message_start, content_block_start, ping).
    Prologue,
    /// Process upstream chunks one at a time.
    Streaming,
    /// Emit the closing events (content_block_stop(s), message_delta, message_stop).
    Epilogue,
    /// Terminal — no more events.
    Done,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Convert an OpenAI streaming response into a Claude-compatible SSE event stream.
///
/// The returned stream yields `Result<Event, Infallible>` — errors from the
/// upstream are converted into SSE error events rather than stream termination,
/// so the consumer always sees well-formed SSE.
pub fn openai_stream_to_claude(
    upstream: impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send + 'static,
    original_model: String,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + Send {
    let message_id = generate_message_id();

    let state = ConverterState {
        upstream: Box::pin(upstream),
        original_model,
        message_id,
        text_block_index: 0,
        tool_block_counter: 0,
        tool_calls: HashMap::new(),
        final_stop_reason: stop_reason::END_TURN.to_string(),
        usage: Usage::default(),
        phase: Phase::Prologue,
    };

    // We collect events into a VecDeque because a single upstream chunk can
    // produce 0-N output events. `unfold` yields one item at a time, so we
    // buffer them internally.
    futures::stream::unfold(
        (state, std::collections::VecDeque::<Event>::new()),
        |(mut state, mut buf)| async move {
            loop {
                // Drain buffer first.
                if let Some(event) = buf.pop_front() {
                    return Some((Ok(event), (state, buf)));
                }

                match state.phase {
                    Phase::Prologue => {
                        emit_prologue(&state, &mut buf);
                        state.phase = Phase::Streaming;
                        // Loop back to drain buffer.
                    }
                    Phase::Streaming => {
                        match state.upstream.next().await {
                            Some(Ok(OpenAiSseEvent::Chunk(chunk))) => {
                                process_chunk(&mut state, &chunk, &mut buf);
                                // Loop back to drain buffer.
                            }
                            Some(Ok(OpenAiSseEvent::Done)) => {
                                debug!("upstream stream done");
                                state.phase = Phase::Epilogue;
                            }
                            Some(Err(e)) => {
                                error!("upstream stream error: {e}");
                                emit_error_event(&e.to_string(), &mut buf);
                                // After error, skip to Done (no epilogue — stream is broken).
                                state.phase = Phase::Done;
                            }
                            None => {
                                // Stream ended without [DONE]. Treat as normal completion.
                                warn!("upstream stream ended without [DONE] sentinel");
                                state.phase = Phase::Epilogue;
                            }
                        }
                    }
                    Phase::Epilogue => {
                        emit_epilogue(&state, &mut buf);
                        state.phase = Phase::Done;
                    }
                    Phase::Done => {
                        return None; // Stream is finished.
                    }
                }
            }
        },
    )
}

// ---------------------------------------------------------------------------
// Prologue: opening three events
// ---------------------------------------------------------------------------

fn emit_prologue(
    state: &ConverterState<impl Stream>,
    buf: &mut std::collections::VecDeque<Event>,
) {
    // 1. message_start — empty message envelope
    let message_start_data = json!({
        "type": sse::MESSAGE_START,
        "message": {
            "id": state.message_id,
            "type": "message",
            "role": "assistant",
            "model": state.original_model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }
    });
    buf.push_back(make_sse(sse::MESSAGE_START, &message_start_data));

    // 2. content_block_start — text block at index 0
    let block_start_data = json!({
        "type": sse::CONTENT_BLOCK_START,
        "index": 0,
        "content_block": {
            "type": "text",
            "text": ""
        }
    });
    buf.push_back(make_sse(sse::CONTENT_BLOCK_START, &block_start_data));

    // 3. ping
    let ping_data = json!({ "type": sse::PING });
    buf.push_back(make_sse(sse::PING, &ping_data));
}

// ---------------------------------------------------------------------------
// Chunk processing: the core state machine step
// ---------------------------------------------------------------------------

fn process_chunk(
    state: &mut ConverterState<impl Stream>,
    chunk: &ChatCompletionChunk,
    buf: &mut std::collections::VecDeque<Event>,
) {
    // Capture usage if present (OpenAI sends this on the final data chunk
    // when stream_options.include_usage is true).
    if let Some(ref usage) = chunk.usage {
        state.usage = Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_input_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
        };
    }

    // Process each choice (typically just one).
    for choice in &chunk.choices {
        let delta = &choice.delta;

        // --- Text delta ---
        if let Some(ref text) = delta.content {
            if !text.is_empty() {
                let data = json!({
                    "type": sse::CONTENT_BLOCK_DELTA,
                    "index": state.text_block_index,
                    "delta": {
                        "type": sse::DELTA_TEXT,
                        "text": text
                    }
                });
                buf.push_back(make_sse(sse::CONTENT_BLOCK_DELTA, &data));
            }
        }

        // --- Tool call deltas ---
        if let Some(ref tool_calls) = delta.tool_calls {
            for tc_delta in tool_calls {
                let tc_index = tc_delta.index;

                // Get or create accumulator for this tool call index.
                let acc = state
                    .tool_calls
                    .entry(tc_index)
                    .or_insert_with(ToolCallAccumulator::new);

                // Update ID if provided.
                if let Some(ref id) = tc_delta.id {
                    acc.id = Some(id.clone());
                }

                // Update function name if provided.
                if let Some(ref func) = tc_delta.function {
                    if let Some(ref name) = func.name {
                        acc.name = Some(name.clone());
                    }
                }

                // Emit content_block_start once we have both id and name.
                if acc.id.is_some() && acc.name.is_some() && !acc.started {
                    state.tool_block_counter += 1;
                    let claude_index = state.text_block_index + state.tool_block_counter;
                    acc.claude_index = Some(claude_index);
                    acc.started = true;

                    let data = json!({
                        "type": sse::CONTENT_BLOCK_START,
                        "index": claude_index,
                        "content_block": {
                            "type": "tool_use",
                            "id": acc.id.as_ref().unwrap(),
                            "name": acc.name.as_ref().unwrap(),
                            "input": {}
                        }
                    });
                    buf.push_back(make_sse(sse::CONTENT_BLOCK_START, &data));
                }

                // Accumulate function arguments and try to emit input_json_delta.
                if let Some(ref func) = tc_delta.function {
                    if let Some(ref args_fragment) = func.arguments {
                        if acc.started {
                            acc.args_buffer.push_str(args_fragment);

                            // Try parsing — emit the full buffer as partial_json once valid.
                            if !acc.json_sent {
                                if serde_json::from_str::<serde_json::Value>(&acc.args_buffer)
                                    .is_ok()
                                {
                                    let data = json!({
                                        "type": sse::CONTENT_BLOCK_DELTA,
                                        "index": acc.claude_index.unwrap(),
                                        "delta": {
                                            "type": sse::DELTA_INPUT_JSON,
                                            "partial_json": acc.args_buffer
                                        }
                                    });
                                    buf.push_back(
                                        make_sse(sse::CONTENT_BLOCK_DELTA, &data),
                                    );
                                    acc.json_sent = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- Finish reason ---
        if let Some(ref reason) = choice.finish_reason {
            state.final_stop_reason = map_finish_reason(reason);
        }
    }
}

// ---------------------------------------------------------------------------
// Epilogue: closing events
// ---------------------------------------------------------------------------

fn emit_epilogue(
    state: &ConverterState<impl Stream>,
    buf: &mut std::collections::VecDeque<Event>,
) {
    // 1. content_block_stop for the text block.
    let text_stop = json!({
        "type": sse::CONTENT_BLOCK_STOP,
        "index": state.text_block_index
    });
    buf.push_back(make_sse(sse::CONTENT_BLOCK_STOP, &text_stop));

    // 2. content_block_stop for each started tool call block.
    // Sort by OpenAI index for deterministic ordering.
    let mut tool_indices: Vec<usize> = state.tool_calls.keys().copied().collect();
    tool_indices.sort();
    for idx in tool_indices {
        let acc = &state.tool_calls[&idx];
        if acc.started {
            if let Some(claude_idx) = acc.claude_index {
                let tool_stop = json!({
                    "type": sse::CONTENT_BLOCK_STOP,
                    "index": claude_idx
                });
                buf.push_back(make_sse(sse::CONTENT_BLOCK_STOP, &tool_stop));
            }
        }
    }

    // 3. message_delta with stop_reason and usage.
    let usage_data = json!({
        "input_tokens": state.usage.input_tokens,
        "output_tokens": state.usage.output_tokens,
        "cache_read_input_tokens": state.usage.cache_read_input_tokens.unwrap_or(0)
    });
    let message_delta = json!({
        "type": sse::MESSAGE_DELTA,
        "delta": {
            "stop_reason": state.final_stop_reason,
            "stop_sequence": null
        },
        "usage": usage_data
    });
    buf.push_back(make_sse(sse::MESSAGE_DELTA, &message_delta));

    // 4. message_stop — final event.
    let message_stop = json!({ "type": sse::MESSAGE_STOP });
    buf.push_back(make_sse(sse::MESSAGE_STOP, &message_stop));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an axum SSE `Event` with the given event name and JSON data.
fn make_sse(event_name: &str, data: &serde_json::Value) -> Event {
    // serde_json::to_string produces UTF-8 (no ASCII escaping) by default,
    // matching the Python `ensure_ascii=False` behavior.
    let json_str = serde_json::to_string(data).unwrap_or_else(|e| {
        error!("failed to serialize SSE data: {e}");
        format!(r#"{{"type":"error","error":{{"type":"serialization_error","message":"{}"}}}}"#, e)
    });
    Event::default().event(event_name).data(json_str)
}

/// Emit an SSE error event (non-fatal — the stream continues to the next phase
/// or terminates, but the consumer still sees a well-formed event).
fn emit_error_event(
    message: &str,
    buf: &mut std::collections::VecDeque<Event>,
) {
    let data = json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": format!("Streaming error: {message}")
        }
    });
    buf.push_back(make_sse("error", &data));
}

/// Map OpenAI `finish_reason` to Claude `stop_reason`.
fn map_finish_reason(reason: &str) -> String {
    match reason {
        "stop" => stop_reason::END_TURN,
        "length" => stop_reason::MAX_TOKENS,
        "tool_calls" | "function_call" => stop_reason::TOOL_USE,
        _ => stop_reason::END_TURN,
    }
    .to_string()
}

/// Generate a Claude-style message ID: `msg_` + 24 hex characters.
fn generate_message_id() -> String {
    let uuid_hex = Uuid::new_v4().simple().to_string(); // 32 hex chars
    format!("msg_{}", &uuid_hex[..24])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::openai::*;
    use futures::stream;

    /// Helper to collect all events from the converter.
    async fn collect_events(
        events: Vec<Result<OpenAiSseEvent, StreamError>>,
        model: &str,
    ) -> Vec<Event> {
        let upstream = stream::iter(events);
        let output = openai_stream_to_claude(upstream, model.to_string());
        futures::pin_mut!(output);
        let mut results = Vec::new();
        while let Some(Ok(event)) = output.next().await {
            results.push(event);
        }
        results
    }

    fn make_text_chunk(text: &str) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "chatcmpl-test".into(),
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: Some(text.to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        }
    }

    fn make_finish_chunk(reason: &str) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "chatcmpl-test".into(),
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some(reason.to_string()),
            }],
            usage: None,
        }
    }

    #[tokio::test]
    async fn test_simple_text_stream() {
        let events = vec![
            Ok(OpenAiSseEvent::Chunk(make_text_chunk("Hello"))),
            Ok(OpenAiSseEvent::Chunk(make_text_chunk(" world"))),
            Ok(OpenAiSseEvent::Chunk(make_finish_chunk("stop"))),
            Ok(OpenAiSseEvent::Done),
        ];

        let result = collect_events(events, "claude-3-opus-20240229").await;

        // Prologue: message_start + content_block_start + ping = 3
        // Streaming: 2 text deltas (one per text chunk, finish chunk has no content)
        // Epilogue: content_block_stop + message_delta + message_stop = 3
        // Total = 3 + 2 + 3 = 8
        assert_eq!(result.len(), 8, "expected 8 events, got {}", result.len());
    }

    #[tokio::test]
    async fn test_empty_stream() {
        // Only [DONE], no content at all.
        let events = vec![Ok(OpenAiSseEvent::Done)];
        let result = collect_events(events, "claude-3-opus-20240229").await;

        // Prologue(3) + Epilogue(3) = 6
        assert_eq!(result.len(), 6, "expected 6 events for empty stream");
    }

    #[tokio::test]
    async fn test_tool_call_stream() {
        let events = vec![
            // First chunk: tool call starts with id + name
            Ok(OpenAiSseEvent::Chunk(ChatCompletionChunk {
                id: "chatcmpl-tool".into(),
                choices: vec![ChunkChoice {
                    delta: ChunkDelta {
                        content: None,
                        tool_calls: Some(vec![ChunkToolCall {
                            index: 0,
                            id: Some("call_abc123".into()),
                            function: Some(ChunkFunction {
                                name: Some("get_weather".into()),
                                arguments: Some(r#"{"lo"#.into()),
                            }),
                        }]),
                    },
                    finish_reason: None,
                }],
                usage: None,
            })),
            // Second chunk: more arguments
            Ok(OpenAiSseEvent::Chunk(ChatCompletionChunk {
                id: "chatcmpl-tool".into(),
                choices: vec![ChunkChoice {
                    delta: ChunkDelta {
                        content: None,
                        tool_calls: Some(vec![ChunkToolCall {
                            index: 0,
                            id: None,
                            function: Some(ChunkFunction {
                                name: None,
                                arguments: Some(r#"cation":"SF"}"#.into()),
                            }),
                        }]),
                    },
                    finish_reason: None,
                }],
                usage: None,
            })),
            Ok(OpenAiSseEvent::Chunk(make_finish_chunk("tool_calls"))),
            Ok(OpenAiSseEvent::Done),
        ];

        let result = collect_events(events, "claude-3-opus-20240229").await;

        // Prologue(3) + content_block_start(tool) + input_json_delta + finish(nothing)
        // Epilogue: text_stop + tool_stop + message_delta + message_stop = 4
        // Total = 3 + 1 (tool block start) + 1 (json delta) + 4 = 9
        assert_eq!(result.len(), 9, "expected 9 events for tool call stream, got {}", result.len());
    }

    #[tokio::test]
    async fn test_error_event() {
        let events = vec![
            Ok(OpenAiSseEvent::Chunk(make_text_chunk("Hi"))),
            Err(StreamError::Connection("connection reset".into())),
        ];

        let result = collect_events(events, "test-model").await;

        // Prologue(3) + text_delta(1) + error(1) = 5
        // No epilogue after error.
        assert_eq!(result.len(), 5, "expected 5 events with error, got {}", result.len());
    }

    #[tokio::test]
    async fn test_message_id_format() {
        let id = generate_message_id();
        assert!(id.starts_with("msg_"), "ID should start with msg_");
        // "msg_" (4) + 24 hex chars = 28 total
        assert_eq!(id.len(), 28, "ID should be 28 chars, got {}", id.len());
        // Everything after "msg_" should be hex.
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_finish_reason_mapping() {
        assert_eq!(map_finish_reason("stop"), "end_turn");
        assert_eq!(map_finish_reason("length"), "max_tokens");
        assert_eq!(map_finish_reason("tool_calls"), "tool_use");
        assert_eq!(map_finish_reason("function_call"), "tool_use");
        assert_eq!(map_finish_reason("content_filter"), "end_turn");
    }
}
