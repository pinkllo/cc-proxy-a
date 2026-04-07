use crate::types::claude::{self, MessagesResponse, ResponseContentBlock, Usage};
use crate::types::openai::ChatCompletionResponse;

/// Convert OpenAI non-streaming response to Claude Messages format
pub fn openai_to_claude(
    response: &ChatCompletionResponse,
    original_model: &str,
    estimated_input_tokens: u32,
) -> MessagesResponse {
    let choice = response.choices.first();
    let message = choice.map(|c| &c.message);

    let mut content_blocks = Vec::new();

    // Text content
    if let Some(text) = message.and_then(|m| m.content.as_deref()) {
        if !text.is_empty() {
            content_blocks.push(ResponseContentBlock::Text {
                text: text.to_string(),
            });
        }
    }

    // Tool calls
    if let Some(tool_calls) = message.and_then(|m| m.tool_calls.as_ref()) {
        for tc in tool_calls {
            if tc.call_type == "function" {
                let input = serde_json::from_str(&tc.function.arguments).unwrap_or_else(
                    |_| serde_json::json!({"raw_arguments": tc.function.arguments}),
                );

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
        content_blocks.push(ResponseContentBlock::Text {
            text: String::new(),
        });
    }

    // Map finish reason
    let has_tool_calls = message
        .and_then(|m| m.tool_calls.as_ref())
        .is_some_and(|tc| !tc.is_empty());

    let finish_reason = choice
        .and_then(|c| c.finish_reason.as_deref())
        .unwrap_or("stop");
    let stop_reason = if has_tool_calls {
        // Force tool_use if response contains tool calls, regardless of finish_reason
        claude::stop_reason::TOOL_USE
    } else {
        match finish_reason {
            "stop" => claude::stop_reason::END_TURN,
            "length" => claude::stop_reason::MAX_TOKENS,
            "tool_calls" | "function_call" => claude::stop_reason::TOOL_USE,
            _ => claude::stop_reason::END_TURN,
        }
    };

    let usage = response
        .usage
        .as_ref()
        .map(|u| {
            // Use min(tiktoken, upstream) to avoid over-reporting.
            let report_input = if estimated_input_tokens > 0 && u.prompt_tokens > 0 {
                estimated_input_tokens.min(u.prompt_tokens)
            } else if estimated_input_tokens > 0 {
                estimated_input_tokens
            } else {
                u.prompt_tokens
            };
            let cache_ratio = if u.prompt_tokens > 0 {
                u.prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .unwrap_or(0) as f64
                    / u.prompt_tokens as f64
            } else {
                0.0
            };
            Usage {
                input_tokens: report_input,
                output_tokens: u.completion_tokens,
                cache_read_input_tokens: if cache_ratio > 0.0 {
                    Some((report_input as f64 * cache_ratio).round() as u32)
                } else {
                    None
                },
            }
        })
        .unwrap_or_default();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::openai::*;

    // ---- F22-1: Normal text response ----

    #[test]
    fn test_normal_text_response() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-abc".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("Hello, how can I help?".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(ResponseUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                prompt_tokens_details: None,
            }),
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);

        assert_eq!(result.id, "chatcmpl-abc");
        assert_eq!(result.response_type, "message");
        assert_eq!(result.role, "assistant");
        assert_eq!(result.model, "claude-3-5-sonnet-20241022");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            claude::ResponseContentBlock::Text { text } => {
                assert_eq!(text, "Hello, how can I help?");
            }
            _ => panic!("Expected text content block"),
        }

        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 20);
    }

    // ---- F22-2: Tool call response ----

    #[test]
    fn test_tool_call_response_stop_reason() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-tool".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_abc123".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "get_weather".into(),
                            arguments: r#"{"location":"Tokyo"}"#.into(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);

        // Must be "tool_use" regardless of the OpenAI finish_reason value
        assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            claude::ResponseContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc123");
                assert_eq!(name, "get_weather");
                assert_eq!(input["location"], "Tokyo");
            }
            _ => panic!("Expected tool_use content block"),
        }
    }

    #[test]
    fn test_tool_call_with_text_and_stop_finish_reason() {
        // Even if finish_reason is "stop", presence of tool_calls forces "tool_use"
        let response = ChatCompletionResponse {
            id: "chatcmpl-mixed".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("Calling tool...".into()),
                    tool_calls: Some(vec![ToolCall {
                        id: "call_xyz".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "search".into(),
                            arguments: r#"{"q":"rust"}"#.into(),
                        },
                    }]),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(result.content.len(), 2); // text + tool_use
    }

    // ---- F22-3: Empty choices array ----

    #[test]
    fn test_empty_choices() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-empty".into(),
            choices: vec![],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);

        // Should produce at least one empty text block as fallback
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            claude::ResponseContentBlock::Text { text } => assert_eq!(text, ""),
            _ => panic!("Expected empty text fallback block"),
        }
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
    }

    // ---- F22-4: Malformed tool call arguments ----

    #[test]
    fn test_malformed_tool_arguments_uses_raw_fallback() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-bad".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_bad".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "do_thing".into(),
                            arguments: "not valid json {{{".into(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        match &result.content[0] {
            claude::ResponseContentBlock::ToolUse { input, .. } => {
                // Should fall back to {"raw_arguments": "not valid json {{{"}
                assert_eq!(input["raw_arguments"], "not valid json {{{");
            }
            _ => panic!("Expected tool_use block"),
        }
    }

    // ---- F22-5: Usage mapping with cache_read_input_tokens ----

    #[test]
    fn test_usage_with_cache_read_tokens() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-cache".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("cached".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(ResponseUsage {
                prompt_tokens: 500,
                completion_tokens: 100,
                prompt_tokens_details: Some(PromptTokensDetails {
                    cached_tokens: Some(300),
                }),
            }),
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        assert_eq!(result.usage.input_tokens, 10); // estimated, not upstream's 500
        assert_eq!(result.usage.output_tokens, 100);
        // cache_ratio = 300/500 = 0.6, applied to estimated 10 → 6
        assert_eq!(result.usage.cache_read_input_tokens, Some(6));
    }

    #[test]
    fn test_usage_without_cache_details() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-nocache".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("no cache".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(ResponseUsage {
                prompt_tokens: 50,
                completion_tokens: 25,
                prompt_tokens_details: None,
            }),
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        assert_eq!(result.usage.input_tokens, 10); // estimated, not upstream's 50
        assert_eq!(result.usage.output_tokens, 25);
        assert!(result.usage.cache_read_input_tokens.is_none());
    }

    #[test]
    fn test_usage_missing_defaults_to_zero() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-nousage".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("hi".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        assert_eq!(result.usage.input_tokens, 0);
        assert_eq!(result.usage.output_tokens, 0);
    }

    // ---- Finish reason mapping ----

    #[test]
    fn test_finish_reason_length_maps_to_max_tokens() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-len".into(),
            choices: vec![Choice {
                message: ChoiceMessage {
                    content: Some("truncated...".into()),
                    tool_calls: None,
                },
                finish_reason: Some("length".into()),
            }],
            usage: None,
        };

        let result = openai_to_claude(&response, "claude-3-5-sonnet-20241022", 10);
        assert_eq!(result.stop_reason.as_deref(), Some("max_tokens"));
    }
}
