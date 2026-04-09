//! SSE streaming converter: OpenAI Responses events -> Claude SSE events.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::Event;
use futures::stream::Stream;
use futures::StreamExt;
use serde_json::json;
use tokio::time::timeout;
use tracing::{error, warn};
use uuid::Uuid;

use crate::convert::response::openai_to_claude;
use crate::convert::usage::derive_claude_usage;
use crate::types::claude::{sse, stop_reason, Usage};
use crate::types::openai::{ResponseObject, ResponseStreamEvent};

#[derive(Debug)]
pub enum OpenAiSseEvent {
    Event(ResponseStreamEvent),
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("upstream connection error: {0}")]
    Connection(String),
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("upstream closed unexpectedly")]
    UnexpectedEof,
}

pub type StreamObserver = Arc<dyn Fn(StreamSummary) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct StreamSummary {
    pub usage: Usage,
    pub stop_reason: String,
    pub response_id: Option<String>,
    pub had_error: bool,
    pub error_message: Option<String>,
}

#[derive(Debug)]
struct ToolCallAccumulator {
    call_id: Option<String>,
    name: Option<String>,
    args_buffer: String,
    claude_index: Option<usize>,
    started: bool,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self {
            call_id: None,
            name: None,
            args_buffer: String::new(),
            claude_index: None,
            started: false,
        }
    }
}

