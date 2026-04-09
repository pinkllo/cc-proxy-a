use std::path::PathBuf;

use crate::error::ProxyError;

pub fn settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude")
        .join("settings.json")
}

pub fn claude_code_installed() -> bool {
    settings_path().exists()
}

pub fn is_configured() -> bool {
    let path = settings_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };

    val.get("env")
        .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
        .and_then(|value| value.as_str())
        .is_some_and(|url| url.contains("localhost"))
}

pub fn configure(port: u16, auth_key: &str) -> Result<(), ProxyError> {
    let path = settings_path();
    let mut settings = load_or_create_settings(&path)?;

    if settings.get("env").is_none() {
        settings["env"] = serde_json::json!({});
    }

    let env = settings["env"]
        .as_object_mut()
        .ok_or_else(|| ProxyError::Config("Claude settings env is not an object".into()))?;

    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        serde_json::json!(format!("http://localhost:{port}")),
    );
    env.insert("ANTHROPIC_API_KEY".into(), serde_json::json!(auth_key));
    env.insert("ANTHROPIC_AUTH_TOKEN".into(), serde_json::json!(""));

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| ProxyError::Config(format!("Failed to serialize Claude settings: {e}")))?;
    std::fs::write(&path, content)
        .map_err(|e| ProxyError::Config(format!("Failed to write Claude settings: {e}")))?;
    Ok(())
}

fn load_or_create_settings(path: &PathBuf) -> Result<serde_json::Value, ProxyError> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ProxyError::Config(format!("Failed to read Claude settings: {e}")))?;
        let value = serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
        return Ok(value);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ProxyError::Config(format!("Failed to create .claude dir: {e}")))?;
    }

    Ok(serde_json::json!({}))
}
