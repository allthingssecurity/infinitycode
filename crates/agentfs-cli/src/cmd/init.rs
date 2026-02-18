use std::path::PathBuf;

use agentfs_core::config::{AgentFSConfig, DurabilityLevel};
use clap::Args;

#[derive(Args)]
pub struct InitArgs {
    /// Path to the new database file
    pub path: PathBuf,

    /// Durability level: off, normal, full
    #[arg(long, default_value = "normal")]
    pub durability: String,

    /// Chunk size in bytes
    #[arg(long, default_value = "65536")]
    pub chunk_size: usize,
}

pub async fn run(args: InitArgs) -> anyhow::Result<()> {
    let durability: DurabilityLevel = args
        .durability
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    let config = AgentFSConfig::builder(&args.path)
        .durability(durability)
        .chunk_size(args.chunk_size)
        .checkpoint_interval_secs(0)
        .build();

    let afs = agentfs_core::AgentFS::create(config).await?;
    afs.close().await?;

    println!("Created AgentFS database at {}", args.path.display());
    Ok(())
}
