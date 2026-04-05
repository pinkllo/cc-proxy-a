use std::collections::HashMap;

use anyhow::{Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Password, Select};

use cc_proxy_core::config::ProxyConfig;

// ─── 内置模型列表 ───────────────────────────────────────────────────

const BUILTIN_MODELS: &[(&str, &str)] = &[
    ("gpt-5.4", "GPT-5.4 (最新旗舰)"),
    ("gpt-5.4-mini", "GPT-5.4 Mini (轻量快速)"),
    ("gpt-5.1", "GPT-5.1"),
    ("gpt-5.2", "GPT-5.2"),
    ("gpt-5", "GPT-5"),
    ("gpt-4o", "GPT-4o"),
    ("gpt-4o-mini", "GPT-4o Mini"),
    ("deepseek-chat", "DeepSeek Chat"),
    ("deepseek-reasoner", "DeepSeek Reasoner"),
];

const REASONING_LEVELS: &[(&str, &str)] = &[
    ("none", "关闭"),
    ("low", "低 — 简单任务"),
    ("medium", "中 — 日常编码"),
    ("high", "高 — 复杂调试"),
    ("xhigh", "极高 — 深度推理"),
];

// ─── Provider Presets ───────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProviderPreset {
    name: &'static str,
    base_url: &'static str,
    default_big: &'static str,
    default_middle: &'static str,
    default_small: &'static str,
    needs_key: bool,
    is_azure: bool,
}

const PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "OpenAI / 第三方中转",
        base_url: "https://api.openai.com/v1",
        default_big: "gpt-5.4",
        default_middle: "gpt-5.4",
        default_small: "gpt-5.4-mini",
        needs_key: true,
        is_azure: false,
    },
    ProviderPreset {
        name: "DeepSeek (国内推荐)",
        base_url: "https://api.deepseek.com",
        default_big: "deepseek-chat",
        default_middle: "deepseek-chat",
        default_small: "deepseek-chat",
        needs_key: true,
        is_azure: false,
    },
    ProviderPreset {
        name: "Ollama (本地部署)",
        base_url: "http://localhost:11434/v1",
        default_big: "qwen2.5:14b",
        default_middle: "qwen2.5:14b",
        default_small: "qwen2.5:7b",
        needs_key: false,
        is_azure: false,
    },
    ProviderPreset {
        name: "Azure OpenAI",
        base_url: "",
        default_big: "gpt-4o",
        default_middle: "gpt-4o",
        default_small: "gpt-4o-mini",
        needs_key: true,
        is_azure: true,
    },
    ProviderPreset {
        name: "自定义服务商",
        base_url: "",
        default_big: "",
        default_middle: "",
        default_small: "",
        needs_key: true,
        is_azure: false,
    },
];

// ═══════════════════════════════════════════════════════════════════
//  Entry Point
// ═══════════════════════════════════════════════════════════════════

