use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;

#[derive(Args)]
pub struct SnapshotArgs {
    /// Path to the source database
    pub path: PathBuf,
    /// Destination path for the snapshot
    pub dest: PathBuf,
}

pub async fn run(args: SnapshotArgs) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;
    afs.snapshot(&args.dest).await?;
    println!(
        "Snapshot: {} -> {}",
        args.path.display(),
        args.dest.display()
    );
    afs.close().await?;
    Ok(())
}
