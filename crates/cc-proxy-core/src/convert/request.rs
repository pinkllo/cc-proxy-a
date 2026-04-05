use crate::config::ProxyConfig;
use crate::model_map;
use crate::types::claude::{ContentBlock, Message, MessageContent, MessagesRequest, SystemContent, ToolResultContent};
use crate::types::openai::*;

/// Convert a Claude Messages API request to OpenAI Chat Completions format
pub fn claude_to_openai(req: &MessagesRequest, config: &ProxyConfig) -> ChatCompletionRequest {
    let openai_model = model_map::map_model(&req.model, config);
    let mut messages = Vec::new();

    // System message
    if let Some(ref system) = req.system {
        let text = extract_system_text(system);
        if !text.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Some(ChatContent::Text(text)),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }

    // Process messages
    let mut i = 0;
    while i < req.messages.len() {
        let msg = &req.messages[i];

        match msg.role.as_str() {
            "user" => {
                messages.push(convert_user_message(msg));
            }
            "assistant" => {
                messages.push(convert_assistant_message(msg));

                // Check if next message has tool results
                if i + 1 < req.messages.len() {
                    let next = &req.messages[i + 1];
                    if next.role == "user" && has_tool_results(&next.content) {
                        i += 1;
                        let tool_msgs = convert_tool_results(next);
                        messages.extend(tool_msgs);
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    let max_tokens = req.max_tokens
        .max(config.min_tokens_limit)
        .min(config.max_tokens_limit);

    let mut request = ChatCompletionRequest {
        model: openai_model,
        messages,
        max_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        stream: req.stream.unwrap_or(false),
        stop: req.stop_sequences.clone(),
        tools: None,
        tool_choice: None,
        stream_options: None,
    };

    // Convert tools
    if let Some(ref tools) = req.tools {
        let openai_tools: Vec<ChatTool> = tools
            .iter()
            .filter(|t| !t.name.trim().is_empty())
            .map(|t| ChatTool {
                tool_type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect();
        if !openai_tools.is_empty() {
            request.tools = Some(openai_tools);
        }
    }

    // Convert tool_choice
    if let Some(ref choice) = req.tool_choice {
        request.tool_choice = Some(convert_tool_choice(choice));
    }

    // Stream options
    if request.stream {
        request.stream_options = Some(StreamOptions { include_usage: true });
    }

    request
}

fn extract_system_text(system: &SystemContent) -> String {
    match system {
        SystemContent::Text(s) => s.trim().to_string(),
        SystemContent::Blocks(blocks) => {
            blocks
                .iter()
                .filter(|b| b.block_type == "text")
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n\n")
                .trim()
                .to_string()
        }
    }
}

fn convert_user_message(msg: &Message) -> ChatMessage {
    match &msg.content {
        MessageContent::Text(s) => ChatMessage {
            role: "user".into(),
            content: Some(ChatContent::Text(s.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        MessageContent::Blocks(blocks) => {
            let parts: Vec<ContentPart> = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => {
                        Some(ContentPart::Text { text: text.clone() })
                    }
                    ContentBlock::Image { source } => {
                        if source.source_type == "base64" {
                            if let (Some(media), Some(data)) = (&source.media_type, &source.data) {
                                return Some(ContentPart::ImageUrl {
                                    image_url: ImageUrl {
                                        url: format!("data:{media};base64,{data}"),
                                    },
                                });
                            }
                        }
                        None
                    }
                    _ => None,
                })
                .collect();

            // Optimize: single text part → plain string
            if parts.len() == 1 {
                if let ContentPart::Text { ref text } = parts[0] {
                    return ChatMessage {
                        role: "user".into(),
                        content: Some(ChatContent::Text(text.clone())),
                        tool_calls: None,
                        tool_call_id: None,
                    };
                }
            }

            ChatMessage {
                role: "user".into(),
                content: Some(ChatContent::Parts(parts)),
                tool_calls: None,
                tool_call_id: None,
            }
        }
    }
}

fn convert_assistant_message(msg: &Message) -> ChatMessage {
    match &msg.content {
        MessageContent::Text(s) => ChatMessage {
            role: "assistant".into(),
            content: Some(ChatContent::Text(s.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        MessageContent::Blocks(blocks) => {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();

            for block in blocks {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name: name.clone(),
                                arguments: serde_json::to_string(input).unwrap_or_default(),
                            },
                        });
                    }
                    _ => {}
                }
            }

            ChatMessage {
                role: "assistant".into(),
                content: if text_parts.is_empty() { None } else { Some(ChatContent::Text(text_parts.join(""))) },
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id: None,
            }
        }
    }
}

fn has_tool_results(content: &MessageContent) -> bool {
    if let MessageContent::Blocks(blocks) = content {
        blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    } else {
        false
    }
}

fn convert_tool_results(msg: &Message) -> Vec<ChatMessage> {
    let mut results = Vec::new();
    if let MessageContent::Blocks(blocks) = &msg.content {
        for block in blocks {
            if let ContentBlock::ToolResult { tool_use_id, content } = block {
                let text = match content {
                    Some(ToolResultContent::Text(s)) => s.clone(),
                    Some(ToolResultContent::Blocks(items)) => {
                        items.iter()
                            .filter_map(|item| {
                                item.get("text").and_then(|v| v.as_str()).map(String::from)
                                    .or_else(|| serde_json::to_string(item).ok())
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    Some(ToolResultContent::Object(v)) => {
                        v.get("text").and_then(|t| t.as_str()).map(String::from)
                            .unwrap_or_else(|| serde_json::to_string(v).unwrap_or_default())
                    }
                    None => "No content provided".into(),
                };

                results.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(ChatContent::Text(text)),
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
        }
    }
    results
}

fn convert_tool_choice(choice: &serde_json::Value) -> serde_json::Value {
    let choice_type = choice.get("type").and_then(|v| v.as_str()).unwrap_or("auto");
    match choice_type {
        "auto" | "any" => serde_json::json!("auto"),
        "tool" => {
            if let Some(name) = choice.get("name").and_then(|v| v.as_str()) {
                serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                })
            } else {
                serde_json::json!("auto")
            }
        }
        _ => serde_json::json!("auto"),
    }
}
