use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;

#[derive(Args)]
pub struct GcArgs {
    /// Path to the database
    pub path: PathBuf,
}

pub async fn run(args: GcArgs, json: bool) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;
    let report = afs.gc().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Garbage Collection Report:");
        println!("  Orphan inodes:   {}", report.orphan_inodes);
        println!("  Stale chunks:    {}", report.stale_chunks);
        println!("  Stale symlinks:  {}", report.stale_symlinks);

        let total = report.orphan_inodes + report.stale_chunks + report.stale_symlinks;
        if total == 0 {
            println!("\nNo garbage found.");
        } else {
            println!("\nCleaned up {total} items.");
        }
    }

    afs.close().await?;
    Ok(())
}
