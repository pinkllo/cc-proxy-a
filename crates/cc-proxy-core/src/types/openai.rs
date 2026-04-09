use serde::{Deserialize, Serialize};

// ===== Request Types =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRequest {
    pub model: String,
    pub input: Vec<ResponseInputItem>,
    pub max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseInputItem {
    Message(ResponseInputMessage),
    FunctionCall(ResponseFunctionCallInput),
    FunctionCallOutput(ResponseFunctionCallOutput),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseInputMessage {
    pub role: String,
    pub content: ResponseMessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseMessageContent {
    Text(String),
    Parts(Vec<InputContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputContentPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image")]
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFunctionCallInput {
    #[serde(rename = "type")]
    pub item_type: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFunctionCallOutput {
    #[serde(rename = "type")]
    pub item_type: String,
    pub call_id: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningConfig {
    pub effort: String,
}

// ===== Response Types =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseObject {
    pub id: String,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub usage: Option<ResponseUsage>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<Vec<ResponseOutputContent>>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseOutputContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub input_tokens_details: Option<InputTokensDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseApiError {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error_type: Option<String>,
}

// ===== Streaming Types =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub output_index: Option<usize>,
    #[serde(default)]
    pub item: Option<ResponseOutputItem>,
    #[serde(default)]
    pub response: Option<ResponseObject>,
    #[serde(default)]
    pub error: Option<ResponseApiError>,
}
