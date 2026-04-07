use anyhow::Result;
use cc_proxy_core::config::ProxyConfig;

pub async fn run() -> Result<()> {
    let config = load_config()?;

    // Print startup banner
    println!("🚀 cc-proxy v{}", env!("CARGO_PKG_VERSION"));
    println!("   Base URL:     {}", config.openai_base_url);
    println!(
        "   Big Model:    {} (reasoning: {})",
        config.big_model,
        config.big_reasoning.as_deref().unwrap_or("none")
    );
    println!(
        "   Middle Model: {} (reasoning: {})",
        config.effective_middle_model(),
        config.middle_reasoning.as_deref().unwrap_or("none")
    );
    println!(
        "   Small Model:  {} (reasoning: {})",
        config.small_model,
        config.small_reasoning.as_deref().unwrap_or("none")
    );
    println!("   Server:       {}:{}", config.host, config.port);
    println!(
        "   Auth:         {}",
        if config.anthropic_api_key.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!();

    cc_proxy_core::server::serve(config).await?;
    Ok(())
}

/// Load config: config.json first, then env vars fallback
pub fn load_config() -> Result<ProxyConfig> {
    let config_path = ProxyConfig::default_config_path();
    if config_path.exists() {
        ProxyConfig::load_from_file(&config_path).map_err(|e| anyhow::anyhow!("{e}"))
    } else {
        ProxyConfig::load().map_err(|e| anyhow::anyhow!("{e}"))
    }
}
