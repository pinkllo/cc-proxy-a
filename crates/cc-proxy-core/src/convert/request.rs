use crate::config::ProxyConfig;
use crate::model_map;
use crate::types::claude::{
    ContentBlock, Message, MessageContent, MessagesRequest, SystemContent, ToolResultContent,
};
use crate::types::openai::*;

#[derive(Debug, Clone, Default)]
pub struct RequestConversionOptions {
    pub input_messages: Option<Vec<Message>>,
    pub previous_response_id: Option<String>,
    pub prompt_cache_key: Option<String>,
    pub prompt_cache_retention: Option<String>,
}

/// Convert a Claude Messages API request to OpenAI Responses format.
pub fn claude_to_openai(req: &MessagesRequest, config: &ProxyConfig) -> ResponseRequest {
    claude_to_openai_with_options(req, config, RequestConversionOptions::default())
}

pub fn claude_to_openai_with_options(
    req: &MessagesRequest,
    config: &ProxyConfig,
    options: RequestConversionOptions,
) -> ResponseRequest {
    let mapped = model_map::map_model(&req.model, config);
    let max_output_tokens = req
        .max_tokens
        .max(config.min_tokens_limit)
        .min(config.max_tokens_limit);
    let instructions = extract_system_text(req.system.as_ref()).filter(|text| !text.is_empty());
    let input_messages = options.input_messages.as_deref().unwrap_or(&req.messages);

    ResponseRequest {
        model: mapped.model,
        input: convert_input_items(input_messages),
        max_output_tokens,
        instructions,
        temperature: req.temperature,
        top_p: req.top_p,
        stream: req.stream.unwrap_or(false),
        tools: convert_tools(req.tools.as_ref()),
        tool_choice: req.tool_choice.as_ref().map(convert_tool_choice),
        reasoning: resolve_reasoning_effort(req, config, mapped.tier)
            .map(|effort| ReasoningConfig { effort }),
        previous_response_id: options.previous_response_id,
        prompt_cache_key: options.prompt_cache_key,
        prompt_cache_retention: options.prompt_cache_retention,
    }
}

fn convert_input_items(messages: &[Message]) -> Vec<ResponseInputItem> {
    let mut input = Vec::new();

    for message in messages {
        match message.role.as_str() {
            "user" => append_user_items(message, &mut input),
            "assistant" => append_assistant_items(message, &mut input),
            _ => {}
        }
    }

    input
}

fn extract_system_text(system: Option<&SystemContent>) -> Option<String> {
    system.map(|system| match system {
        SystemContent::Text(text) => text.trim().to_string(),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim()
            .to_string(),
    })
}

fn append_user_items(message: &Message, input: &mut Vec<ResponseInputItem>) {
    match &message.content {
        MessageContent::Null => input.push(text_message("user", String::new())),
        MessageContent::Text(text) => input.push(text_message("user", text.clone())),
        MessageContent::Blocks(blocks) => {
            let initial_len = input.len();
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        parts.push(InputContentPart::InputText { text: text.clone() })
                    }
                    ContentBlock::Image { source } => append_image_part(source, &mut parts),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        flush_message_parts("user", &mut parts, input);
                        input.push(ResponseInputItem::FunctionCallOutput(
                            ResponseFunctionCallOutput {
                                item_type: "function_call_output".into(),
                                call_id: tool_use_id.clone(),
                                output: serialize_tool_result(content),
                            },
                        ));
                    }
                    _ => {}
                }
            }
            flush_message_parts("user", &mut parts, input);
            if input.len() == initial_len {
                input.push(text_message("user", String::new()));
            }
        }
    }
}

fn append_assistant_items(message: &Message, input: &mut Vec<ResponseInputItem>) {
    match &message.content {
        MessageContent::Null => input.push(text_message("assistant", String::new())),
        MessageContent::Text(text) => input.push(text_message("assistant", text.clone())),
        MessageContent::Blocks(blocks) => {
            let mut text = String::new();
            let mut emitted = false;

            for block in blocks {
                match block {
                    ContentBlock::Text { text: chunk } => text.push_str(chunk),
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input: args,
                    } => {
                        flush_assistant_text(&mut text, input, &mut emitted);
                        input.push(ResponseInputItem::FunctionCall(ResponseFunctionCallInput {
                            item_type: "function_call".into(),
                            call_id: id.clone(),
                            name: name.clone(),
                            arguments: serde_json::to_string(args).unwrap_or_default(),
                        }));
                        emitted = true;
                    }
                    _ => {}
                }
            }

            flush_assistant_text(&mut text, input, &mut emitted);
            if !emitted {
                input.push(text_message("assistant", String::new()));
            }
        }
    }
}

