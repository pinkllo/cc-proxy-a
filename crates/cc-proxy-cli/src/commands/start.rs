use anyhow::Result;
use cc_proxy_core::config::ProxyConfig;

pub async fn run() -> Result<()> {
    let config = ProxyConfig::load()?;

    // Print startup banner
    println!("🚀 cc-proxy v0.1.0");
    println!("   Base URL:     {}", config.openai_base_url);
    println!("   Big Model:    {}", config.big_model);
    println!("   Middle Model: {}", config.effective_middle_model());
    println!("   Small Model:  {}", config.small_model);
    println!("   Server:       {}:{}", config.host, config.port);
    println!("   Auth:         {}", if config.anthropic_api_key.is_some() { "enabled" } else { "disabled" });
    println!();

    // Start server
    cc_proxy_core::server::serve(config).await?;
    Ok(())
}
