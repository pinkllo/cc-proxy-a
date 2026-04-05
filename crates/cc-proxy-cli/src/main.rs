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
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => {
            // Subcommand mode: init tracing for logs
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
        None => {
            // No subcommand → interactive TUI
            interactive_menu().await?;
        }
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
        let has_config = cc_proxy_core::config::ProxyConfig::default_config_path().exists();
        let proxy_running = check_proxy_running().await;

        // ── Build menu ──
        let mut items: Vec<String> = Vec::new();
        let mut actions: Vec<&str> = Vec::new();

        // 配置
        items.push(menu_item(
            "⚙",
            "yellow",
            "配置向导",
            if has_config { "修改配置" } else { "首次使用请先配置" },
        ));
        actions.push("setup");

        if has_config {
            // 启动/重启
            if proxy_running {
                items.push(menu_item("🔄", "green", "重启代理", "停止后重新启动"));
                actions.push("restart");
            } else {
                items.push(menu_item("▶", "green", "启动代理", "后台启动代理服务"));
                actions.push("start");
            }

            // 状态
            items.push(menu_item(
                "📊",
                "cyan",
                "查看状态",
                if proxy_running { "运行中" } else { "已停止" },
            ));
            actions.push("status");

            // 测试
            items.push(menu_item("🔗", "cyan", "测试连接", "测试上游 API"));
            actions.push("test");

            // 停止
            if proxy_running {
                items.push(menu_item("⏹", "red", "停止代理", "停止后台进程"));
                actions.push("stop");
            }
        }

        // 退出
        items.push(menu_item("Q", "red", "退出", ""));
        actions.push("quit");

        // ── 状态栏 ──
        if has_config {
            let status_text = if proxy_running {
                style("● 代理运行中").green().to_string()
            } else {
                style("○ 代理未运行").dim().to_string()
            };
            println!("  {}", status_text);
            println!();
        }

        let selection = Select::new()
            .with_prompt(format!("  {}", style("选择操作").bold()))
            .items(&items)
            .default(if has_config { 1 } else { 0 }) // 有配置默认选启动，没有默认选配置
            .interact_opt()?;

        let Some(idx) = selection else {
            break;
        };

        println!();

        match actions[idx] {
            "setup" => {
                commands::setup::run().await?;
                // setup 内部可能已启动代理，如果没有，回到菜单
                continue;
            }
            "start" => {
                daemon::start_daemon()?;
                print_claude_hint();
            }
            "restart" => {
                println!("  {} 正在重启...", style("🔄").yellow());
                let _ = commands::stop::run();
                std::thread::sleep(std::time::Duration::from_secs(1));
                daemon::start_daemon()?;
                print_claude_hint();
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
            "quit" => break,
            _ => break,
        }

        println!();
    }

    Ok(())
}

fn menu_item(icon: &str, _color: &str, label: &str, desc: &str) -> String {
    if desc.is_empty() {
        format!("  {}  {}", icon, style(label).white())
    } else {
        format!(
            "  {}  {:<12} {}",
            icon,
            style(label).white(),
            style(format!("— {desc}")).dim()
        )
    }
}

async fn check_proxy_running() -> bool {
    let port = get_config_port();
    let url = format!("http://127.0.0.1:{port}/health");
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

fn get_config_port() -> u16 {
    let path = cc_proxy_core::config::ProxyConfig::default_config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(port) = val.get("port").and_then(|v| v.as_u64()) {
                return port as u16;
            }
        }
    }
    8082
}

fn print_claude_hint() {
    let port = get_config_port();
    println!();
    println!(
        "  {} 连接 Claude Code:",
        style("💡").cyan()
    );
    println!(
        "  {}",
        style(format!(
            "ANTHROPIC_BASE_URL=http://localhost:{port} ANTHROPIC_API_KEY=any claude"
        ))
        .green()
    );
    println!();
}

fn print_banner() {
    let c = |s: &str| style(s).cyan().to_string();
    let d = |s: &str| style(s).dim().to_string();

    println!();
    println!(
        "  {}",
        c("┌─────────────────────────────────────────────────────┐")
    );
    println!(
        "  {}                                                     {}",
        c("│"),
        c("│")
    );
    println!(
        "  {}              {}              {}",
        c("│"),
        style("cc-proxy").bold().white(),
        c("│")
    );
    println!(
        "  {}        {}        {}",
        c("│"),
        d("Claude Code ↔ Any LLM Provider"),
        c("│")
    );
    println!(
        "  {}                                                     {}",
        c("│"),
        c("│")
    );
    println!(
        "  {}        {}   |   {}   |   {}            {}",
        c("│"),
        style("v0.1.2").green(),
        style("Rust").yellow(),
        d("6.4MB"),
        c("│")
    );
    println!(
        "  {}                                                     {}",
        c("│"),
        c("│")
    );
    println!(
        "  {}",
        c("└─────────────────────────────────────────────────────┘")
    );
    println!();
}