pub async fn run() -> Result<()> {
    print_banner();

    // Step 1: Provider
    let provider = select_provider()?;
    let base_url = resolve_base_url(&provider)?;
    let azure_api_version = resolve_azure_version(&provider)?;
    let api_key = collect_api_key(&provider)?;

    // Step 2: Models (per-tier with presets)
    print_section("模型配置");
    println!(
        "  {}",
        style("Claude Code 使用三个模型级别，你可以分别指定映射:").dim()
    );
    println!(
        "    {} → opus (大模型)    {} → sonnet (中模型)    {} → haiku (小模型)",
        style("BIG").green(),
        style("MIDDLE").yellow(),
        style("SMALL").cyan()
    );
    println!();

    let big_model = select_model("BIG (opus 映射)", provider.default_big)?;
    let big_reasoning = select_reasoning(&format!("BIG [{}]", big_model))?;

    let middle_model = select_model("MIDDLE (sonnet 映射)", provider.default_middle)?;
    let middle_reasoning = select_reasoning(&format!("MIDDLE [{}]", middle_model))?;

    let small_model = select_model("SMALL (haiku 映射)", provider.default_small)?;
    let small_reasoning = select_reasoning(&format!("SMALL [{}]", small_model))?;

    // Step 3: Server settings
    print_section("服务设置");
    let port: u16 = Input::new()
        .with_prompt("  代理端口")
        .default(8082u16)
        .interact_text()
        .context("输入端口失败")?;

    let want_auth = Confirm::new()
        .with_prompt("  是否启用 API Key 鉴权 (防止他人使用你的代理)")
        .default(false)
        .interact()
        .context("确认鉴权失败")?;

    let anthropic_key = if want_auth {
        let key = Password::new()
            .with_prompt("  设置鉴权密钥")
            .interact()
            .context("输入鉴权密钥失败")?;
        if key.trim().is_empty() {
            None
        } else {
            Some(key)
        }
    } else {
        None
    };

    // Build config
    let middle_opt = if middle_model == big_model {
        None
    } else {
        Some(middle_model.clone())
    };

    let config = ProxyConfig {
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
        reasoning_effort: "none".to_string(),
        big_reasoning: if big_reasoning == "none" {
            None
        } else {
            Some(big_reasoning)
        },
        middle_reasoning: if middle_reasoning == "none" {
            None
        } else {
            Some(middle_reasoning)
        },
        small_reasoning: if small_reasoning == "none" {
            None
        } else {
            Some(small_reasoning)
        },
    };

    // Save
    let path = ProxyConfig::default_config_path();
    config.save_to_file(&path).context("保存配置失败")?;

    // Summary
    print_summary(&config, &path);

    // Offer start
    let start_now = Confirm::new()
        .with_prompt(format!("  {}", style("现在启动代理？").bold()))
        .default(true)
        .interact()
        .context("确认启动失败")?;

    if start_now {
        println!();
        println!(
            "  {} 正在启动 cc-proxy on {}:{} ...",
            style("▶").green().bold(),
            config.host,
            config.port
        );
        println!();
        cc_proxy_core::server::serve(config).await?;
    } else {
        println!();
        println!(
            "  配置已保存。运行 {} 启动代理",
            style("cc-proxy start").bold().green()
        );
        println!();
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
//  UI Components
// ═══════════════════════════════════════════════════════════════════

fn print_banner() {
    let cyan = |s: &str| style(s).cyan().to_string();
    let dim = |s: &str| style(s).dim().to_string();
    let green = |s: &str| style(s).green().to_string();
    let yellow = |s: &str| style(s).yellow().to_string();

    println!();
    println!("  {}", cyan("┌─────────────────────────────────────────────────────┐"));
    println!("  {}                                                     {}", cyan("│"), cyan("│"));
    println!(
        "  {}      {}     {}",
        cyan("│"),
        style("cc-proxy").bold().white(),
        cyan("│")
    );
    println!(
        "  {}      {}      {}",
        cyan("│"),
        dim("Claude Code ↔ Any LLM Provider"),
        cyan("│")
    );
    println!("  {}                                                     {}", cyan("│"), cyan("│"));
    println!(
        "  {}      {}  |  {}  |  {}       {}",
        cyan("│"),
        green("v0.1.0"),
        yellow("Rust"),
        dim("6.4MB"),
        cyan("│")
    );
    println!("  {}                                                     {}", cyan("│"), cyan("│"));
    println!("  {}", cyan("└─────────────────────────────────────────────────────┘"));
    println!();
}

fn print_section(title: &str) {
    println!();
    println!(
        "  {}── {} ──{}",
        style("─────").dim(),
        style(title).bold().cyan(),
        style("─────").dim()
    );
    println!();
}

fn select_provider() -> Result<ProviderPreset> {
    print_section("选择服务商");

    let _items: Vec<String> = PROVIDERS
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "  {}. {}",
                style(i + 1).green(),
                style(p.name).white()
            )
        })
        .collect();

    let display_items: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();

    let idx = Select::new()
        .with_prompt(format!("  {}", style("选择 API 提供商").bold()))
        .items(&display_items)
        .default(0)
        .interact()
        .context("选择提供商失败")?;

    Ok(PROVIDERS[idx].clone())
}

fn resolve_base_url(provider: &ProviderPreset) -> Result<String> {
    if provider.is_azure {
        let resource: String = Input::new()
            .with_prompt("  Azure 资源名称")
            .interact_text()
            .context("输入失败")?;
        let deployment: String = Input::new()
            .with_prompt("  Azure 部署名称")
            .interact_text()
            .context("输入失败")?;
        return Ok(format!(
            "https://{resource}.openai.azure.com/openai/deployments/{deployment}"
        ));
    }

    if provider.base_url.is_empty() {
        let url: String = Input::new()
            .with_prompt("  API Base URL")
            .interact_text()
            .context("输入失败")?;
        return Ok(url);
    }

    let url: String = Input::new()
        .with_prompt("  API Base URL")
        .default(provider.base_url.to_string())
        .interact_text()
        .context("输入失败")?;
    Ok(url)
}

