use std::collections::HashMap;

use anyhow::{Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Password, Select};

use cc_proxy_core::config::ProxyConfig;

// ─── Provider Presets ───────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProviderPreset {
    name: &'static str,
    base_url: &'static str,
    big_model: &'static str,
    middle_model: &'static str,
    small_model: &'static str,
    needs_key: bool,
    is_azure: bool,
}

const PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        big_model: "gpt-4o",
        middle_model: "gpt-4o",
        small_model: "gpt-4o-mini",
        needs_key: true,
        is_azure: false,
    },
    ProviderPreset {
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        big_model: "deepseek-chat",
        middle_model: "deepseek-chat",
        small_model: "deepseek-chat",
        needs_key: true,
        is_azure: false,
    },
    ProviderPreset {
        name: "Ollama (本地)",
        base_url: "http://localhost:11434/v1",
        big_model: "qwen2.5:14b",
        middle_model: "qwen2.5:14b",
        small_model: "qwen2.5:7b",
        needs_key: false,
        is_azure: false,
    },
    ProviderPreset {
        name: "Azure OpenAI",
        base_url: "", // built dynamically
        big_model: "gpt-4o",
        middle_model: "gpt-4o",
        small_model: "gpt-4o-mini",
        needs_key: true,
        is_azure: true,
    },
    ProviderPreset {
        name: "自定义 (Custom)",
        base_url: "",
        big_model: "",
        middle_model: "",
        small_model: "",
        needs_key: true,
        is_azure: false,
    },
];

// ─── Entry Point ────────────────────────────────────────────────────

pub async fn run() -> Result<()> {
    print_banner();

    let provider = select_provider()?;
    let base_url = resolve_base_url(&provider)?;
    let azure_api_version = resolve_azure_version(&provider)?;
    let api_key = collect_api_key(&provider)?;
    let (big, middle, small) = collect_models(&provider)?;
    let (port, anthropic_key, reasoning_effort) = collect_optional()?;

    let config = build_config(
        api_key,
        base_url,
        big,
        middle,
        small,
        port,
        anthropic_key,
        azure_api_version,
        reasoning_effort,
    );

    save_config(&config)?;
    print_summary(&config);
    offer_start(&config).await?;

    Ok(())
}

// ─── Banner ─────────────────────────────────────────────────────────

fn print_banner() {
    println!();
    println!(
        "  {}",
        style("╔══════════════════════════════════════╗").cyan()
    );
    println!(
        "  {}",
        style("║       cc-proxy 交互式配置向导       ║").cyan()
    );
    println!(
        "  {}",
        style("╚══════════════════════════════════════╝").cyan()
    );
    println!();
}

// ─── Step 1: Select Provider ────────────────────────────────────────

fn select_provider() -> Result<ProviderPreset> {
    let items: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();

    let idx = Select::new()
        .with_prompt(format!("{}", style("选择 API 提供商").bold()))
        .items(&items)
        .default(0)
        .interact()
        .context("选择提供商失败")?;

    Ok(PROVIDERS[idx].clone())
}

// ─── Resolve Base URL ───────────────────────────────────────────────

fn resolve_base_url(provider: &ProviderPreset) -> Result<String> {
    if provider.is_azure {
        return build_azure_url();
    }

    if provider.base_url.is_empty() {
        // Custom provider
        let url: String = Input::new()
            .with_prompt("输入 API Base URL")
            .interact_text()
            .context("输入 Base URL 失败")?;
        return Ok(url);
    }

    // Preset URL — allow override
    let url: String = Input::new()
        .with_prompt("API Base URL")
        .default(provider.base_url.to_string())
        .interact_text()
        .context("输入 Base URL 失败")?;
    Ok(url)
}

fn build_azure_url() -> Result<String> {
    let resource: String = Input::new()
        .with_prompt("Azure 资源名称 (resource name)")
        .interact_text()
        .context("输入 Azure 资源名称失败")?;

    let deployment: String = Input::new()
        .with_prompt("Azure 部署名称 (deployment name)")
        .interact_text()
        .context("输入 Azure 部署名称失败")?;

    Ok(format!(
        "https://{resource}.openai.azure.com/openai/deployments/{deployment}"
    ))
}

fn resolve_azure_version(provider: &ProviderPreset) -> Result<Option<String>> {
    if !provider.is_azure {
        return Ok(None);
    }

    let version: String = Input::new()
        .with_prompt("Azure API 版本 (api-version)")
        .default("2024-12-01-preview".to_string())
        .interact_text()
        .context("输入 Azure API 版本失败")?;

    Ok(Some(version))
}

// ─── Step 2: API Key ────────────────────────────────────────────────

fn collect_api_key(provider: &ProviderPreset) -> Result<String> {
    if !provider.needs_key {
        println!(
            "  {} Ollama 无需 API Key，已自动填充占位符",
            style("ℹ").blue()
        );
        return Ok("sk-ollama".to_string());
    }

    let key = Password::new()
        .with_prompt("输入 API Key")
        .interact()
        .context("输入 API Key 失败")?;

    if key.trim().is_empty() {
        anyhow::bail!("API Key 不能为空");
    }

    Ok(key)
}

// ─── Step 3: Models ─────────────────────────────────────────────────

