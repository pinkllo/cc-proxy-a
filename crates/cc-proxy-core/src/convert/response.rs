use crate::convert::usage::derive_claude_usage;
use crate::types::claude::{self, MessagesResponse, ResponseContentBlock};
use crate::types::openai::ResponseObject;

/// Convert an OpenAI Responses API payload to Claude Messages format.
pub fn openai_to_claude(
    response: &ResponseObject,
    original_model: &str,
    estimated_input_tokens: u32,
) -> MessagesResponse {
    let mut content = collect_text_blocks(response);
    let has_tool_calls = append_tool_calls(response, &mut content);

    if content.is_empty() {
        content.push(ResponseContentBlock::Text {
            text: String::new(),
        });
    }

    let usage = response
        .usage
        .as_ref()
        .map(|usage| {
            derive_claude_usage(
                estimated_input_tokens,
                usage.input_tokens,
                usage.output_tokens,
                usage
                    .input_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens),
            )
        })
        .unwrap_or_default();

    MessagesResponse {
        id: response.id.clone(),
        response_type: "message".into(),
        role: "assistant".into(),
        model: original_model.into(),
        content,
        stop_reason: Some(resolve_stop_reason(response, has_tool_calls)),
        stop_sequence: None,
        usage,
    }
}

fn collect_text_blocks(response: &ResponseObject) -> Vec<ResponseContentBlock> {
    response
        .output
        .iter()
        .filter(|item| item.item_type == "message" && item.role.as_deref() == Some("assistant"))
        .flat_map(|item| item.content.iter().flatten())
        .filter(|content| content.content_type == "output_text")
        .filter_map(|content| content.text.as_ref())
        .filter(|text| !text.is_empty())
        .map(|text| ResponseContentBlock::Text { text: text.clone() })
        .collect()
}

fn append_tool_calls(response: &ResponseObject, content: &mut Vec<ResponseContentBlock>) -> bool {
    let mut has_tool_calls = false;

    for item in &response.output {
        if item.item_type != "function_call" {
            continue;
        }

        let Some(call_id) = item.call_id.as_ref() else {
            continue;
        };
        let Some(name) = item.name.as_ref() else {
            continue;
        };

        has_tool_calls = true;
        let arguments = item.arguments.as_deref().unwrap_or("{}");
        let input = serde_json::from_str(arguments)
            .unwrap_or_else(|_| serde_json::json!({ "raw_arguments": arguments }));

        content.push(ResponseContentBlock::ToolUse {
            id: call_id.clone(),
            name: name.clone(),
            input,
        });
    }

    has_tool_calls
}

fn resolve_stop_reason(response: &ResponseObject, has_tool_calls: bool) -> String {
    if has_tool_calls {
        return claude::stop_reason::TOOL_USE.into();
    }

    match response
        .incomplete_details
        .as_ref()
        .and_then(|details| details.reason.as_deref())
    {
        Some("max_output_tokens") => claude::stop_reason::MAX_TOKENS.into(),
        _ => claude::stop_reason::END_TURN.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::openai::*;

    #[test]
    fn converts_text_output() {
        let response = ResponseObject {
            id: "resp_1".into(),
            output: vec![ResponseOutputItem {
                item_type: "message".into(),
                role: Some("assistant".into()),
                content: Some(vec![ResponseOutputContent {
                    content_type: "output_text".into(),
                    text: Some("Hello".into()),
                }]),
                call_id: None,
                name: None,
                arguments: None,
                status: None,
            }],
            usage: Some(ResponseUsage {
                input_tokens: 10,
                output_tokens: 20,
                input_tokens_details: None,
            }),
            status: Some("completed".into()),
            incomplete_details: None,
        };

        let result = openai_to_claude(&response, "claude-test", 10);
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(result.usage.output_tokens, 20);
    }

    #[test]
    fn uses_call_id_for_tool_use() {
        let response = ResponseObject {
            id: "resp_2".into(),
            output: vec![ResponseOutputItem {
                item_type: "function_call".into(),
                role: None,
                content: None,
                call_id: Some("call_123".into()),
                name: Some("lookup".into()),
                arguments: Some(r#"{"city":"SF"}"#.into()),
                status: Some("completed".into()),
            }],
            usage: None,
            status: Some("completed".into()),
            incomplete_details: None,
        };

        let result = openai_to_claude(&response, "claude-test", 0);
        assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));

        match &result.content[0] {
            ResponseContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "lookup");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn maps_max_output_tokens_to_max_tokens() {
        let response = ResponseObject {
            id: "resp_3".into(),
            output: vec![],
            usage: None,
            status: Some("incomplete".into()),
            incomplete_details: Some(IncompleteDetails {
                reason: Some("max_output_tokens".into()),
            }),
        };

        let result = openai_to_claude(&response, "claude-test", 0);
        assert_eq!(result.stop_reason.as_deref(), Some("max_tokens"));
    }
}
