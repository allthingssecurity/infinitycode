use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Subcommand)]
pub enum KvCommands {
    /// Get a value by key
    Get {
        db: PathBuf,
        key: String,
    },
    /// Set a key-value pair
    Set {
        db: PathBuf,
        key: String,
        value: String,
    },
    /// Delete a key
    Delete {
        db: PathBuf,
        key: String,
    },
    /// List all keys (or keys with a prefix)
    List {
        db: PathBuf,
        /// Optional prefix filter
        #[arg(long)]
        prefix: Option<String>,
    },
}

pub async fn run(cmd: KvCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        KvCommands::Get { db, key } => {
            let afs = open_db(&db).await?;
            let entry = afs.kv.get(&key).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("{}", entry.value);
            }
            afs.close().await?;
        }
        KvCommands::Set { db, key, value } => {
            let afs = open_db(&db).await?;
            afs.kv.set(&key, &value).await?;
            if json {
                println!("{}", serde_json::json!({ "set": key }));
            } else {
                println!("Set {key}");
            }
            afs.close().await?;
        }
        KvCommands::Delete { db, key } => {
            let afs = open_db(&db).await?;
            afs.kv.delete(&key).await?;
            if json {
                println!("{}", serde_json::json!({ "deleted": key }));
            } else {
                println!("Deleted {key}");
            }
            afs.close().await?;
        }
        KvCommands::List { db, prefix } => {
            let afs = open_db(&db).await?;
            let entries = if let Some(prefix) = &prefix {
                afs.kv.list_prefix(prefix).await?
            } else {
                afs.kv.list_prefix("").await?
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Key", "Value", "Updated"]);

                for entry in &entries {
                    let val = if entry.value.len() > 60 {
                        format!("{}...", &entry.value[..57])
                    } else {
                        entry.value.clone()
                    };
                    table.add_row(vec![&entry.key, &val, &entry.updated]);
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
