//! Precise token counting using tiktoken BPE tokenizer.
//!
//! Uses the same tokenizer family as OpenAI (o200k_base) to count tokens
//! from the original Claude-format request BEFORE OpenAI conversion.
//! This gives Claude Code accurate context window tracking.

use tiktoken_rs::o200k_base;

use crate::types::claude::{
    ContentBlock, MessageContent, MessagesRequest, SystemContent, ToolResultContent,
};

/// Count input tokens for a Claude Messages API request using tiktoken BPE.
///
/// Extracts all text content from system prompt, messages, and tool definitions,
/// then counts tokens with the o200k_base tokenizer (GPT-4o / GPT-5 family).
pub fn count_request_tokens(request: &MessagesRequest) -> u32 {
    let bpe = match o200k_base() {
        Ok(bpe) => bpe,
        Err(_) => {
            tracing::error!("failed to initialize tiktoken o200k_base tokenizer");
            return 0;
        }
    };

    let mut segments: Vec<&str> = Vec::with_capacity(64);

    // System prompt
    if let Some(ref system) = request.system {
        match system {
            SystemContent::Text(s) => segments.push(s),
            SystemContent::Blocks(blocks) => {
                for b in blocks {
                    if let Some(ref text) = b.text {
                        segments.push(text);
                    }
                }
            }
        }
    }

    // Messages
    for msg in &request.messages {
        // Role token overhead (~1 token per message)
        segments.push(&msg.role);
        match &msg.content {
            MessageContent::Text(s) => segments.push(s),
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => segments.push(text),
                        ContentBlock::ToolUse { name, .. } => {
                            segments.push(name);
                            // Tool input JSON counted via owned_segments below
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            if let Some(ref c) = content {
                                match c {
                                    ToolResultContent::Text(s) => segments.push(s),
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            MessageContent::Null => {}
        }
    }

    // Collect owned strings that need counting (tool inputs, tool schemas)
    let mut owned_segments: Vec<String> = Vec::new();

    // Tool use inputs from messages
    for msg in &request.messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolUse { input, .. } = block {
                    owned_segments.push(serde_json::to_string(input).unwrap_or_default());
                }
                if let ContentBlock::ToolResult {
                    content: Some(ToolResultContent::Blocks(items)),
                    ..
                } = block
                {
                    for item in items {
                        owned_segments.push(serde_json::to_string(item).unwrap_or_default());
                    }
                }
                if let ContentBlock::ToolResult {
                    content: Some(ToolResultContent::Object(v)),
                    ..
                } = block
                {
                    owned_segments.push(serde_json::to_string(v).unwrap_or_default());
                }
            }
        }
    }

    // Tool definitions
    if let Some(ref tools) = request.tools {
        for tool in tools {
            owned_segments.push(tool.name.clone());
            if let Some(ref desc) = tool.description {
                owned_segments.push(desc.clone());
            }
            owned_segments.push(serde_json::to_string(&tool.input_schema).unwrap_or_default());
        }
    }

    // Join all segments and count with tiktoken
    let all_text: String = segments
        .iter()
        .copied()
        .chain(owned_segments.iter().map(|s| s.as_str()))
        .collect::<Vec<&str>>()
        .join("\n");

    let tokens = bpe.encode_with_special_tokens(&all_text);

    // Add per-message overhead (~4 tokens per message for role/formatting)
    let overhead = (request.messages.len() as u32) * 4;

    (tokens.len() as u32) + overhead
}
