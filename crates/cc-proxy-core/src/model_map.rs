use crate::config::ProxyConfig;

/// Map Claude model names to configured OpenAI-compatible model names
pub fn map_model(claude_model: &str, config: &ProxyConfig) -> String {
    // Already an OpenAI/provider model — pass through
    let prefixes = ["gpt-", "o1-", "o3-", "ep-", "doubao-", "deepseek-"];
    if prefixes.iter().any(|p| claude_model.starts_with(p)) {
        return claude_model.to_string();
    }

    let lower = claude_model.to_lowercase();
    if lower.contains("haiku") {
        config.small_model.clone()
    } else if lower.contains("sonnet") {
        config.effective_middle_model().to_string()
    } else if lower.contains("opus") {
        config.big_model.clone()
    } else {
        // Unknown model — default to big
        config.big_model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ProxyConfig {
        ProxyConfig {
            openai_api_key: "test".into(),
            openai_base_url: "https://api.openai.com/v1".into(),
            big_model: "gpt-4o".into(),
            middle_model: Some("gpt-4o".into()),
            small_model: "gpt-4o-mini".into(),
            host: "0.0.0.0".into(),
            port: 8082,
            anthropic_api_key: None,
            azure_api_version: None,
            log_level: "info".into(),
            max_tokens_limit: 4096,
            min_tokens_limit: 100,
            request_timeout: 90,
            custom_headers: Default::default(),
        }
    }

    #[test]
    fn test_haiku_mapping() {
        let cfg = test_config();
        assert_eq!(map_model("claude-3-5-haiku-20241022", &cfg), "gpt-4o-mini");
    }

    #[test]
    fn test_sonnet_mapping() {
        let cfg = test_config();
        assert_eq!(map_model("claude-3-5-sonnet-20241022", &cfg), "gpt-4o");
    }

    #[test]
    fn test_opus_mapping() {
        let cfg = test_config();
        assert_eq!(map_model("claude-3-opus-20240229", &cfg), "gpt-4o");
    }

    #[test]
    fn test_passthrough() {
        let cfg = test_config();
        assert_eq!(map_model("gpt-4o", &cfg), "gpt-4o");
        assert_eq!(map_model("deepseek-chat", &cfg), "deepseek-chat");
    }
}
