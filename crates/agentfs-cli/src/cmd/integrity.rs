use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum IntegrityCommands {
    /// Quick integrity check (SQLite + checksum summary)
    Check {
        db: PathBuf,
    },
    /// Full scrub — verify every chunk checksum
    Scrub {
        db: PathBuf,
    },
}

pub async fn run(cmd: IntegrityCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        IntegrityCommands::Check { db } | IntegrityCommands::Scrub { db } => {
            let config = AgentFSConfig::builder(&db)
                .checkpoint_interval_secs(0)
                .build();
            let afs = agentfs_core::AgentFS::open(config).await?;
            let report = afs.integrity_check().await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Integrity Report:");
                println!("  SQLite integrity: {}", if report.sqlite_integrity_ok { "OK" } else { "FAILED" });
                println!("  Total chunks:     {}", report.total_chunks);
                println!("  Verified OK:      {}", report.verified_chunks);
                println!("  Corrupt:          {}", report.corrupt_chunks.len());

                if !report.corrupt_chunks.is_empty() {
                    println!();
                    println!("Corrupt chunks:");
                    for c in &report.corrupt_chunks {
                        println!(
                            "  ino={} chunk={}: expected={:#018x} actual={:#018x}",
                            c.ino, c.chunk_index, c.expected, c.actual
                        );
                    }
                }

                if report.is_clean() {
                    println!("\nAll checks passed.");
                } else {
                    println!("\nIntegrity issues detected!");
                    std::process::exit(1);
                }
            }

            afs.close().await?;
        }
    }
    Ok(())
}
