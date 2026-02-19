mod cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "infinity", about = "SQLite-backed agent filesystem with proper durability")]
struct Cli {
    /// Output as JSON instead of human-readable tables
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new agent database
    Init(cmd::init::InitArgs),
    /// Show database info and stats
    Info(cmd::info::InfoArgs),
    /// Filesystem operations
    #[command(subcommand)]
    Fs(cmd::fs::FsCommands),
    /// Key-value store operations
    #[command(subcommand)]
    Kv(cmd::kv::KvCommands),
    /// Tool call audit trail
    #[command(subcommand)]
    Tools(cmd::tools::ToolsCommands),
    /// Unified audit timeline
    Timeline(cmd::timeline::TimelineArgs),
    /// Integrity checking
    #[command(subcommand)]
    Integrity(cmd::integrity::IntegrityCommands),
    /// Garbage collection
    Gc(cmd::gc::GcArgs),
    /// Create a snapshot using SQLite backup API
    Snapshot(cmd::snapshot::SnapshotArgs),
    /// Force a WAL checkpoint
    Checkpoint(cmd::checkpoint::CheckpointArgs),
    /// Run schema migration
    Migrate(cmd::migrate::MigrateArgs),
    /// Session management
    #[command(subcommand)]
    Sessions(cmd::sessions::SessionsCommands),
    /// Token usage analytics
    #[command(subcommand)]
    Analytics(cmd::analytics::AnalyticsCommands),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let json = cli.json;

    match cli.command {
        Commands::Init(args) => cmd::init::run(args).await,
        Commands::Info(args) => cmd::info::run(args, json).await,
        Commands::Fs(sub) => cmd::fs::run(sub, json).await,
        Commands::Kv(sub) => cmd::kv::run(sub, json).await,
        Commands::Tools(sub) => cmd::tools::run(sub, json).await,
        Commands::Timeline(args) => cmd::timeline::run(args, json).await,
        Commands::Integrity(sub) => cmd::integrity::run(sub, json).await,
        Commands::Gc(args) => cmd::gc::run(args, json).await,
        Commands::Snapshot(args) => cmd::snapshot::run(args).await,
        Commands::Checkpoint(args) => cmd::checkpoint::run(args).await,
        Commands::Migrate(args) => cmd::migrate::run(args).await,
        Commands::Sessions(sub) => cmd::sessions::run(sub, json).await,
        Commands::Analytics(sub) => cmd::analytics::run(sub, json).await,
    }
}
