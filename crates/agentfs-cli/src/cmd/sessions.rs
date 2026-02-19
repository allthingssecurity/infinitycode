use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Subcommand)]
pub enum SessionsCommands {
    /// List sessions (active and recent)
    List {
        db: PathBuf,
        /// Number of recent sessions to show
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Start a new session
    Start {
        db: PathBuf,
        /// Unique session ID
        session_id: String,
        /// Agent name (e.g., planner, coder)
        #[arg(long)]
        agent: Option<String>,
        /// Provider name (e.g., anthropic, openai)
        #[arg(long)]
        provider: Option<String>,
    },
    /// End a session
    End {
        db: PathBuf,
        /// Session ID to end
        session_id: String,
        /// Final status: completed or failed
        #[arg(long, default_value = "completed")]
        status: String,
    },
}

pub async fn run(cmd: SessionsCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        SessionsCommands::List { db, limit } => {
            let afs = open_db(&db).await?;
            let sessions = afs.sessions.list_recent(limit).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Session ID", "Agent", "Provider", "Status", "Started", "Ended"]);

                for s in &sessions {
                    table.add_row(vec![
                        &s.session_id,
                        s.agent_name.as_deref().unwrap_or("-"),
                        s.provider.as_deref().unwrap_or("-"),
                        &s.status,
                        &s.started_at,
                        s.ended_at.as_deref().unwrap_or("-"),
                    ]);
                }

                println!("{table}");
            }
            afs.close().await?;
        }
        SessionsCommands::Start {
            db,
            session_id,
            agent,
            provider,
        } => {
            let afs = open_db(&db).await?;
            let session = afs
                .sessions
                .start(&session_id, agent.as_deref(), provider.as_deref(), None)
                .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                println!("Started session {}", session.session_id);
            }
            afs.close().await?;
        }
        SessionsCommands::End {
            db,
            session_id,
            status,
        } => {
            let afs = open_db(&db).await?;
            afs.sessions.end(&session_id, &status).await?;

            if json {
                println!("{}", serde_json::json!({ "ended": session_id, "status": status }));
            } else {
                println!("Ended session {session_id} (status: {status})");
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
