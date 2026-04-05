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
    /// Start the proxy server
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
    // Load .env
    let _ = dotenvy::dotenv();

    // Init tracing (suppress unless explicit)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Start { daemon }) => {
            if daemon {
                daemon::start_daemon()?;
            } else {
                commands::start::run().await?;
            }
        }
        Some(Commands::Stop) => commands::stop::run()?,
        Some(Commands::Status) => commands::status::run().await?,
        Some(Commands::Setup) => commands::setup::run().await?,
        Some(Commands::Test) => commands::test::run().await?,
        None => {
            // No subcommand → interactive menu
            interactive_menu().await?;
        }
    }

    Ok(())
}

/// CCG-style interactive main menu
async fn interactive_menu() -> anyhow::Result<()> {
    print_banner();

    loop {
        let has_config = cc_proxy_core::config::ProxyConfig::default_config_path().exists();

        println!(
            "  {}── {} ──{}",
            style("──────").dim(),
            style("主菜单").bold().cyan(),
            style("──────").dim()
        );
        println!();

        let mut items = Vec::new();
        let mut actions: Vec<&str> = Vec::new();

        if has_config {
            items.push(format!(
                "  {}  {}         — {}",
                style("▶").green(),
                style("启动代理").bold().white(),
                style("前台启动代理服务").dim()
            ));
            actions.push("start");

            items.push(format!(
                "  {}  {}     — {}",
                style("▶").green(),
                style("后台启动代理").bold().white(),
                style("守护进程模式").dim()
            ));
            actions.push("start-d");
        }

        items.push(format!(
            "  {}  {}         — {}",
            style("⚙").yellow(),
            style("配置向导").bold().white(),
            style(if has_config {
                "修改现有配置"
            } else {
                "首次配置 (必须先运行)"
            })
            .dim()
        ));
        actions.push("setup");

        if has_config {
            items.push(format!(
                "  {}  {}         — {}",
                style("📊").cyan(),
                style("查看状态").white(),
                style("显示配置和运行状态").dim()
            ));
            actions.push("status");

            items.push(format!(
                "  {}  {}         — {}",
                style("🔗").cyan(),
                style("测试连接").white(),
                style("测试上游 API 连通性").dim()
            ));
            actions.push("test");

            items.push(format!(
                "  {}  {}         — {}",
                style("⏹").red(),
                style("停止代理").white(),
                style("停止后台代理进程").dim()
            ));
            actions.push("stop");
        }

        items.push(format!(
            "  {}  {}",
            style("Q").red(),
            style("退出").dim()
        ));
        actions.push("quit");

        let selection = Select::new()
            .items(&items)
            .default(0)
            .interact_opt()?;

        let Some(idx) = selection else {
            break;
        };

        let action = actions[idx];
        println!();

        match action {
            "start" => {
                commands::start::run().await?;
                break;
            }
            "start-d" => {
                daemon::start_daemon()?;
                // After daemon start, show hint and loop back
                println!();
                println!(
                    "  {} 使用 Claude Code:",
                    style("提示").cyan().bold()
                );
                println!(
                    "  {}",
                    style("ANTHROPIC_BASE_URL=http://localhost:8082 ANTHROPIC_API_KEY=any claude")
                        .green()
                );
                println!();
            }
            "setup" => {
                commands::setup::run().await?;
                break;
            }
            "status" => {
                commands::status::run().await?;
                println!();
            }
            "test" => {
                commands::test::run().await?;
                println!();
            }
            "stop" => {
                commands::stop::run()?;
                println!();
            }
            "quit" => break,
            _ => break,
        }
    }

    Ok(())
}

fn print_banner() {
    let c = |s: &str| style(s).cyan().to_string();
    let d = |s: &str| style(s).dim().to_string();

    println!();
    println!("  {}", c("┌─────────────────────────────────────────────────────┐"));
    println!("  {}                                                     {}", c("│"), c("│"));
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
    println!("  {}                                                     {}", c("│"), c("│"));
    println!(
        "  {}        {}   |   {}   |   {}            {}",
        c("│"),
        style("v0.1.1").green(),
        style("Rust").yellow(),
        d("6.4MB"),
        c("│")
    );
    println!("  {}                                                     {}", c("│"), c("│"));
    println!("  {}", c("└─────────────────────────────────────────────────────┘"));
    println!();
}