fn append_image_part(
    source: &crate::types::claude::ImageSource,
    parts: &mut Vec<InputContentPart>,
) {
    if source.source_type != "base64" {
        return;
    }
    let Some(media_type) = source.media_type.as_ref() else {
        return;
    };
    let Some(data) = source.data.as_ref() else {
        return;
    };
    parts.push(InputContentPart::InputImage {
        image_url: format!("data:{media_type};base64,{data}"),
    });
}

fn serialize_tool_result(content: &Option<ToolResultContent>) -> String {
    match content {
        Some(ToolResultContent::Text(text)) => text.clone(),
        Some(ToolResultContent::Blocks(items)) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(|value| value.as_str())
                    .map(String::from)
                    .or_else(|| serde_json::to_string(item).ok())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(ToolResultContent::Object(value)) => value
            .get("text")
            .and_then(|text| text.as_str())
            .map(String::from)
            .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
        None => "No content provided".into(),
    }
}

fn flush_message_parts(
    role: &str,
    parts: &mut Vec<InputContentPart>,
    input: &mut Vec<ResponseInputItem>,
) {
    if parts.is_empty() {
        return;
    }

    if parts.len() == 1 {
        if let InputContentPart::InputText { text } = &parts[0] {
            input.push(text_message(role, text.clone()));
            parts.clear();
            return;
        }
    }

    let content = std::mem::take(parts);
    input.push(ResponseInputItem::Message(ResponseInputMessage {
        role: role.into(),
        content: ResponseMessageContent::Parts(content),
    }));
}

fn flush_assistant_text(text: &mut String, input: &mut Vec<ResponseInputItem>, emitted: &mut bool) {
    if text.is_empty() {
        return;
    }
    input.push(text_message("assistant", std::mem::take(text)));
    *emitted = true;
}

fn text_message(role: &str, text: String) -> ResponseInputItem {
    ResponseInputItem::Message(ResponseInputMessage {
        role: role.into(),
        content: ResponseMessageContent::Text(text),
    })
}

fn convert_tools(tools: Option<&Vec<crate::types::claude::Tool>>) -> Option<Vec<ResponseTool>> {
    let tools = tools?
        .iter()
        .filter(|tool| !tool.name.trim().is_empty())
        .map(|tool| ResponseTool {
            tool_type: "function".into(),
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        })
        .collect::<Vec<_>>();

    (!tools.is_empty()).then_some(tools)
}

fn convert_tool_choice(choice: &serde_json::Value) -> serde_json::Value {
    let choice_type = choice
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("auto");

    match choice_type {
        "auto" => serde_json::json!("auto"),
        "any" => serde_json::json!("required"),
        "tool" => choice
            .get("name")
            .and_then(|value| value.as_str())
            .map(|name| serde_json::json!({ "type": "function", "name": name }))
            .unwrap_or_else(|| serde_json::json!("auto")),
        _ => serde_json::json!("auto"),
    }
}

