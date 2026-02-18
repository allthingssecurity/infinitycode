use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Args;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Args)]
pub struct TimelineArgs {
    /// Path to the database
    pub path: PathBuf,

    /// Number of recent events to show
    #[arg(long, default_value = "50")]
    pub limit: i64,

    /// Filter by event type
    #[arg(long, name = "type")]
    pub event_type: Option<String>,

    /// Filter by session ID
    #[arg(long)]
    pub session: Option<String>,
}

pub async fn run(args: TimelineArgs, json: bool) -> anyhow::Result<()> {
    let config = AgentFSConfig::builder(&args.path)
        .checkpoint_interval_secs(0)
        .build();
    let afs = agentfs_core::AgentFS::open(config).await?;

    let events = if let Some(ref event_type) = args.event_type {
        afs.events.by_type(event_type, args.limit).await?
    } else if let Some(ref session_id) = args.session {
        afs.events.by_session(session_id, args.limit).await?
    } else {
        afs.events.recent(args.limit).await?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&events)?);
    } else {
        let mut table = Table::new();
        table.load_preset(UTF8_FULL_CONDENSED);
        table.set_header(vec!["Time", "Type", "Path", "Session", "Detail"]);

        for event in &events {
            let detail = event
                .detail
                .as_deref()
                .map(|d| {
                    if d.len() > 40 {
                        format!("{}...", &d[..37])
                    } else {
                        d.to_string()
                    }
                })
                .unwrap_or_default();

            table.add_row(vec![
                &event.recorded_at,
                &event.event_type,
                event.path.as_deref().unwrap_or("-"),
                event.session_id.as_deref().unwrap_or("-"),
                &detail,
            ]);
        }

        println!("{table}");

        if events.is_empty() {
            println!("(no events)");
        }
    }

    afs.close().await?;
    Ok(())
}