fn collect_models(provider: &ProviderPreset) -> Result<(String, String, String)> {
    println!();
    println!("  {}", style("模型配置").bold().underlined());

    let big: String = if provider.big_model.is_empty() {
        Input::new()
            .with_prompt("BIG_MODEL (主力模型)")
            .interact_text()
            .context("输入 BIG_MODEL 失败")?
    } else {
        Input::new()
            .with_prompt("BIG_MODEL (主力模型)")
            .default(provider.big_model.to_string())
            .interact_text()
            .context("输入 BIG_MODEL 失败")?
    };

    let middle_default = if provider.middle_model.is_empty() {
        big.clone()
    } else {
        provider.middle_model.to_string()
    };

    let middle: String = Input::new()
        .with_prompt("MIDDLE_MODEL (中等模型)")
        .default(middle_default)
        .interact_text()
        .context("输入 MIDDLE_MODEL 失败")?;

    let small: String = if provider.small_model.is_empty() {
        Input::new()
            .with_prompt("SMALL_MODEL (轻量模型)")
            .default(big.clone())
            .interact_text()
            .context("输入 SMALL_MODEL 失败")?
    } else {
        Input::new()
            .with_prompt("SMALL_MODEL (轻量模型)")
            .default(provider.small_model.to_string())
            .interact_text()
            .context("输入 SMALL_MODEL 失败")?
    };

    Ok((big, middle, small))
}

// ─── Step 4: Optional Settings ──────────────────────────────────────

fn collect_optional() -> Result<(u16, Option<String>, String)> {
    println!();
    println!("  {}", style("可选配置").bold().underlined());

    let port: u16 = Input::new()
        .with_prompt("代理监听端口")
        .default(8082u16)
        .interact_text()
        .context("输入端口失败")?;

    // Reasoning effort
    let reasoning_items = &["none (关闭)", "low", "medium", "high", "xhigh"];
    let reasoning_idx = Select::new()
        .with_prompt(format!("{}", style("思考模式 (Reasoning Effort)").bold()))
        .items(reasoning_items)
        .default(0)
        .interact()
        .context("选择思考模式失败")?;
    let reasoning_effort = match reasoning_idx {
        0 => "none",
        1 => "low",
        2 => "medium",
        3 => "high",
        4 => "xhigh",
        _ => "none",
    }
    .to_string();

    let want_auth = Confirm::new()
        .with_prompt("是否配置 ANTHROPIC_API_KEY (用于鉴权)")
        .default(false)
        .interact()
        .context("确认鉴权配置失败")?;

    let anthropic_key = if want_auth {
        let key = Password::new()
            .with_prompt("输入 ANTHROPIC_API_KEY")
            .interact()
            .context("输入 ANTHROPIC_API_KEY 失败")?;
        if key.trim().is_empty() {
            None
        } else {
            Some(key)
        }
    } else {
        None
    };

    Ok((port, anthropic_key, reasoning_effort))
}

// ─── Build Config ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_config(
    api_key: String,
    base_url: String,
    big_model: String,
    middle_model: String,
    small_model: String,
    port: u16,
    anthropic_key: Option<String>,
    azure_api_version: Option<String>,
    reasoning_effort: String,
) -> ProxyConfig {
    let middle_opt = if middle_model == big_model {
        None
    } else {
        Some(middle_model)
    };

    ProxyConfig {
        openai_api_key: api_key,
        openai_base_url: base_url,
        big_model,
        middle_model: middle_opt,
        small_model,
        host: "0.0.0.0".to_string(),
        port,
        anthropic_api_key: anthropic_key,
        azure_api_version,
        log_level: "info".to_string(),
        max_tokens_limit: 4096,
        min_tokens_limit: 100,
        request_timeout: 90,
        custom_headers: HashMap::new(),
        reasoning_effort,
        big_reasoning: None,
        middle_reasoning: None,
        small_reasoning: None,
    }
}

// ─── Step 5: Save ───────────────────────────────────────────────────

fn save_config(config: &ProxyConfig) -> Result<()> {
    let path = ProxyConfig::default_config_path();

    config.save_to_file(&path).context("保存配置文件失败")?;

    println!();
    println!(
        "  {} 配置已保存到 {}",
        style("✔").green().bold(),
        style(path.display()).underlined()
    );

    Ok(())
}

// ─── Step 6: Summary ────────────────────────────────────────────────

fn print_summary(config: &ProxyConfig) {
    let masked_key = mask_key(&config.openai_api_key);
    let auth_status = if config.anthropic_api_key.is_some() {
        style("已启用").green().to_string()
    } else {
        style("未启用").yellow().to_string()
    };

    println!();
    println!("  {}", style("═══ 配置摘要 ═══").cyan().bold());
    println!(
        "  Base URL:      {}",
        style(&config.openai_base_url).white()
    );
    println!("  API Key:       {}", style(&masked_key).dim());
    println!("  BIG_MODEL:     {}", style(&config.big_model).green());
    println!(
        "  MIDDLE_MODEL:  {}",
        style(config.effective_middle_model()).green()
    );
    println!("  SMALL_MODEL:   {}", style(&config.small_model).green());
    println!("  监听端口:      {}", style(config.port).white());
    println!("  鉴权:          {}", auth_status);

    if let Some(ref ver) = config.azure_api_version {
        println!("  Azure 版本:    {}", style(ver).white());
    }

    println!();
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "*".repeat(key.len());
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

// ─── Step 7: Offer Start ────────────────────────────────────────────

async fn offer_start(config: &ProxyConfig) -> Result<()> {
    let start_now = Confirm::new()
        .with_prompt("现在启动代理服务？")
        .default(true)
        .interact()
        .context("确认启动失败")?;

    if start_now {
        println!();
        println!(
            "  {} cc-proxy 启动中... 监听 {}:{}",
            style("▶").green().bold(),
            config.host,
            config.port,
        );
        println!();
        cc_proxy_core::server::serve(config.clone()).await?;
    } else {
        println!();
        println!("  可随时运行 {} 启动代理", style("cc-proxy start").bold());
    }

    Ok(())
}
