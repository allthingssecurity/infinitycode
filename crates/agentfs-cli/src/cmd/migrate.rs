use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;

#[derive(Args)]
pub struct MigrateArgs {
    /// Path to the database
    pub path: PathBuf,
}

pub async fn run(args: MigrateArgs) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;
    afs.migrate().await?;
    println!("Migration complete.");
    afs.close().await?;
    Ok(())
}
