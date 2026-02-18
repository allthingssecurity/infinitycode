use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;

#[derive(Args)]
pub struct InfoArgs {
    /// Path to the database file
    pub path: PathBuf,
}

pub async fn run(args: InfoArgs, json: bool) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;
    let info = afs.info().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("AgentFS Database: {}", args.path.display());
        println!("  Schema version:  {}", info.schema_version);
        println!("  Created at:      {}", info.created_at);
        println!("  Durability:      {}", info.durability);
        println!("  Chunk size:      {} bytes", info.chunk_size);
        println!("  DB size:         {} bytes", info.db_size_bytes);
        println!("  WAL pages:       {}", info.wal_pages);
        println!();
        println!("  Inodes:          {}", info.inode_count);
        println!("  Files:           {}", info.file_count);
        println!("  Directories:     {}", info.dir_count);
        println!("  Data bytes:      {}", info.total_data_bytes);
        println!("  KV entries:      {}", info.kv_count);
        println!("  Tool calls:      {}", info.tool_call_count);
        println!();
        println!("  Sessions:        {} ({} active)", info.session_count, info.active_sessions);
        println!("  Total tokens:    {}", info.total_tokens);
        println!("  Total cost:      {} microcents", info.total_cost_microcents);
        println!("  Events:          {}", info.event_count);
    }

    afs.close().await?;
    Ok(())
}
