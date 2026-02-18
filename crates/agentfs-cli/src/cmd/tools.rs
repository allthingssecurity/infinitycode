use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Subcommand)]
pub enum ToolsCommands {
    /// List recent tool calls
    List {
        db: PathBuf,
        /// Number of recent calls to show
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Show tool call statistics
    Stats {
        db: PathBuf,
    },
}

pub async fn run(cmd: ToolsCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        ToolsCommands::List { db, limit } => {
            let afs = open_db(&db).await?;
            let calls = afs.tools.recent(limit).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&calls)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["ID", "Tool", "Status", "Started", "Ended"]);

                for call in &calls {
                    table.add_row(vec![
                        &call.id.to_string(),
                        &call.tool_name,
                        &call.status,
                        &call.started_at,
                        call.ended_at.as_deref().unwrap_or("-"),
                    ]);
                }

                println!("{table}");
            }
            afs.close().await?;
        }
        ToolsCommands::Stats { db } => {
            let afs = open_db(&db).await?;
            let stats = afs.tools.stats().await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Tool", "Total", "Success", "Error", "In Progress"]);

                for s in &stats {
                    table.add_row(vec![
                        &s.tool_name,
                        &s.total.to_string(),
                        &s.successes.to_string(),
                        &s.errors.to_string(),
                        &s.in_progress.to_string(),
                    ]);
                }

                println!("{table}");
            }
            afs.close().await?;
        }
    }
    Ok(())
}

async fn open_db(path: &PathBuf) -> anyhow::Result<agentfs_core::AgentFS> {
    let config = AgentFSConfig::builder(path)
        .checkpoint_interval_secs(0)
        .build();
    Ok(agentfs_core::AgentFS::open(config).await?)
}
