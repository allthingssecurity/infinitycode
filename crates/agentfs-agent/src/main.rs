mod agent;
mod api;
mod auth;
mod config;
mod display;
mod error;
mod executor;
mod streaming;
mod tools;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

use agentfs_core::config::AgentFSConfig;
use agentfs_core::AgentFS;

use crate::agent::Agent;
use crate::api::AnthropicClient;
use crate::auth::AuthProvider;
use crate::config::AgentConfig;
use crate::executor::ToolExecutor;

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
        #[arg(long, default_value = "infinity.db")]
        db: PathBuf,
        /// Number of sessions to show
        #[arg(short, long, default_value = "10")]
        limit: i64,
    },
    /// Start interactive agent (default)
    Chat {
        /// Path to the AgentFS database
        #[arg(long, default_value = "infinity.db")]
        db: PathBuf,
        /// Model to use
        #[arg(long, default_value = "claude-sonnet-4-6")]
        model: String,
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
        Some(Commands::Chat {
            db,
            model,
            max_tokens,
            system,
            prompt,
            resume,
        }) => {
            cmd_chat(db, model, max_tokens, system, prompt, resume).await?;
        }
        None => {
            cmd_chat(
                PathBuf::from("infinity.db"),
                "claude-sonnet-4-6".to_string(),
                8192,
                None,
                None,
                None,
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

async fn cmd_chat(
    db_path: PathBuf,
    model: String,
    max_tokens: u32,
    system: Option<String>,
    prompt: Option<String>,
    resume: Option<String>,
) -> anyhow::Result<()> {
    let mut config = AgentConfig::from_args(db_path.clone(), model.clone(), max_tokens, system)?;

    if !config.auth.is_authenticated() {
        eprintln!("Not authenticated. Run `infinity-agent login` or set ANTHROPIC_API_KEY.");
        std::process::exit(1);
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
            // Auto-resume: check if the last session has saved messages
            let recent = db.sessions.list_recent(1).await?;
            if let Some(s) = recent.first() {
                let key = format!("session:messages:{}", s.session_id);
                match db.kv.get(&key).await {
                    Ok(entry) if entry.value.len() > 2 => {
                        println!("Auto-resuming session: {}", s.session_id);
                        (s.session_id.clone(), true)
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

    let client = AnthropicClient::new(model.clone(), max_tokens);
    let executor = ToolExecutor::new(db, session_id.clone());

    let default_system = config.system_prompt.take().unwrap_or_else(|| {
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

    let mut agent = Agent::new(
        client,
        executor,
        Some(default_system),
        session_id.clone(),
        model.clone(),
    );

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
        let executor = agent.into_executor();
        executor.db.sessions.end(&session_id, "completed").await?;
        executor.db.close().await?;
        return Ok(());
    }

    // Interactive REPL
    display::print_banner(&model, &db_path.display().to_string());
    if is_resume {
        println!(
            "  \x1b[36mresumed session: {session_id}\x1b[0m"
        );
    }

    let mut rl = rustyline::DefaultEditor::new()?;
    let prompt = display::prompt_string();

    loop {
        display::print_separator();
        let line = match rl.readline(&prompt) {
            Ok(line) => line,
            Err(
                rustyline::error::ReadlineError::Interrupted
                | rustyline::error::ReadlineError::Eof,
            ) => {
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
            _ => {}
        }

        rl.add_history_entry(input)?;

        match agent.run_turn(&mut config.auth, input).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("\nError: {e}");
            }
        }
    }

    // End session
    println!("\nEnding session...");
    let (input_t, output_t) = agent.token_counts();
    println!("Total tokens: {input_t} input, {output_t} output");

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
    executor.db.close().await?;

    Ok(())
}
