use clap::{Parser, Subcommand};
use console::style;
use dialoguer::Select;

mod commands;
mod daemon;

#[derive(Parser)]
#[command(name = "cc-proxy")]
#[command(about = "Claude Code Proxy — use any OpenAI-compatible API with Claude Code")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the proxy server (foreground)
    Start {
        /// Run in daemon mode (background)
        #[arg(short, long)]
        daemon: bool,
    },
    /// Stop the background proxy
    Stop,
    /// Show proxy status and configuration
    Status,
    /// Interactive configuration wizard
    Setup,
    /// Test upstream API connection
    Test,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ONLY load .env when running subcommands (not interactive menu)
    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => {
            let _ = dotenvy::dotenv();
            init_tracing();
            match cmd {
                Commands::Start { daemon } => {
                    if daemon {
                        daemon::start_daemon()?;
                    } else {
                        commands::start::run().await?;
                    }
                }
                Commands::Stop => commands::stop::run()?,
                Commands::Status => commands::status::run().await?,
                Commands::Setup => commands::setup::run().await?,
                Commands::Test => commands::test::run().await?,
            }
        }
        None => interactive_menu().await?,
    }

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn,reqwest=warn".into()),
        )
        .init();
}

// ═══════════════════════════════════════════════════════════════════
//  Interactive Menu
// ═══════════════════════════════════════════════════════════════════

async fn interactive_menu() -> anyhow::Result<()> {
    print_banner();

    loop {
        let has_config = config_file_exists();
        let proxy_running = check_proxy_running().await;

        // ── Status bar ──
        if has_config {
            let status = if proxy_running {
                style("● 代理运行中").green().to_string()
            } else {
                style("○ 代理未运行").dim().to_string()
            };
            println!("  {}", status);
        } else {
            println!("  {}", style("⚠ 尚未配置").yellow());
        }
        println!();

        // ── Build menu ──
        let mut items: Vec<String> = Vec::new();
        let mut actions: Vec<&str> = Vec::new();

        if has_config {
            if proxy_running {
                items.push(mi("🔄", "重启代理", "停止后重新启动"));
                actions.push("restart");
            } else {
                items.push(mi("▶", "启动代理", "后台启动"));
                actions.push("start");
            }

            items.push(mi("🔑", "连接信息", "查看地址和密钥"));
            actions.push("info");

            items.push(mi("📊", "查看状态", if proxy_running { "运行中" } else { "已停止" }));
            actions.push("status");

            items.push(mi("🔗", "测试连接", "测试上游 API"));
            actions.push("test");

            if proxy_running {
                items.push(mi("⏹", "停止代理", ""));
                actions.push("stop");
            }
        }

        items.push(mi("⚙", "配置向导", if has_config { "修改配置" } else { "首次使用请先配置" }));
        actions.push("setup");

        items.push(mi("Q", "退出", ""));
        actions.push("quit");

        let default_idx = if !has_config {
            items.len() - 2 // 配置向导
        } else {
            0 // 启动/重启 (always first when configured)
        };

        let selection = Select::new()
            .with_prompt(format!("  {}", style("选择操作").bold()))
            .items(&items)
            .default(default_idx)
            .interact_opt()?;

        let Some(idx) = selection else { break };

        println!();

        match actions[idx] {
            "start" => {
                daemon::start_daemon()?;
                print_connection_info();
            }
            "restart" => {
                println!("  {} 正在重启...", style("🔄").yellow());
                let _ = commands::stop::run();
                std::thread::sleep(std::time::Duration::from_secs(1));
                daemon::start_daemon()?;
                print_connection_info();
            }
            "info" => {
                print_connection_info();
            }
            "status" => {
                commands::status::run().await?;
            }
            "test" => {
                commands::test::run().await?;
            }
            "stop" => {
                commands::stop::run()?;
            }
            "setup" => {
                commands::setup::run().await?;
                continue;
            }
            "quit" => break,
            _ => break,
        }

        println!();
    }

    Ok(())
}

// ── Helpers ──

fn mi(icon: &str, label: &str, desc: &str) -> String {
    if desc.is_empty() {
        format!("  {}  {}", icon, style(label).white())
    } else {
        format!("  {}  {:<12} {}", icon, style(label).white(), style(format!("— {desc}")).dim())
    }
}

fn config_file_exists() -> bool {
    cc_proxy_core::config::ProxyConfig::default_config_path().exists()
}

/// Read port and auth key from config.json without full ProxyConfig::load()
fn read_config_json() -> Option<serde_json::Value> {
    let path = cc_proxy_core::config::ProxyConfig::default_config_path();
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn get_config_port() -> u16 {
    read_config_json()
        .and_then(|v| v.get("port").and_then(|p| p.as_u64()))
        .map(|p| p as u16)
        .unwrap_or(8082)
}

fn get_config_auth_key() -> Option<String> {
    read_config_json()
        .and_then(|v| v.get("anthropic_api_key").and_then(|k| k.as_str()).map(String::from))
}

async fn check_proxy_running() -> bool {
    let port = get_config_port();
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

fn print_connection_info() {
    let port = get_config_port();
    let auth_key = get_config_auth_key().unwrap_or_else(|| "any-value".into());

    println!("  {}── {} ──{}", style("──────").dim(), style("连接信息").bold().cyan(), style("──────").dim());
    println!();
    println!("  {}  http://localhost:{}", style("代理地址:").dim(), style(port).green());
    println!("  {}  {}", style("鉴权密钥:").dim(), style(&auth_key).green());
    println!();
    println!("  {} 在终端中运行:", style("使用方法:").dim());
    println!();
    println!(
        "  {}",
        style(format!(
            "ANTHROPIC_BASE_URL=http://localhost:{port} ANTHROPIC_API_KEY=\"{auth_key}\" claude"
        )).green()
    );
    println!();
    println!("  {} 写入 shell 配置 (永久生效):", style("或者:").dim());
    println!();
    println!(
        "  {}",
        style(format!(
            "echo 'export ANTHROPIC_BASE_URL=http://localhost:{port}' >> ~/.zshrc"
        )).dim()
    );
    println!(
        "  {}",
        style(format!(
            "echo 'export ANTHROPIC_API_KEY={auth_key}' >> ~/.zshrc"
        )).dim()
    );
    println!("  {}", style("source ~/.zshrc && claude").dim());
    println!();
}

fn print_banner() {
    let c = |s: &str| style(s).cyan().to_string();
    let d = |s: &str| style(s).dim().to_string();

    println!();
    println!("  {}", c("┌─────────────────────────────────────────────────────┐"));
    println!("  {}                                                     {}", c("│"), c("│"));
    println!("  {}              {}              {}", c("│"), style("cc-proxy").bold().white(), c("│"));
    println!("  {}        {}        {}", c("│"), d("Claude Code ↔ Any LLM Provider"), c("│"));
    println!("  {}                                                     {}", c("│"), c("│"));
    println!("  {}        {}   |   {}   |   {}            {}", c("│"), style("v0.1.5").green(), style("Rust").yellow(), d("6.4MB"), c("│"));
    println!("  {}                                                     {}", c("│"), c("│"));
    println!("  {}", c("└─────────────────────────────────────────────────────┘"));
    println!();
}
