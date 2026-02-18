use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossterm::cursor;
use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor, ResetColor};
use crossterm::terminal;
use crossterm::ExecutableCommand;

use crate::streaming::{ContentBlockType, StreamEvent, ToolUseBlock};

// ── Spinner ─────────────────────────────────────────────────────────

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const THINKING_MESSAGES: &[&str] = &[
    "Thinking",
    "Reasoning",
    "Analyzing",
    "Processing",
    "Considering",
    "Reflecting",
    "Evaluating",
    "Pondering",
];

/// A terminal spinner that runs in a background task.
pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with a message.
    pub fn start(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let message = message.to_string();

        let handle = tokio::spawn(async move {
            let mut frame = 0usize;
            let mut stdout = std::io::stdout();

            // Hide cursor
            let _ = stdout.execute(cursor::Hide);

            while running_clone.load(Ordering::Relaxed) {
                let spinner = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
                print!(
                    "\r{}{}  {spinner} {message}{}    ",
                    SetForegroundColor(Color::Magenta),
                    SetAttribute(Attribute::Bold),
                    SetAttribute(Attribute::Reset),
                );
                let _ = stdout.flush();
                frame += 1;
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }

            // Clear spinner line
            print!("\r{}\r", " ".repeat(80));
            let _ = stdout.flush();

            // Show cursor
            let _ = stdout.execute(cursor::Show);
        });

        Self {
            running,
            handle: Some(handle),
        }
    }

    /// Start a thinking spinner with a random message.
    pub fn thinking() -> Self {
        let idx = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as usize;
        let msg = THINKING_MESSAGES[idx % THINKING_MESSAGES.len()];
        Self::start(&format!("{msg}..."))
    }

    /// Stop the spinner.
    pub async fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Best-effort show cursor
        let _ = std::io::stdout().execute(cursor::Show);
    }
}

// ── Stream events ───────────────────────────────────────────────────

/// Print a stream event to the terminal in real-time.
pub fn print_stream_event(event: &StreamEvent) {
    let mut stdout = std::io::stdout();

    match event {
        StreamEvent::TextDelta { text, .. } => {
            print!("{text}");
            let _ = stdout.flush();
        }
        StreamEvent::ContentBlockStart {
            block_type: ContentBlockType::ToolUse { .. },
            ..
        } => {
            // Handled by print_tool_call after accumulation
        }
        StreamEvent::ContentBlockStop { .. } | StreamEvent::InputJsonDelta { .. } => {}
        StreamEvent::Error { message } => {
            eprintln!(
                "\n{}{}  \u{2716} Error: {message}{}",
                SetForegroundColor(Color::Red),
                SetAttribute(Attribute::Bold),
                SetAttribute(Attribute::Reset),
            );
        }
        _ => {}
    }
}

// ── Tool calls ──────────────────────────────────────────────────────

/// Print a tool execution header — clean Claude Code style.
pub fn print_tool_call(tool: &ToolUseBlock) {
    let mut stdout = std::io::stdout();

    print!(
        "\n{}{}  \u{25CF} {}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        tool_display_name(&tool.name),
    );
    print!("{}", SetAttribute(Attribute::Reset));

    if let Some(obj) = tool.input.as_object() {
        let summary = tool_param_summary(&tool.name, obj);
        if !summary.is_empty() {
            print!(
                "{}: {summary}{}",
                SetForegroundColor(Color::Cyan),
                ResetColor,
            );
        }
    }

    println!();
    let _ = stdout.flush();
}

/// Print tool execution result with a subtle left border.
pub fn print_tool_result(_tool_name: &str, result: &str, is_error: bool) {
    let color = if is_error { Color::Red } else { Color::DarkGrey };

    let display = if result.len() > 2000 {
        format!("{}… ({} bytes)", &result[..2000], result.len())
    } else {
        result.to_string()
    };

    for line in display.lines() {
        println!(
            "{}    \u{2502} {line}{}",
            SetForegroundColor(color),
            ResetColor,
        );
    }
}

/// Start a spinner for tool execution.
pub fn tool_spinner(tool_name: &str) -> Spinner {
    let verb = match tool_name {
        "read_file" => "Reading file",
        "write_file" => "Writing file",
        "list_dir" => "Listing directory",
        "search" => "Searching files",
        "tree" => "Building tree",
        "bash" => "Running command",
        "kv_get" => "Reading key",
        "kv_set" => "Writing key",
        _ => "Executing",
    };
    Spinner::start(&format!("{verb}..."))
}

// ── Chrome ──────────────────────────────────────────────────────────

/// Print token usage summary.
pub fn print_token_usage(input_tokens: u64, output_tokens: u64) {
    println!(
        "\n{}{}  \u{2500} {input_tokens} in + {output_tokens} out tokens{}",
        SetForegroundColor(Color::DarkGrey),
        SetAttribute(Attribute::Dim),
        SetAttribute(Attribute::Reset),
    );
}

/// Print the separator line above the prompt.
pub fn print_separator() {
    let width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
    let line: String = "\u{2500}".repeat(width);
    println!(
        "\n{}{}{}",
        SetForegroundColor(Color::DarkGrey),
        line,
        ResetColor,
    );
}

/// Return the prompt string for rustyline (with ANSI colors).
pub fn prompt_string() -> String {
    "\x1b[1;34m\u{221e}\x1b[0m ".to_string()
}

/// Print a welcome banner.
pub fn print_banner(model: &str, db_path: &str) {
    println!(
        "\n  {}{}\u{221e} Infinity Agent{}",
        SetForegroundColor(Color::Magenta),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    println!(
        "  {}model: {model}  \u{2502}  db: {db_path}{}",
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
    println!(
        "  {}commands: /quit  /new  /clear  /tokens  /session{}",
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
}

// ── Helpers ─────────────────────────────────────────────────────────

fn tool_display_name(name: &str) -> &str {
    match name {
        "read_file" => "Read",
        "write_file" => "Write",
        "list_dir" => "List",
        "search" => "Search",
        "tree" => "Tree",
        "bash" => "Bash",
        "kv_get" => "KV Get",
        "kv_set" => "KV Set",
        _ => name,
    }
}

fn tool_param_summary(tool_name: &str, params: &serde_json::Map<String, serde_json::Value>) -> String {
    match tool_name {
        "read_file" | "list_dir" | "tree" => params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "write_file" => {
            let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let len = params
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0);
            format!("{path} ({len} bytes)")
        }
        "search" => params
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "bash" => {
            let cmd = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.len() > 80 {
                format!("{}…", &cmd[..77])
            } else {
                cmd.to_string()
            }
        }
        "kv_get" => params
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "kv_set" => {
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            key.to_string()
        }
        _ => String::new(),
    }
}