struct ConverterState<S> {
    upstream: Pin<Box<S>>,
    original_model: String,
    message_id: String,
    text_block_index: usize,
    tool_block_counter: usize,
    tool_calls: HashMap<usize, ToolCallAccumulator>,
    final_stop_reason: String,
    usage: Usage,
    phase: Phase,
    idle_timeout: Duration,
    estimated_input_tokens: u32,
    observer: Option<StreamObserver>,
    had_error: bool,
    error_message: Option<String>,
    response_id: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Phase {
    Prologue,
    Streaming,
    Epilogue,
    Done,
}

pub fn openai_stream_to_claude(
    upstream: impl Stream<Item = Result<OpenAiSseEvent, StreamError>> + Send + 'static,
    original_model: String,
    idle_timeout: Duration,
    estimated_input_tokens: u32,
    observer: Option<StreamObserver>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + Send {
    let state = ConverterState {
        upstream: Box::pin(upstream),
        original_model,
        message_id: generate_message_id(),
        text_block_index: 0,
        tool_block_counter: 0,
        tool_calls: HashMap::new(),
        final_stop_reason: stop_reason::END_TURN.into(),
        usage: Usage::default(),
        phase: Phase::Prologue,
        idle_timeout,
        estimated_input_tokens,
        observer,
        had_error: false,
        error_message: None,
        response_id: None,
    };

    futures::stream::unfold(
        (state, VecDeque::<Event>::new()),
        |(mut state, mut buf)| async move {
            loop {
                if let Some(event) = buf.pop_front() {
                    return Some((Ok(event), (state, buf)));
                }

                match state.phase {
                    Phase::Prologue => {
                        emit_prologue(&state, &mut buf);
                        state.phase = Phase::Streaming;
                    }
                    Phase::Streaming => {
                        let next = if state.idle_timeout.is_zero() {
                            state.upstream.next().await
                        } else {
                            match timeout(state.idle_timeout, state.upstream.next()).await {
                                Ok(result) => result,
                                Err(_) => {
                                    let secs = state.idle_timeout.as_secs();
                                    let message = format!("stream idle timeout ({secs}s)");
                                    mark_stream_error(&mut state, message.clone());
                                    emit_error_event(&message, &mut buf);
                                    state.phase = Phase::Epilogue;
                                    continue;
                                }
                            }
                        };

                        match next {
                            Some(Ok(OpenAiSseEvent::Event(event))) => {
                                process_event(&mut state, event, &mut buf);
                            }
                            Some(Ok(OpenAiSseEvent::Done)) => state.phase = Phase::Epilogue,
                            Some(Err(error)) => {
                                mark_stream_error(&mut state, error.to_string());
                                emit_error_event(&error.to_string(), &mut buf);
                                state.phase = Phase::Epilogue;
                            }
                            None => {
                                warn!("upstream stream ended without terminal event");
                                state.phase = Phase::Epilogue;
                            }
                        }
                    }
                    Phase::Epilogue => {
                        emit_epilogue(&state, &mut buf);
                        state.phase = Phase::Done;
                    }
                    Phase::Done => return None,
                }
            }
        },
    )
}

fn emit_prologue<S>(state: &ConverterState<S>, buf: &mut VecDeque<Event>) {
    buf.push_back(make_sse(
        sse::MESSAGE_START,
        &json!({
            "type": sse::MESSAGE_START,
            "message": {
                "id": state.message_id,
                "type": "message",
                "role": "assistant",
                "model": state.original_model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    ));
    buf.push_back(make_sse(
        sse::CONTENT_BLOCK_START,
        &json!({
            "type": sse::CONTENT_BLOCK_START,
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        }),
    ));
    buf.push_back(make_sse(sse::PING, &json!({ "type": sse::PING })));
}

fn process_event<S>(
    state: &mut ConverterState<S>,
    event: ResponseStreamEvent,
    buf: &mut VecDeque<Event>,
) {
    match event.event_type.as_str() {
        "response.output_text.delta" => emit_text_delta(&event, state.text_block_index, buf),
        "response.output_item.added" | "response.output_item.done" => {
            register_tool_call(state, &event, buf)
        }
        "response.function_call_arguments.delta" => {
            emit_function_arguments_delta(state, &event, buf)
        }
        "response.completed" | "response.incomplete" => {
            if let Some(response) = event.response {
                apply_response_snapshot(state, &response);
            }
            state.phase = Phase::Epilogue;
        }
        "response.failed" => {
            let message = event
                .response
                .as_ref()
                .and_then(|response| response.status.clone())
                .unwrap_or_else(|| "response failed".into());
            mark_stream_error(state, message.clone());
            emit_error_event(&message, buf);
            state.phase = Phase::Epilogue;
        }
        "error" => {
            let message = event
                .error
                .and_then(|error| error.message)
                .unwrap_or_else(|| "unknown streaming error".into());
            mark_stream_error(state, message.clone());
            emit_error_event(&message, buf);
            state.phase = Phase::Epilogue;
        }
        _ => {}
    }
}

fn emit_text_delta(event: &ResponseStreamEvent, text_index: usize, buf: &mut VecDeque<Event>) {
    let Some(delta) = event.delta.as_ref() else {
        return;
    };
    if delta.is_empty() {
        return;
    }

    buf.push_back(make_sse(
        sse::CONTENT_BLOCK_DELTA,
        &json!({
            "type": sse::CONTENT_BLOCK_DELTA,
            "index": text_index,
            "delta": { "type": sse::DELTA_TEXT, "text": delta }
        }),
    ));
}

fn register_tool_call<S>(
    state: &mut ConverterState<S>,
    event: &ResponseStreamEvent,
    buf: &mut VecDeque<Event>,
) {
    let Some(index) = event.output_index else {
        return;
    };
    let Some(item) = event.item.as_ref() else {
        return;
    };
    if item.item_type != "function_call" {
        return;
    }

    let mut start_block = None;
    {
        let accumulator = state
            .tool_calls
            .entry(index)
            .or_insert_with(ToolCallAccumulator::new);

        if let Some(call_id) = item.call_id.as_ref() {
            accumulator.call_id = Some(call_id.clone());
        }
        if let Some(name) = item.name.as_ref() {
            accumulator.name = Some(name.clone());
        }

        if !accumulator.started {
            if let (Some(call_id), Some(name)) =
                (accumulator.call_id.clone(), accumulator.name.clone())
            {
                start_block = Some((call_id, name));
            }
        }
    }

    if let Some((call_id, name)) = start_block {
        state.tool_block_counter += 1;
        let claude_index = state.text_block_index + state.tool_block_counter;
        if let Some(accumulator) = state.tool_calls.get_mut(&index) {
            accumulator.claude_index = Some(claude_index);
            accumulator.started = true;
        }
        buf.push_back(make_sse(
            sse::CONTENT_BLOCK_START,
            &json!({
                "type": sse::CONTENT_BLOCK_START,
                "index": claude_index,
                "content_block": {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": {}
                }
            }),
        ));
    }

    if let Some(arguments) = item.arguments.as_ref() {
        if let Some(accumulator) = state.tool_calls.get_mut(&index) {
            emit_function_arguments(accumulator, arguments, buf);
        }
    }
}

fn emit_function_arguments_delta<S>(
    state: &mut ConverterState<S>,
    event: &ResponseStreamEvent,
    buf: &mut VecDeque<Event>,
) {
    let Some(index) = event.output_index else {
        return;
    };
    let Some(delta) = event.delta.as_ref() else {
        return;
    };
    if delta.is_empty() {
        return;
    }

    let Some(accumulator) = state.tool_calls.get_mut(&index) else {
        return;
    };
    if !accumulator.started {
        return;
    }

    accumulator.args_buffer.push_str(delta);
    let Some(claude_index) = accumulator.claude_index else {
        return;
    };

    buf.push_back(make_sse(
        sse::CONTENT_BLOCK_DELTA,
        &json!({
            "type": sse::CONTENT_BLOCK_DELTA,
            "index": claude_index,
            "delta": { "type": sse::DELTA_INPUT_JSON, "partial_json": delta }
        }),
    ));
}

fn emit_function_arguments(
    accumulator: &mut ToolCallAccumulator,
    arguments: &str,
    buf: &mut VecDeque<Event>,
) {
    if arguments.is_empty() {
        return;
    }
    let Some(claude_index) = accumulator.claude_index else {
        return;
    };
    if accumulator.args_buffer == arguments {
        return;
    }

    let delta = if arguments.starts_with(&accumulator.args_buffer) {
        &arguments[accumulator.args_buffer.len()..]
    } else {
        arguments
    };
    if delta.is_empty() {
        return;
    }

    accumulator.args_buffer = arguments.into();
    buf.push_back(make_sse(
        sse::CONTENT_BLOCK_DELTA,
        &json!({
            "type": sse::CONTENT_BLOCK_DELTA,
            "index": claude_index,
            "delta": { "type": sse::DELTA_INPUT_JSON, "partial_json": delta }
        }),
    ));
}

fn apply_response_snapshot<S>(state: &mut ConverterState<S>, response: &ResponseObject) {
    state.response_id = Some(response.id.clone());
    if let Some(usage) = response.usage.as_ref() {
        state.usage = Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage
                .input_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
            upstream_input_tokens: Some(usage.input_tokens),
        };
    }

    let claude_response = openai_to_claude(
        response,
        &state.original_model,
        state.estimated_input_tokens,
    );
    state.final_stop_reason = claude_response
        .stop_reason
        .unwrap_or_else(|| stop_reason::END_TURN.into());
}

fn emit_epilogue<S>(state: &ConverterState<S>, buf: &mut VecDeque<Event>) {
    buf.push_back(make_sse(
        sse::CONTENT_BLOCK_STOP,
        &json!({ "type": sse::CONTENT_BLOCK_STOP, "index": state.text_block_index }),
    ));

    let mut indices = state.tool_calls.keys().copied().collect::<Vec<_>>();
    indices.sort_unstable();
    for index in indices {
        let accumulator = &state.tool_calls[&index];
        if let Some(claude_index) = accumulator.claude_index {
            buf.push_back(make_sse(
                sse::CONTENT_BLOCK_STOP,
                &json!({ "type": sse::CONTENT_BLOCK_STOP, "index": claude_index }),
            ));
        }
    }

    let usage = derive_claude_usage(
        state.estimated_input_tokens,
        state.usage.input_tokens,
        state.usage.output_tokens,
        state.usage.cache_read_input_tokens,
    );
    buf.push_back(make_sse(
        sse::MESSAGE_DELTA,
        &json!({
            "type": sse::MESSAGE_DELTA,
            "delta": {
                "stop_reason": state.final_stop_reason,
                "stop_sequence": null
            },
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_input_tokens": usage.cache_read_input_tokens.unwrap_or(0)
            }
        }),
    ));
    notify_completion(state, usage, state.final_stop_reason.clone());
    buf.push_back(make_sse(
        sse::MESSAGE_STOP,
        &json!({ "type": sse::MESSAGE_STOP }),
    ));
}

fn make_sse(event_name: &str, data: &serde_json::Value) -> Event {
    let payload = serde_json::to_string(data).unwrap_or_else(|error| {
        error!("failed to serialize SSE data: {error}");
        format!(
            r#"{{"type":"error","error":{{"type":"serialization_error","message":"{}"}}}}"#,
            error
        )
    });
    Event::default().event(event_name).data(payload)
}

fn emit_error_event(message: &str, buf: &mut VecDeque<Event>) {
    buf.push_back(make_sse(
        "error",
        &json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": format!("Streaming error: {message}")
            }
        }),
    ));
}

