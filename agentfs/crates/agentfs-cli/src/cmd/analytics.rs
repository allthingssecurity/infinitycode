use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Subcommand)]
pub enum AnalyticsCommands {
    /// Show total token usage and cost summary
    Summary {
        db: PathBuf,
    },
    /// Show cost breakdown by session
    Sessions {
        db: PathBuf,
    },
    /// Show recent token usage records
    Recent {
        db: PathBuf,
        /// Number of records to show
        #[arg(long, default_value = "20")]
        limit: i64,
    },
}

pub async fn run(cmd: AnalyticsCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        AnalyticsCommands::Summary { db } => {
            let afs = open_db(&db).await?;
            let summary = afs.analytics.summary().await?;
            let models = afs.analytics.by_model().await?;

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "summary": summary,
                        "by_model": models,
                    }))?
                );
            } else {
                println!("Token Usage Summary:");
                println!("  Input tokens:    {}", summary.total_input_tokens);
                println!("  Output tokens:   {}", summary.total_output_tokens);
                println!("  Cache read:      {}", summary.total_cache_read);
                println!("  Cache write:     {}", summary.total_cache_write);
                println!("  Total cost:      {} microcents", summary.total_cost_microcents);
                println!("  Records:         {}", summary.record_count);

                if !models.is_empty() {
                    println!();
                    let mut table = Table::new();
                    table.load_preset(UTF8_FULL_CONDENSED);
                    table.set_header(vec!["Model", "Input Tokens", "Output Tokens", "Cost (microcents)"]);
                    for m in &models {
                        table.add_row(vec![
                            &m.model,
                            &m.input_tokens.to_string(),
                            &m.output_tokens.to_string(),
                            &m.cost_microcents.to_string(),
                        ]);
                    }
                    println!("{table}");
                }
            }
            afs.close().await?;
        }
        AnalyticsCommands::Sessions { db } => {
            let afs = open_db(&db).await?;
            let sessions = afs.analytics.by_session().await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Session", "Agent", "Total Tokens", "Cost (microcents)"]);
                for s in &sessions {
                    table.add_row(vec![
                        &s.session_id,
                        s.agent_name.as_deref().unwrap_or("-"),
                        &s.total_tokens.to_string(),
                        &s.cost_microcents.to_string(),
                    ]);
                }
                println!("{table}");
            }
            afs.close().await?;
        }
        AnalyticsCommands::Recent { db, limit } => {
            let afs = open_db(&db).await?;
            let records = afs.analytics.recent_usage(limit).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&records)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Model", "Input", "Output", "Cost", "Session", "Time"]);
                for r in &records {
                    table.add_row(vec![
                        &r.model,
                        &r.input_tokens.to_string(),
                        &r.output_tokens.to_string(),
                        &r.cost_microcents.to_string(),
                        r.session_id.as_deref().unwrap_or("-"),
                        r.recorded_at.as_deref().unwrap_or("-"),
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
