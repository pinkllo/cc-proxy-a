use crate::types::claude::{self, MessagesResponse, ResponseContentBlock, Usage};
use crate::types::openai::ChatCompletionResponse;

/// Convert OpenAI non-streaming response to Claude Messages format
pub fn openai_to_claude(response: &ChatCompletionResponse, original_model: &str) -> MessagesResponse {
    let choice = response.choices.first();
    let message = choice.map(|c| &c.message);

    let mut content_blocks = Vec::new();

    // Text content
    if let Some(text) = message.and_then(|m| m.content.as_deref()) {
        if !text.is_empty() {
            content_blocks.push(ResponseContentBlock::Text { text: text.to_string() });
        }
    }

    // Tool calls
    if let Some(tool_calls) = message.and_then(|m| m.tool_calls.as_ref()) {
        for tc in tool_calls {
            if tc.call_type == "function" {
                let input = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({"raw_arguments": tc.function.arguments}));

                content_blocks.push(ResponseContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    input,
                });
            }
        }
    }

    // Ensure at least one content block
    if content_blocks.is_empty() {
        content_blocks.push(ResponseContentBlock::Text { text: String::new() });
    }

    // Map finish reason
    let finish_reason = choice.and_then(|c| c.finish_reason.as_deref()).unwrap_or("stop");
    let stop_reason = match finish_reason {
        "stop" => claude::stop_reason::END_TURN,
        "length" => claude::stop_reason::MAX_TOKENS,
        "tool_calls" | "function_call" => claude::stop_reason::TOOL_USE,
        _ => claude::stop_reason::END_TURN,
    };

    let usage = response.usage.as_ref().map(|u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_read_input_tokens: u.prompt_tokens_details.as_ref()
            .and_then(|d| d.cached_tokens),
    }).unwrap_or_default();

    MessagesResponse {
        id: response.id.clone(),
        response_type: "message".into(),
        role: "assistant".into(),
        model: original_model.to_string(),
        content: content_blocks,
        stop_reason: Some(stop_reason.to_string()),
        stop_sequence: None,
        usage,
    }
}