fn mark_stream_error<S>(state: &mut ConverterState<S>, message: String) {
    state.had_error = true;
    if state.error_message.is_none() {
        state.error_message = Some(message);
    }
}

fn generate_message_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("msg_{}", &uuid[..24])
}

fn notify_completion<S>(state: &ConverterState<S>, usage: Usage, stop_reason: String) {
    let Some(observer) = state.observer.as_ref() else {
        return;
    };
    observer(StreamSummary {
        usage,
        stop_reason,
        response_id: state.response_id.clone(),
        had_error: state.had_error,
        error_message: state.error_message.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::openai::*;
    use futures::stream;

    async fn collect_events(events: Vec<Result<OpenAiSseEvent, StreamError>>) -> Vec<Event> {
        let output = openai_stream_to_claude(
            stream::iter(events),
            "claude-test".into(),
            Duration::ZERO,
            0,
            None,
        );
        futures::pin_mut!(output);
        let mut result = Vec::new();
        while let Some(Ok(event)) = output.next().await {
            result.push(event);
        }
        result
    }

    #[tokio::test]
    async fn streams_text_deltas() {
        let events = vec![
            Ok(OpenAiSseEvent::Event(ResponseStreamEvent {
                event_type: "response.output_text.delta".into(),
                delta: Some("Hello".into()),
                output_index: Some(0),
                item: None,
                response: None,
                error: None,
            })),
            Ok(OpenAiSseEvent::Event(ResponseStreamEvent {
                event_type: "response.completed".into(),
                delta: None,
                output_index: None,
                item: None,
                response: Some(ResponseObject {
                    id: "resp_1".into(),
                    output: vec![],
                    usage: Some(ResponseUsage {
                        input_tokens: 5,
                        output_tokens: 2,
                        input_tokens_details: None,
                    }),
                    status: Some("completed".into()),
                    incomplete_details: None,
                }),
                error: None,
            })),
        ];

        let result = collect_events(events).await;
        assert_eq!(result.len(), 7);
    }

    #[tokio::test]
    async fn streams_function_call_arguments() {
        let events = vec![
            Ok(OpenAiSseEvent::Event(ResponseStreamEvent {
                event_type: "response.output_item.added".into(),
                delta: None,
                output_index: Some(1),
                item: Some(ResponseOutputItem {
                    item_type: "function_call".into(),
                    role: None,
                    content: None,
                    call_id: Some("call_1".into()),
                    name: Some("lookup".into()),
                    arguments: Some("{".into()),
                    status: None,
                }),
                response: None,
                error: None,
            })),
            Ok(OpenAiSseEvent::Event(ResponseStreamEvent {
                event_type: "response.function_call_arguments.delta".into(),
                delta: Some(r#""city":"SF"}"#.into()),
                output_index: Some(1),
                item: None,
                response: None,
                error: None,
            })),
            Ok(OpenAiSseEvent::Event(ResponseStreamEvent {
                event_type: "response.completed".into(),
                delta: None,
                output_index: None,
                item: None,
                response: Some(ResponseObject {
                    id: "resp_2".into(),
                    output: vec![ResponseOutputItem {
                        item_type: "function_call".into(),
                        role: None,
                        content: None,
                        call_id: Some("call_1".into()),
                        name: Some("lookup".into()),
                        arguments: Some(r#"{"city":"SF"}"#.into()),
                        status: Some("completed".into()),
                    }],
                    usage: None,
                    status: Some("completed".into()),
                    incomplete_details: None,
                }),
                error: None,
            })),
        ];

        let result = collect_events(events).await;
        assert_eq!(result.len(), 10);
    }

    #[tokio::test]
    async fn emits_error_then_epilogue() {
        let events = vec![Err(StreamError::Connection("boom".into()))];
        let result = collect_events(events).await;
        assert_eq!(result.len(), 7);
    }

    #[test]
    fn message_id_matches_claude_shape() {
        let id = generate_message_id();
        assert!(id.starts_with("msg_"));
        assert_eq!(id.len(), 28);
    }
}
