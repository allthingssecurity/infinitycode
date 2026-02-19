mod agent;
mod api;
mod auth;
mod config;
mod dashboard;
mod display;
mod error;
mod executor;
mod mcp_client;
mod memory;
mod skills;
mod streaming;
mod tools;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use uuid::Uuid;

use agentfs_core::config::AgentFSConfig;
use agentfs_core::AgentFS;

use crate::agent::Agent;
use crate::api::{AnthropicClient, LlmClient, OpenAICompatClient};
use crate::auth::AuthProvider;
use crate::config::AgentConfig;
use crate::executor::ToolExecutor;
use crate::mcp_client::McpManager;
use crate::memory::{load_memory_config, MemoryManager};
use crate::skills::SkillRegistry;

/// Return the global default DB path: `~/.infinity/infinity.db`.
fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".infinity")
        .join("infinity.db")
}

#[derive(Parser)]
#[command(name = "infinity-agent", version, about = "AI coding agent with AgentFS integration")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in with your Claude account (opens browser)
    Login,
    /// Log out and clear stored credentials
    Logout,
    /// Show authentication status
    Status,
    /// List past sessions
    Sessions {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
        /// Number of sessions to show
        #[arg(short, long, default_value = "10")]
        limit: i64,
    },
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: McpAction,
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
    /// Manage skills
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
    /// Manage memory system
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Launch web dashboard (read-only)
    Dashboard {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
        /// Port to serve on
        #[arg(long, default_value = "3210")]
        port: u16,
    },
    /// Start interactive agent (default)
    Chat {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
        /// Model to use (default depends on provider)
        #[arg(long)]
        model: Option<String>,
        /// Maximum output tokens
        #[arg(long, default_value = "8192")]
        max_tokens: u32,
        /// System prompt
        #[arg(long)]
        system: Option<String>,
        /// Single prompt (non-interactive mode)
        #[arg(short = 'p', long)]
        prompt: Option<String>,
        /// Resume a previous session by ID (or "last" for the most recent)
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// LLM provider: anthropic (default) or nvidia
        #[arg(long, default_value = "anthropic")]
        provider: String,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// List configured MCP servers
    List,
    /// Add an MCP server
    Add {
        /// Server name
        name: String,
        /// Command to run
        command: String,
        /// Arguments for the command
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Remove an MCP server
    Remove {
        /// Server name
        name: String,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// List all skills in the database
    List,
    /// Add a skill from a SKILL.md file
    Add {
        /// Skill name
        name: String,
        /// Path to a SKILL.md file
        file: PathBuf,
    },
    /// Remove a skill by name
    Remove {
        /// Skill name
        name: String,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Show memory entries (playbook, episodes, tool patterns)
    Show,
    /// Show memory statistics with tier distribution
    Stats {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
    /// Search memory entries using BM25 full-text search
    Search {
        /// Search query
        query: String,
        /// Maximum results to return
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
    /// Run compaction cycle (dedup, compress, rebalance)
    Compact {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
    /// Clear all memory data
    Clear {
        /// Path to the AgentFS database
        #[arg(long, default_value_os_t = default_db_path())]
        db: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("agentfs_agent=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Login) => cmd_login().await?,
        Some(Commands::Logout) => cmd_logout()?,
        Some(Commands::Status) => cmd_status()?,
        Some(Commands::Sessions { db, limit }) => cmd_sessions(db, limit).await?,
        Some(Commands::Dashboard { db, port }) => cmd_dashboard(db, port).await?,
        Some(Commands::Mcp { action, db }) => cmd_mcp(action, &db).await?,
        Some(Commands::Skills { action, db }) => cmd_skills(action, db).await?,
        Some(Commands::Memory { action }) => cmd_memory(action).await?,
        Some(Commands::Chat {
            db,
            model,
            max_tokens,
            system,
            prompt,
            resume,
            provider,
        }) => {
            cmd_chat(db, model, max_tokens, system, prompt, resume, provider).await?;
        }
        None => {
            cmd_chat(
                default_db_path(),
                None,
                8192,
                None,
                None,
                None,
                "anthropic".to_string(),
            )
            .await?;
        }
    }

    Ok(())
}

async fn cmd_login() -> anyhow::Result<()> {
    let mut auth = AuthProvider::load()?;
    auth.login().await?;
    println!("Successfully authenticated!");
    Ok(())
}

fn cmd_logout() -> anyhow::Result<()> {
    let auth = AuthProvider::load()?;
    auth.logout()?;
    println!("Logged out. Credentials cleared.");
    Ok(())
}

fn cmd_status() -> anyhow::Result<()> {
    let auth = AuthProvider::load()?;
    println!("{}", auth.status_string());
    Ok(())
}

async fn cmd_mcp(action: McpAction, db_path: &PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let afs_config = AgentFSConfig::builder(db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = if db_path.exists() {
        AgentFS::open(afs_config).await?
    } else {
        AgentFS::create(afs_config).await?
    };

    match action {
        McpAction::List => {
            let servers = mcp_client::list_mcp_servers_from_db(&db).await;
            display::print_mcp_server_list(&servers);
        }
        McpAction::Add { name, command, args } => {
            let entry = mcp_client::McpServerEntry {
                command: command.clone(),
                args: args.clone(),
                env: HashMap::new(),
            };
            mcp_client::save_mcp_server_to_db(&db, &name, &entry).await;
            println!("Added MCP server '{name}': {command} {}", args.join(" "));
        }
        McpAction::Remove { name } => {
            let removed = mcp_client::remove_mcp_server_from_db(&db, &name).await;
            if removed {
                println!("Removed MCP server '{name}'.");
            } else {
                println!("MCP server '{name}' not found.");
            }
        }
    }

    db.close().await?;
    Ok(())
}

async fn cmd_skills(action: SkillsAction, db_path: PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let afs_config = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = if db_path.exists() {
        AgentFS::open(afs_config).await?
    } else {
        AgentFS::create(afs_config).await?
    };

    match action {
        SkillsAction::List => {
            let skills = SkillRegistry::list_from_db(&db).await;
            if skills.is_empty() {
                println!("No skills in database.");
                println!("Add one with: infinity-agent skills add <name> <file.md> --db {}", db_path.display());
            } else {
                let pairs: Vec<(&str, &str)> = skills
                    .iter()
                    .map(|s| (s.name.as_str(), s.description.as_str()))
                    .collect();
                display::print_skills_list(&pairs);
            }
        }
        SkillsAction::Add { name, file } => {
            let content = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", file.display()))?;

            // Try to parse as SKILL.md format; otherwise treat entire file as body
            let (description, body) = parse_skill_content(&content);

            let skill = skills::Skill {
                name: name.clone(),
                description,
                body,
                dir: PathBuf::new(),
            };
            SkillRegistry::save_to_db(&db, &skill).await;
            println!("Added skill '{name}' to DB.");
        }
        SkillsAction::Remove { name } => {
            let removed = SkillRegistry::remove_from_db(&db, &name).await;
            if removed {
                println!("Removed skill '{name}' from DB.");
            } else {
                println!("Skill '{name}' not found in DB.");
            }
        }
    }

    db.close().await?;
    Ok(())
}

/// Parse SKILL.md content to extract description and body.
/// If no frontmatter is found, use empty description and the entire content as body.
fn parse_skill_content(content: &str) -> (String, String) {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        return (String::new(), trimmed.to_string());
    }

    let after_first = trimmed[3..].trim_start_matches(['\r', '\n']);
    let end_idx = match after_first.find("\n---") {
        Some(idx) => idx,
        None => return (String::new(), trimmed.to_string()),
    };
    let frontmatter = &after_first[..end_idx];
    let body_start = end_idx + 4;
    let body = if body_start < after_first.len() {
        after_first[body_start..].trim().to_string()
    } else {
        String::new()
    };

    let mut description = String::new();
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().to_string();
        }
    }

    (description, body)
}

async fn cmd_memory(action: MemoryAction) -> anyhow::Result<()> {
    match action {
        MemoryAction::Show => {
            let db_path = default_db_path();
            if !db_path.exists() {
                eprintln!("Database not found: {}", db_path.display());
                std::process::exit(1);
            }

            let afs_config = AgentFSConfig::builder(&db_path)
                .checkpoint_interval_secs(0)
                .build();
            let db = AgentFS::open(afs_config).await?;
            let db_arc = Arc::new(db);

            let mem_config = load_memory_config();
            let manager = MemoryManager::from_config(mem_config, Arc::clone(&db_arc))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Initialize providers to load data
            manager.on_session_start("__show__").await;

            // Show each provider's context
            let mut found_any = false;
            for provider in manager.providers() {
                if let Ok(Some(ctx)) = provider.context_for_prompt("").await {
                    if found_any {
                        println!();
                    }
                    println!("{ctx}");
                    found_any = true;
                }
            }
            if !found_any {
                println!("No memory entries yet.");
            }

            // db_arc is dropped here; the DB will be cleaned up by Drop.
        }
        MemoryAction::Stats { db: db_path } => {
            if !db_path.exists() {
                eprintln!("Database not found: {}", db_path.display());
                std::process::exit(1);
            }

            let afs_config = AgentFSConfig::builder(&db_path)
                .checkpoint_interval_secs(0)
                .build();
            let db = AgentFS::open(afs_config).await?;
            let db_arc = Arc::new(db);

            // Count entries by prefix
            let playbook_entries = db_arc.kv.list_prefix("memory:playbook:").await.unwrap_or_default();
            let episode_entries = db_arc.kv.list_prefix("memory:episode:").await.unwrap_or_default();
            let tool_entries = db_arc.kv.list_prefix("memory:tool_pattern:").await.unwrap_or_default();

            let mut stats = vec![
                ("playbook".to_string(), format!("{} entries", playbook_entries.len())),
                ("episodes".to_string(), format!("{} episodes", episode_entries.len())),
                ("tool_patterns".to_string(), format!("{} tools tracked", tool_entries.len())),
            ];

            // Show tier distribution
            let mem_config = load_memory_config();
            if let Ok(manager) = MemoryManager::from_config(mem_config, Arc::clone(&db_arc)).await {
                manager.on_session_start("__stats__").await;
                if let Ok((hot, warm, cold)) = manager.tier_counts().await {
                    stats.push(("tiers".to_string(), format!("{hot} hot / {warm} warm / {cold} cold")));
                }
                if let Ok(pressure) = manager.memory_pressure().await {
                    let pressure_str = match pressure {
                        memory::tiers::MemoryPressure::Low => "low",
                        memory::tiers::MemoryPressure::Medium => "medium",
                        memory::tiers::MemoryPressure::High => "high",
                    };
                    stats.push(("pressure".to_string(), pressure_str.to_string()));
                }
            }

            display::print_memory_stats(&stats);
            // db_arc is dropped here
        }
        MemoryAction::Search { query, limit, db } => {
            if !db.exists() {
                eprintln!("Database not found: {}", db.display());
                std::process::exit(1);
            }

            let afs_config = AgentFSConfig::builder(&db)
                .checkpoint_interval_secs(0)
                .build();
            let db_inst = AgentFS::open(afs_config).await?;
            let db_arc = Arc::new(db_inst);

            let mem_config = load_memory_config();
            let manager = MemoryManager::from_config(mem_config, Arc::clone(&db_arc))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Initialize to load and index data
            manager.on_session_start("__search__").await;

            match manager.search(&query, limit).await {
                Ok(results) => {
                    display::print_search_results(&query, &results);
                }
                Err(e) => {
                    eprintln!("Search error: {e}");
                }
            }
        }
        MemoryAction::Compact { db } => {
            if !db.exists() {
                eprintln!("Database not found: {}", db.display());
                std::process::exit(1);
            }

            let afs_config = AgentFSConfig::builder(&db)
                .checkpoint_interval_secs(0)
                .build();
            let db_inst = AgentFS::open(afs_config).await?;
            let db_arc = Arc::new(db_inst);

            let mem_config = load_memory_config();
            let manager = MemoryManager::from_config(mem_config, Arc::clone(&db_arc))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Initialize to load data
            manager.on_session_start("__compact__").await;

            match manager.compact().await {
                Ok(report) => {
                    display::print_compaction_report(&report);
                }
                Err(e) => {
                    eprintln!("Compaction error: {e}");
                }
            }
        }
        MemoryAction::Clear { db } => {
            if !db.exists() {
                eprintln!("Database not found: {}", db.display());
                std::process::exit(1);
            }

            let afs_config = AgentFSConfig::builder(&db)
                .checkpoint_interval_secs(0)
                .build();
            let db_inst = AgentFS::open(afs_config).await?;

            // Delete all memory keys
            let mut deleted = 0usize;
            for prefix in &["memory:playbook:", "memory:episode:", "memory:tool_pattern:"] {
                let entries = db_inst.kv.list_prefix(prefix).await.unwrap_or_default();
                for entry in &entries {
                    let _ = db_inst.kv.delete(&entry.key).await;
                    deleted += 1;
                }
            }

            // Clear metadata and FTS tables
            let writer = db_inst.writer().clone();
            let _ = writer.with_conn(|conn| {
                conn.execute("DELETE FROM memory_metadata", [])?;
                conn.execute("DELETE FROM memory_fts", [])?;
                Ok(())
            }).await;

            println!("Cleared {deleted} memory entries (+ metadata and search index).");
            db_inst.close().await?;
        }
    }
    Ok(())
}

async fn cmd_sessions(db_path: PathBuf, limit: i64) -> anyhow::Result<()> {
    if !db_path.exists() {
        eprintln!("Database not found: {}", db_path.display());
        std::process::exit(1);
    }

    let afs_config = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = AgentFS::open(afs_config).await?;

    let sessions = db.sessions.list_recent(limit).await?;
    if sessions.is_empty() {
        println!("No sessions found.");
    } else {
        println!(
            "{:<38} {:<10} {:<20} {}",
            "SESSION ID", "STATUS", "STARTED", "AGENT"
        );
        println!("{}", "-".repeat(80));
        for s in &sessions {
            println!(
                "{:<38} {:<10} {:<20} {}",
                s.session_id,
                s.status,
                &s.started_at[..19.min(s.started_at.len())],
                s.agent_name.as_deref().unwrap_or("-"),
            );
        }
        println!(
            "\nResume with: infinity-agent chat --db {} --resume <SESSION_ID>",
            db_path.display()
        );
    }

    db.close().await?;
    Ok(())
}

async fn cmd_dashboard(db_path: PathBuf, port: u16) -> anyhow::Result<()> {
    if !db_path.exists() {
        eprintln!("Database not found: {}", db_path.display());
        std::process::exit(1);
    }

    let afs_config = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = AgentFS::open(afs_config).await?;
    let db_arc = Arc::new(db);

    let mem_config = load_memory_config();
    let memory = match MemoryManager::from_config(mem_config, Arc::clone(&db_arc)).await {
        Ok(m) => {
            m.on_session_start("__dashboard__").await;
            Arc::new(m)
        }
        Err(e) => {
            eprintln!("Warning: memory system init failed: {e}");
            // Create a minimal memory manager with disabled config
            let mut cfg = crate::memory::MemoryConfig::default();
            cfg.providers.clear();
            Arc::new(
                MemoryManager::from_config(cfg, Arc::clone(&db_arc))
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
            )
        }
    };

    dashboard::run_dashboard(db_arc, memory, port).await
}

async fn resolve_last_session(db: &AgentFS) -> (String, bool) {
    let recent = db.sessions.list_recent(1).await.unwrap_or_default();
    match recent.first() {
        Some(s) => {
            println!("Resuming session: {}", s.session_id);
            (s.session_id.clone(), true)
        }
        None => {
            println!("No previous sessions found. Starting new session.");
            (Uuid::new_v4().to_string(), false)
        }
    }
}

/// Resolve a model shorthand or full name into a new LlmClient + model string.
fn resolve_model_switch(
    name: &str,
    max_tokens: u32,
) -> std::result::Result<(LlmClient, String, String), String> {
    let name_lower = name.to_lowercase();

    // Check presets first
    for (shorthand, provider, model_id, _desc) in display::MODEL_PRESETS {
        if name_lower == *shorthand || name_lower == *model_id {
            let client = create_client_for_provider(provider, model_id, max_tokens)?;
            return Ok((client, model_id.to_string(), provider.to_string()));
        }
    }

    // Allow direct provider names
    match name_lower.as_str() {
        "nvidia" => {
            let model = "moonshotai/kimi-k2.5";
            let client = create_client_for_provider("nvidia", model, max_tokens)?;
            Ok((client, model.to_string(), "nvidia".to_string()))
        }
        "openrouter" => {
            let model = "moonshotai/kimi-k2.5";
            let client = create_client_for_provider("openrouter", model, max_tokens)?;
            Ok((client, model.to_string(), "openrouter".to_string()))
        }
        "anthropic" => {
            let model = "claude-sonnet-4-6";
            let client = create_client_for_provider("anthropic", model, max_tokens)?;
            Ok((client, model.to_string(), "anthropic".to_string()))
        }
        _ => Err(format!(
            "Unknown model '{name}'. Use: sonnet, opus, haiku, kimi, openrouter, anthropic"
        )),
    }
}

/// Create an LlmClient for a given provider and model.
fn create_client_for_provider(
    provider: &str,
    model: &str,
    max_tokens: u32,
) -> std::result::Result<LlmClient, String> {
    match provider {
        "nvidia" => {
            let api_key = std::env::var("NVIDIA_API_KEY")
                .map_err(|_| "NVIDIA_API_KEY not set. Export it first: export NVIDIA_API_KEY=nvapi-...".to_string())?;
            Ok(LlmClient::OpenAICompat(OpenAICompatClient::nvidia(
                api_key,
                model.to_string(),
                max_tokens,
            )))
        }
        "openrouter" => {
            let api_key = std::env::var("OPENROUTER_API_KEY")
                .map_err(|_| "OPENROUTER_API_KEY not set. Export it first: export OPENROUTER_API_KEY=sk-or-...".to_string())?;
            Ok(LlmClient::OpenAICompat(OpenAICompatClient::openrouter(
                api_key,
                model.to_string(),
                max_tokens,
            )))
        }
        "anthropic" | _ => Ok(LlmClient::Anthropic(AnthropicClient::new(
            model.to_string(),
            max_tokens,
        ))),
    }
}

async fn cmd_chat(
    db_path: PathBuf,
    model: Option<String>,
    max_tokens: u32,
    system: Option<String>,
    prompt: Option<String>,
    resume: Option<String>,
    provider: String,
) -> anyhow::Result<()> {
    // Resolve model default based on provider
    let model = model.unwrap_or_else(|| match provider.as_str() {
        "nvidia" | "openrouter" => "moonshotai/kimi-k2.5".to_string(),
        _ => "claude-sonnet-4-6".to_string(),
    });

    let mut config = AgentConfig::from_args(db_path.clone(), model.clone(), max_tokens, system)?;

    // For non-Anthropic providers, check their API key instead
    if provider == "nvidia" {
        if std::env::var("NVIDIA_API_KEY").is_err() {
            eprintln!("NVIDIA_API_KEY environment variable not set.");
            eprintln!("Set it with: export NVIDIA_API_KEY=nvapi-...");
            std::process::exit(1);
        }
    } else if provider == "openrouter" {
        if std::env::var("OPENROUTER_API_KEY").is_err() {
            eprintln!("OPENROUTER_API_KEY environment variable not set.");
            eprintln!("Set it with: export OPENROUTER_API_KEY=sk-or-...");
            std::process::exit(1);
        }
    } else if !config.auth.is_authenticated() {
        eprintln!("Not authenticated. Run `infinity-agent login` or set ANTHROPIC_API_KEY.");
        std::process::exit(1);
    }

    // Ensure parent directory exists (e.g. ~/.infinity/)
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Open or create AgentFS database
    let afs_config = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(30)
        .build();

    let db = if db_path.exists() {
        AgentFS::open(afs_config).await?
    } else {
        AgentFS::create(afs_config).await?
    };

    // Resolve session ID:
    //   --resume <id>   → resume that specific session
    //   --resume last   → resume most recent session
    //   (no flag)       → auto-resume last session if it has saved messages, else new
    let (session_id, is_resume) = match &resume {
        Some(arg) if arg == "last" => {
            resolve_last_session(&db).await
        }
        Some(id) => {
            match db.sessions.get(id).await {
                Ok(_) => {
                    println!("Resuming session: {id}");
                    (id.clone(), true)
                }
                Err(_) => {
                    eprintln!("Session not found: {id}");
                    std::process::exit(1);
                }
            }
        }
        None => {
            // Check if there's a previous session with saved messages
            let recent = db.sessions.list_recent(1).await?;
            if let Some(s) = recent.first() {
                let key = format!("session:messages:{}", s.session_id);
                match db.kv.get(&key).await {
                    Ok(entry) if entry.value.len() > 2 => {
                        // Ask the user whether to resume or start fresh
                        let short_id = if s.session_id.len() > 8 {
                            &s.session_id[..8]
                        } else {
                            &s.session_id
                        };
                        let started = &s.started_at[..19.min(s.started_at.len())];
                        println!(
                            "\n  {}Previous session found:{} {}{}...{} ({})",
                            "\x1b[1m", "\x1b[0m",
                            "\x1b[36m", short_id, "\x1b[0m",
                            started,
                        );
                        print!("  Resume previous session? [Y/n] ");
                        std::io::stdout().flush().ok();

                        let mut answer = String::new();
                        std::io::stdin().read_line(&mut answer).ok();
                        let answer = answer.trim().to_lowercase();

                        if answer.is_empty() || answer == "y" || answer == "yes" {
                            (s.session_id.clone(), true)
                        } else {
                            (Uuid::new_v4().to_string(), false)
                        }
                    }
                    _ => (Uuid::new_v4().to_string(), false),
                }
            } else {
                (Uuid::new_v4().to_string(), false)
            }
        }
    };

    // Start or reopen session
    if !is_resume {
        db.sessions
            .start(&session_id, Some("infinity-agent"), Some("anthropic"), None)
            .await?;
        db.events
            .log(Some(&session_id), "session_start", None, Some(&model))
            .await?;
    } else {
        db.events
            .log(Some(&session_id), "session_resume", None, Some(&model))
            .await?;
    }

    // Load MCP servers (from DB with filesystem fallback)
    let mcp_manager = McpManager::from_db_config(&db).await;
    let mcp_tools = mcp_manager.all_tool_definitions();

    let mcp_arc = Arc::new(Mutex::new(mcp_manager));

    // Load skills (from DB with filesystem fallback)
    let skill_registry = SkillRegistry::load_from_db(&db).await;

    // Load memory system
    let mem_config = load_memory_config();
    let db_arc = Arc::new(db);
    let memory_manager = if mem_config.enabled {
        match MemoryManager::from_config(mem_config.clone(), Arc::clone(&db_arc)).await {
            Ok(manager) => {
                let mgr = Arc::new(manager);
                // Notify providers of session start
                mgr.on_session_start(&session_id).await;
                Some(mgr)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize memory system: {e}");
                None
            }
        }
    } else {
        None
    };

    // Open a second DB connection for the executor (the memory system holds its own Arc).
    let executor_db = {
        let afs_config2 = AgentFSConfig::builder(&db_path)
            .checkpoint_interval_secs(0) // Only the primary connection checkpoints
            .build();
        AgentFS::open(afs_config2).await?
    };

    let client: LlmClient = match provider.as_str() {
        "nvidia" => {
            let api_key = std::env::var("NVIDIA_API_KEY").unwrap();
            LlmClient::OpenAICompat(OpenAICompatClient::nvidia(api_key, model.clone(), max_tokens))
        }
        "openrouter" => {
            let api_key = std::env::var("OPENROUTER_API_KEY").unwrap();
            LlmClient::OpenAICompat(OpenAICompatClient::openrouter(api_key, model.clone(), max_tokens))
        }
        _ => LlmClient::Anthropic(AnthropicClient::new(model.clone(), max_tokens)),
    };
    let executor = ToolExecutor::new(executor_db, session_id.clone()).with_mcp(Arc::clone(&mcp_arc));

    let mut default_system = config.system_prompt.take().unwrap_or_else(|| {
        "You are Infinity Agent, an AI coding assistant.\n\n\
         You have two separate environments:\n\n\
         1. **Workspace (AgentFS)** — a persistent virtual filesystem stored in a database.\n\
         Tools: read_file, write_file, list_dir, search, tree, kv_get, kv_set.\n\
         Paths like /src/main.rs live ONLY in this virtual DB — they are NOT on the host disk.\n\n\
         2. **Host shell** — the user's real machine.\n\
         Tool: bash. This runs real commands on the host OS.\n\
         Files on the host are at normal paths like /tmp/foo.py or ~/project/.\n\n\
         IMPORTANT RULES:\n\
         - If the user asks you to write and RUN code, use `bash` to write it to a temp \
         location on the host (e.g. write via bash: echo '...' > /tmp/script.py) and then \
         run it with bash. Do NOT write to AgentFS and then try to run it — the virtual \
         filesystem is not mounted on the host.\n\
         - Use AgentFS (write_file/read_file) for persistent notes, project files, or \
         artifacts the user wants to keep across sessions.\n\
         - Use bash for everything that needs to execute: running code, git, installs, etc.\n\
         - Keep responses concise. Show code, not explanations unless asked."
            .to_string()
    });

    // Inject skills section into system prompt
    if let Some(section) = skill_registry.system_prompt_section() {
        default_system.push_str(&section);
    }

    let mut agent = Agent::new(
        client,
        executor,
        Some(default_system),
        session_id.clone(),
        model.clone(),
        mcp_tools,
    );

    // Attach memory manager if available
    if let Some(ref mgr) = memory_manager {
        agent = agent.with_memory(Arc::clone(mgr));
    }

    // If resuming, load persisted messages
    if is_resume {
        let count = agent.load_messages().await?;
        if count > 0 {
            println!("Loaded {count} messages from previous session.");
        }
    }

    // Single-prompt mode
    if let Some(prompt) = prompt {
        agent.run_turn(&mut config.auth, &prompt).await?;
        println!();

        // End memory session
        if let Some(ref mgr) = memory_manager {
            mgr.on_session_end(&session_id).await;
        }

        let executor = agent.into_executor();
        executor.db.sessions.end(&session_id, "completed").await?;
        mcp_arc.lock().await.shutdown().await;
        executor.db.close().await?;
        // Close memory DB
        if let Some(db) = Arc::into_inner(db_arc) {
            db.close().await?;
        }
        return Ok(());
    }

    // Interactive REPL
    display::print_banner(&model, &db_path.display().to_string());

    // Build memory info string
    let memory_info_string = if let Some(ref mgr) = memory_manager {
        let executor_ref = agent.executor();
        let playbook_count = executor_ref.db.kv
            .list_prefix("memory:playbook:")
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        let episode_count = executor_ref.db.kv
            .list_prefix("memory:episode:")
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        let has_reflect = if mgr.has_reflector() { " + reflection" } else { "" };
        Some(format!(
            "{playbook_count} playbook entries, {episode_count} episodes{has_reflect}"
        ))
    } else {
        None
    };

    // Get MCP summary
    let mcp_summary = {
        let manager = mcp_arc.lock().await;
        manager.server_summary().into_iter().map(|(n, c)| (n.to_string(), c)).collect::<Vec<_>>()
    };

    // Built-in tool names
    let builtin_tools: Vec<&str> = vec![
        "read_file", "write_file", "bash", "list_dir", "search", "tree", "kv_get", "kv_set",
    ];

    // Count loaded messages for resume
    let loaded_msg_count = agent.message_count();

    display::print_startup_status(
        &session_id,
        loaded_msg_count,
        is_resume,
        memory_info_string.as_deref(),
        &builtin_tools,
        &mcp_summary,
    );

    let mut rl = rustyline::DefaultEditor::new()?;
    let prompt = display::prompt_string();

    loop {
        display::print_separator();
        let line = match rl.readline(&prompt) {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                // Ctrl+C at prompt — don't exit, just show new prompt
                println!();
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                // Ctrl+D — exit
                break;
            }
            Err(e) => {
                eprintln!("Input error: {e}");
                break;
            }
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "/quit" | "/exit" => break,
            "/clear" => {
                agent.clear();
                println!("Conversation cleared. (still same session — messages cleared from memory only)");
                continue;
            }
            "/new" => {
                agent.clear();
                println!("Starting fresh conversation. (previous messages still saved in DB)");
                continue;
            }
            "/tokens" => {
                let (input_t, output_t) = agent.token_counts();
                println!("Session tokens: {input_t} input, {output_t} output");
                continue;
            }
            "/session" => {
                println!("Session ID: {session_id}");
                continue;
            }
            "/skills" => {
                display::print_skills_list(&skill_registry.list());
                continue;
            }
            "/mcp" => {
                let manager = mcp_arc.lock().await;
                let summary = manager.server_summary();
                if summary.is_empty() {
                    println!("No MCP servers connected.");
                    println!("Configure with: infinity-agent mcp add <name> <command> [args...]");
                } else {
                    println!("Connected MCP servers:");
                    for (name, count) in &summary {
                        println!("  {name} ({count} tools)");
                    }
                }
                continue;
            }
            "/help" => {
                println!("Commands:");
                println!("  /model [name]  — Show or switch model (sonnet, opus, haiku, kimi)");
                println!("  /mcp           — Show connected MCP servers");
                println!("  /skills        — List available skills");
                println!("  /memory        — Show memory stats");
                println!("  /tokens        — Show session token usage");
                println!("  /session       — Show current session ID");
                println!("  /clear         — Clear conversation history");
                println!("  /new           — Start fresh conversation");
                println!("  /quit          — Exit");
                continue;
            }
            _ if input.starts_with("/model") => {
                let arg = input["/model".len()..].trim();
                if arg.is_empty() {
                    display::print_model_info(agent.model_name(), agent.provider_name());
                } else {
                    // Resolve model preset
                    match resolve_model_switch(arg, max_tokens) {
                        Ok((new_client, new_model, new_provider)) => {
                            agent.set_client(new_client, new_model.clone());
                            display::print_model_switched(&new_model, &new_provider);
                        }
                        Err(msg) => {
                            display::print_model_error(&msg);
                        }
                    }
                }
                continue;
            }
            "/memory" => {
                if let Some(ref mgr) = memory_manager {
                    // Get stats from each provider via KV prefix counts
                    let executor = agent.executor();
                    let playbook_count = executor.db.kv
                        .list_prefix("memory:playbook:")
                        .await
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let episode_count = executor.db.kv
                        .list_prefix("memory:episode:")
                        .await
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let tool_count = executor.db.kv
                        .list_prefix("memory:tool_pattern:")
                        .await
                        .map(|v| v.len())
                        .unwrap_or(0);

                    let mut stats = vec![
                        ("playbook".to_string(), format!("{playbook_count} entries")),
                        ("episodes".to_string(), format!("{episode_count} episodes")),
                        ("tool_patterns".to_string(), format!("{tool_count} tools tracked")),
                    ];

                    // Show tier distribution
                    if let Ok((hot, warm, cold)) = mgr.tier_counts().await {
                        stats.push(("tiers".to_string(), format!("{hot} hot / {warm} warm / {cold} cold")));
                    }
                    if let Ok(pressure) = mgr.memory_pressure().await {
                        let pressure_str = match pressure {
                            memory::tiers::MemoryPressure::Low => "low",
                            memory::tiers::MemoryPressure::Medium => "medium",
                            memory::tiers::MemoryPressure::High => "high",
                        };
                        stats.push(("pressure".to_string(), pressure_str.to_string()));
                    }

                    display::print_memory_stats(&stats);
                    println!(
                        "\nManage with: infinity-agent memory show | stats | search <query> | compact | clear"
                    );
                } else {
                    println!("Memory system is not enabled.");
                    println!("Create ~/.infinity/memory.json with {{\"enabled\": true}} to enable.");
                }
                continue;
            }
            _ => {}
        }

        // Check if input matches a skill invocation
        if let Some((skill, args)) = skill_registry.matches_command(input) {
            rl.add_history_entry(input)?;
            let args_str = if args.is_empty() {
                format!("Run the /{} skill", skill.name)
            } else {
                args.to_string()
            };
            let before = agent.message_count();
            let result = tokio::select! {
                r = agent.run_skill_turn(&mut config.auth, &skill.body, &args_str) => Some(r),
                _ = tokio::signal::ctrl_c() => None,
            };
            match result {
                Some(Ok(_)) => {}
                Some(Err(e)) => eprintln!("\nError: {e}"),
                None => {
                    agent.rollback_to(before);
                    display::print_cancelled();
                }
            }
            continue;
        }

        rl.add_history_entry(input)?;

        let before = agent.message_count();
        let result = tokio::select! {
            r = agent.run_turn(&mut config.auth, input) => Some(r),
            _ = tokio::signal::ctrl_c() => None,
        };
        match result {
            Some(Ok(_)) => {}
            Some(Err(e)) => eprintln!("\nError: {e}"),
            None => {
                agent.rollback_to(before);
                display::print_cancelled();
            }
        }
    }

    // End session
    println!("\nEnding session...");
    let (input_t, output_t) = agent.token_counts();
    println!("Total tokens: {input_t} input, {output_t} output");

    // End memory session
    if let Some(ref mgr) = memory_manager {
        mgr.on_session_end(&session_id).await;
    }

    let executor = agent.into_executor();
    executor
        .db
        .sessions
        .end(&session_id, "completed")
        .await?;
    executor
        .db
        .events
        .log(Some(&session_id), "session_end", None, None)
        .await?;

    // Shutdown MCP servers before closing DB
    mcp_arc.lock().await.shutdown().await;
    executor.db.close().await?;

    // Close memory DB
    if let Some(db) = Arc::into_inner(db_arc) {
        db.close().await?;
    }

    Ok(())
}
