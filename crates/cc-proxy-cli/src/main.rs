use clap::{Parser, Subcommand};

mod commands;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env
    let _ = dotenvy::dotenv();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn,reqwest=warn".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Start { daemon: _ }) | None => {
            // Default: start server in foreground
            commands::start::run().await?;
        }
    }

    Ok(())
}