fn resolve_azure_version(provider: &ProviderPreset) -> Result<Option<String>> {
    if !provider.is_azure {
        return Ok(None);
    }
    let version: String = Input::new()
        .with_prompt("  Azure API 版本")
        .default("2024-12-01-preview".to_string())
        .interact_text()
        .context("输入失败")?;
    Ok(Some(version))
}

fn collect_api_key(provider: &ProviderPreset) -> Result<String> {
    if !provider.needs_key {
        println!(
            "  {} Ollama 无需 API Key",
            style("ℹ").blue()
        );
        return Ok("sk-ollama".to_string());
    }

    let key = Password::new()
        .with_prompt("  输入 API Key")
        .interact()
        .context("输入失败")?;

    if key.trim().is_empty() {
        anyhow::bail!("API Key 不能为空");
    }
    Ok(key)
}

/// Select a model from presets or custom input
fn select_model(label: &str, default: &str) -> Result<String> {
    // Build menu: presets + custom option
    let mut items: Vec<String> = BUILTIN_MODELS
        .iter()
        .map(|(id, desc)| format!("{} — {}", style(id).green(), style(desc).dim()))
        .collect();
    items.push(format!("{}", style("自定义模型 (手动输入)").yellow()));

    // Find default index
    let default_idx = BUILTIN_MODELS
        .iter()
        .position(|(id, _)| {
            if default.is_empty() {
                false
            } else {
                *id == default
            }
        })
        .unwrap_or(0);

    let idx = Select::new()
        .with_prompt(format!("  {}", style(label).bold()))
        .items(&items)
        .default(default_idx)
        .interact()
        .context("选择模型失败")?;

    if idx < BUILTIN_MODELS.len() {
        Ok(BUILTIN_MODELS[idx].0.to_string())
    } else {
        // Custom input
        let model: String = Input::new()
            .with_prompt("  输入模型名称")
            .interact_text()
            .context("输入失败")?;
        Ok(model)
    }
}

/// Select reasoning effort for a specific model tier
fn select_reasoning(label: &str) -> Result<String> {
    let items: Vec<String> = REASONING_LEVELS
        .iter()
        .map(|(id, desc)| format!("{} — {}", style(id).cyan(), desc))
        .collect();

    let idx = Select::new()
        .with_prompt(format!("  {} 思考强度", style(label).bold()))
        .items(&items)
        .default(0) // none
        .interact()
        .context("选择思考强度失败")?;

    Ok(REASONING_LEVELS[idx].0.to_string())
}

fn mask_key(key: &str) -> String {
    if !key.is_ascii() || key.len() <= 8 {
        return "*".repeat(key.len().min(8));
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

fn print_summary(config: &ProxyConfig, path: &std::path::Path) {
    print_section("配置摘要");

    let r = |s: &str| {
        if s == "none" {
            style("关闭").dim().to_string()
        } else {
            style(s).cyan().to_string()
        }
    };

    println!(
        "  {} {}",
        style("Base URL:").dim(),
        style(&config.openai_base_url).white()
    );
    println!(
        "  {} {}",
        style("API Key: ").dim(),
        style(mask_key(&config.openai_api_key)).dim()
    );
    println!();
    println!(
        "  {} {:<16} reasoning: {}",
        style("BIG   (opus):  ").dim(),
        style(&config.big_model).green(),
        r(config.big_reasoning.as_deref().unwrap_or("none"))
    );
    println!(
        "  {} {:<16} reasoning: {}",
        style("MIDDLE(sonnet):").dim(),
        style(config.effective_middle_model()).yellow(),
        r(config.middle_reasoning.as_deref().unwrap_or("none"))
    );
    println!(
        "  {} {:<16} reasoning: {}",
        style("SMALL (haiku): ").dim(),
        style(&config.small_model).cyan(),
        r(config.small_reasoning.as_deref().unwrap_or("none"))
    );
    println!();
    println!(
        "  {} {}:{}",
        style("Server:").dim(),
        config.host,
        style(config.port).white()
    );
    println!(
        "  {} {}",
        style("Auth:  ").dim(),
        if config.anthropic_api_key.is_some() {
            style("已启用").green().to_string()
        } else {
            style("未启用").dim().to_string()
        }
    );
    println!();
    println!(
        "  {} {}",
        style("✔").green().bold(),
        style(format!("配置已保存: {}", path.display())).dim()
    );
}
