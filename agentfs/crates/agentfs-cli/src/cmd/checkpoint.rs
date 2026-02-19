use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;

#[derive(Args)]
pub struct CheckpointArgs {
    /// Path to the database
    pub path: PathBuf,
}

pub async fn run(args: CheckpointArgs) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;
    afs.checkpoint().await?;
    println!("Checkpoint complete.");
    afs.close().await?;
    Ok(())
}