/// Resolve reasoning effort with per-tier support.
pub(crate) fn resolve_reasoning_effort(
    req: &MessagesRequest,
    config: &ProxyConfig,
    tier: Option<crate::config::ModelTier>,
) -> Option<String> {
    if let Some(tier) = tier {
        let tier_effort = config.reasoning_for_tier(tier);
        if tier_effort != "none" {
            return Some(tier_effort.to_string());
        }
    }

    if req
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.enabled)
    {
        return Some(if config.reasoning_effort != "none" {
            config.reasoning_effort.clone()
        } else {
            "medium".into()
        });
    }

    (config.reasoning_effort != "none").then(|| config.reasoning_effort.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::claude::*;

    fn test_config() -> ProxyConfig {
        ProxyConfig {
            openai_api_key: "test-key".into(),
            openai_base_url: "https://api.openai.com/v1".into(),
            big_model: "gpt-4.1".into(),
            middle_model: Some("gpt-4.1".into()),
            small_model: "gpt-4.1-mini".into(),
            host: "0.0.0.0".into(),
            port: 8082,
            anthropic_api_key: None,
            azure_api_version: None,
            log_level: "info".into(),
            max_tokens_limit: 128000,
            min_tokens_limit: 100,
            request_timeout: 600,
            streaming_first_byte_timeout: 300,
            streaming_idle_timeout: 300,
            connect_timeout: 30,
            token_count_scale: 1.0,
            custom_headers: Default::default(),
            reasoning_effort: "none".into(),
            big_reasoning: None,
            middle_reasoning: None,
            small_reasoning: None,
            prompt_cache_retention: None,
            model_pricing: Default::default(),
        }
    }

    fn base_request() -> MessagesRequest {
        MessagesRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            max_tokens: 1024,
            messages: vec![],
            system: None,
            stop_sequences: None,
            stream: None,
            temperature: Some(1.0),
            top_p: None,
            top_k: None,
            metadata: None,
            tools: None,
            tool_choice: None,
            thinking: None,
        }
    }

    #[test]
    fn extracts_system_text_blocks() {
        let text = extract_system_text(Some(&SystemContent::Blocks(vec![
            SystemBlock {
                block_type: "text".into(),
                text: Some("One".into()),
                cache_control: None,
            },
            SystemBlock {
                block_type: "text".into(),
                text: Some("Two".into()),
                cache_control: None,
            },
        ])));
        assert_eq!(text.as_deref(), Some("One\n\nTwo"));
    }

    #[test]
    fn converts_user_blocks_with_image() {
        let message = Message {
            role: "user".into(),
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "Look".into(),
                },
                ContentBlock::Image {
                    source: ImageSource {
                        source_type: "base64".into(),
                        media_type: Some("image/png".into()),
                        data: Some("abc".into()),
                    },
                },
            ]),
        };

        let mut input = Vec::new();
        append_user_items(&message, &mut input);

        match &input[0] {
            ResponseInputItem::Message(ResponseInputMessage {
                content: ResponseMessageContent::Parts(parts),
                ..
            }) => assert_eq!(parts.len(), 2),
            _ => panic!("expected multipart user message"),
        }
    }

    #[test]
    fn converts_assistant_tool_use_to_function_call() {
        let message = Message {
            role: "assistant".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "search".into(),
                input: serde_json::json!({ "q": "rust" }),
            }]),
        };

        let mut input = Vec::new();
        append_assistant_items(&message, &mut input);

        match &input[0] {
            ResponseInputItem::FunctionCall(call) => {
                assert_eq!(call.call_id, "toolu_1");
                assert_eq!(call.name, "search");
            }
            _ => panic!("expected function_call"),
        }
    }

    #[test]
    fn converts_tool_result_to_function_call_output() {
        let message = Message {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: Some(ToolResultContent::Text("done".into())),
            }]),
        };

        let mut input = Vec::new();
        append_user_items(&message, &mut input);

        match &input[0] {
            ResponseInputItem::FunctionCallOutput(output) => {
                assert_eq!(output.call_id, "toolu_1");
                assert_eq!(output.output, "done");
            }
            _ => panic!("expected function_call_output"),
        }
    }

    #[test]
    fn converts_tool_choice_to_responses_shape() {
        let choice = serde_json::json!({ "type": "tool", "name": "lookup" });
        assert_eq!(
            convert_tool_choice(&choice),
            serde_json::json!({ "type": "function", "name": "lookup" })
        );
    }

    #[test]
    fn clamps_max_output_tokens() {
        let config = test_config();
        let mut req = base_request();
        req.max_tokens = 10;
        let result = claude_to_openai(&req, &config);
        assert_eq!(result.max_output_tokens, 100);
    }

    #[test]
    fn thinking_enabled_defaults_to_medium() {
        let config = test_config();
        let mut req = base_request();
        req.thinking = Some(ThinkingConfig { enabled: true });
        let reasoning = resolve_reasoning_effort(&req, &config, None);
        assert_eq!(reasoning.as_deref(), Some("medium"));
    }

    #[test]
    fn full_conversion_sets_instructions_and_stream() {
        let config = test_config();
        let mut req = base_request();
        req.system = Some(SystemContent::Text("You are helpful.".into()));
        req.stream = Some(true);
        req.messages.push(Message {
            role: "user".into(),
            content: MessageContent::Text("Hello".into()),
        });

        let result = claude_to_openai(&req, &config);

        assert_eq!(result.instructions.as_deref(), Some("You are helpful."));
        assert!(result.stream);
        assert_eq!(result.input.len(), 1);
    }

    #[test]
    fn conversion_options_override_input_and_cache_fields() {
        let config = test_config();
        let mut req = base_request();
        req.messages.push(Message {
            role: "user".into(),
            content: MessageContent::Text("old".into()),
        });
        req.messages.push(Message {
            role: "user".into(),
            content: MessageContent::Text("new".into()),
        });

        let result = claude_to_openai_with_options(
            &req,
            &config,
            RequestConversionOptions {
                input_messages: Some(vec![req.messages[1].clone()]),
                previous_response_id: Some("resp_123".into()),
                prompt_cache_key: Some("ccproxy_session".into()),
                prompt_cache_retention: Some("24h".into()),
            },
        );

        assert_eq!(result.previous_response_id.as_deref(), Some("resp_123"));
        assert_eq!(result.prompt_cache_key.as_deref(), Some("ccproxy_session"));
        assert_eq!(result.prompt_cache_retention.as_deref(), Some("24h"));
        assert_eq!(result.input.len(), 1);
    }
}
