use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

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

const FOLLOW_UP_MESSAGES: &[&str] = &[
    "Reviewing results",
    "Analyzing output",
    "Processing feedback",
    "Formulating next step",
    "Synthesizing",
    "Connecting the dots",
    "Iterating",
    "Working through it",
];

const SPINNER_COLORS: &[Color] = &[Color::Magenta, Color::Blue, Color::Cyan, Color::Blue];

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

            let start = Instant::now();
            while running_clone.load(Ordering::Relaxed) {
                let spinner = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
                let elapsed = start.elapsed().as_secs_f32();
                let color = SPINNER_COLORS[(frame / 4) % SPINNER_COLORS.len()];
                print!(
                    "\r{}{}  {spinner} {message} {}({elapsed:.1}s){}      ",
                    SetForegroundColor(color),
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::DarkGrey),
                    SetAttribute(Attribute::Reset),
                );
                let _ = stdout.flush();
                frame += 1;
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }

            // Clear spinner line
            let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(120);
            print!("\r{}\r", " ".repeat(width));
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

    /// Start a follow-up spinner (after tool execution, returning to API).
    pub fn thinking_follow_up() -> Self {
        let idx = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as usize;
        let msg = FOLLOW_UP_MESSAGES[idx % FOLLOW_UP_MESSAGES.len()];
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

/// Print a non-text stream event to the terminal.
/// TextDelta events should go through StreamRenderer instead.
pub fn print_stream_event(event: &StreamEvent) {
    match event {
        StreamEvent::TextDelta { .. } => {
            // Handled by StreamRenderer — should not reach here
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

// ── Rich Stream Renderer ────────────────────────────────────────────

/// Rich stream renderer: code blocks with borders and diff coloring,
/// prose streams character-by-character for a live feel.
pub struct StreamRenderer {
    in_code_block: bool,
    line_buffer: String,
}

impl StreamRenderer {
    pub fn new() -> Self {
        Self {
            in_code_block: false,
            line_buffer: String::new(),
        }
    }

    /// Push a text delta chunk. Handles line buffering and rich rendering.
    pub fn push(&mut self, text: &str) {
        let mut stdout = std::io::stdout();

        for ch in text.chars() {
            self.line_buffer.push(ch);

            if ch == '\n' {
                let line = self.line_buffer[..self.line_buffer.len() - 1].to_string();
                let is_fence = line.trim_start().starts_with("```");

                if is_fence && !self.in_code_block {
                    // Opening code fence — overwrite the streamed backticks
                    let clear_len = line.len() + 4;
                    print!("\r{}\r", " ".repeat(clear_len));
                    self.in_code_block = true;
                    let lang = line.trim_start().trim_start_matches('`').trim();
                    print!(
                        "{}  \u{250C}\u{2500}",
                        SetForegroundColor(Color::DarkGrey),
                    );
                    if !lang.is_empty() {
                        print!(
                            " {}{}{}",
                            SetForegroundColor(Color::Cyan),
                            lang,
                            SetForegroundColor(Color::DarkGrey),
                        );
                    }
                    println!("{}", ResetColor);
                } else if is_fence && self.in_code_block {
                    // Closing code fence
                    self.in_code_block = false;
                    println!(
                        "{}  \u{2514}\u{2500}{}",
                        SetForegroundColor(Color::DarkGrey),
                        ResetColor,
                    );
                } else if self.in_code_block {
                    // Code block line: render with diff awareness
                    Self::render_code_line(&line);
                    println!();
                } else {
                    // Prose: chars already streamed, just emit newline
                    println!();
                }

                self.line_buffer.clear();
                let _ = stdout.flush();
            } else if !self.in_code_block {
                // Prose: print immediately for live streaming feel
                print!("{ch}");
                let _ = stdout.flush();
            }
            // In code block: chars are buffered until newline for line-level coloring
        }
    }

    /// Render a code line with diff coloring and subtle border.
    fn render_code_line(line: &str) {
        let trimmed = line.trim_start();

        if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
            // Added line — green bold
            print!(
                "{}  \u{2502}{}{}{}{}",
                SetForegroundColor(Color::DarkGrey),
                SetForegroundColor(Color::Green),
                SetAttribute(Attribute::Bold),
                line,
                SetAttribute(Attribute::Reset),
            );
        } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
            // Removed line — red dim
            print!(
                "{}  \u{2502}{}{}{}{}",
                SetForegroundColor(Color::DarkGrey),
                SetForegroundColor(Color::Red),
                SetAttribute(Attribute::Dim),
                line,
                SetAttribute(Attribute::Reset),
            );
        } else if trimmed.starts_with("@@") {
            // Diff hunk header — cyan
            print!(
                "{}  {}{}",
                SetForegroundColor(Color::Cyan),
                line,
                ResetColor,
            );
        } else {
            // Normal code line with subtle border
            print!(
                "{}  \u{2502}{} {}",
                SetForegroundColor(Color::DarkGrey),
                ResetColor,
                line,
            );
        }
    }

    /// Flush any remaining buffered content.
    pub fn finish(&mut self) {
        if !self.line_buffer.is_empty() {
            if self.in_code_block {
                Self::render_code_line(&self.line_buffer);
                println!();
            }
            // Prose partial was already printed char by char
            self.line_buffer.clear();
        }
        self.in_code_block = false;
    }
}

/// Print a cancel/interrupt message.
pub fn print_cancelled() {
    let mut stdout = std::io::stdout();
    // Ensure cursor is visible and line is clear
    let _ = stdout.execute(cursor::Show);
    let width = terminal::size().map(|(w, _)| w as usize).unwrap_or(120);
    print!("\r{}\r", " ".repeat(width));
    let _ = stdout.flush();
    println!(
        "\n{}{}  ^C \u{2014} cancelled{}",
        SetForegroundColor(Color::Yellow),
        SetAttribute(Attribute::Dim),
        SetAttribute(Attribute::Reset),
    );
}

// ── Progress indicators ─────────────────────────────────────────────

/// Print a continuation indicator when the agentic loop goes back for another round.
pub fn print_agentic_continue(step: u32) {
    println!(
        "\n{}{}  \u{21bb} continuing (step {step}){}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Dim),
        SetAttribute(Attribute::Reset),
    );
}

// ── Tool calls ──────────────────────────────────────────────────────

/// Get color for a tool type.
fn tool_color(name: &str) -> Color {
    if name.contains("__") {
        return Color::Magenta;
    }
    match name {
        "read_file" | "list_dir" | "tree" => Color::Cyan,
        "write_file" => Color::Green,
        "search" => Color::Magenta,
        "bash" => Color::Yellow,
        "kv_get" | "kv_set" => Color::Blue,
        _ => Color::Cyan,
    }
}

/// Print a tool execution header with progress indicator and color per tool type.
pub fn print_tool_call(tool: &ToolUseBlock, index: usize, total: usize) {
    let mut stdout = std::io::stdout();
    let color = tool_color(&tool.name);
    let name = tool_display_name(&tool.name);

    let progress = if total > 1 {
        format!("[{}/{}] ", index + 1, total)
    } else {
        String::new()
    };

    print!(
        "\n{}{}  \u{25CF} {progress}{name}",
        SetForegroundColor(color),
        SetAttribute(Attribute::Bold),
    );
    print!("{}", SetAttribute(Attribute::Reset));

    if let Some(obj) = tool.input.as_object() {
        let summary = tool_param_summary(&tool.name, obj);
        if !summary.is_empty() {
            print!(
                "{}  {summary}{}",
                SetForegroundColor(Color::DarkGrey),
                ResetColor,
            );
        }
    }

    println!();
    let _ = stdout.flush();
}

/// Print tool execution result with a color-coded left border.
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

/// Print tool completion status with duration.
pub fn print_tool_done(duration: std::time::Duration, is_error: bool) {
    let secs = duration.as_secs_f32();
    let (color, icon) = if is_error {
        (Color::Red, "\u{2717}")
    } else {
        (Color::Green, "\u{2713}")
    };
    println!(
        "{}  {icon} {secs:.1}s{}",
        SetForegroundColor(color),
        SetAttribute(Attribute::Reset),
    );
}

/// Start a spinner for tool execution with context from the tool input.
pub fn tool_spinner(tool_name: &str, input: &serde_json::Value) -> Spinner {
    if tool_name.contains("__") {
        let display = tool_display_name(tool_name);
        return Spinner::start(&format!("Calling {display}"));
    }

    let context = match tool_name {
        "read_file" => input
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| format!("Reading {p}")),
        "write_file" => input
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| format!("Writing {p}")),
        "list_dir" => input
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| format!("Listing {p}")),
        "search" => input
            .get("pattern")
            .and_then(|p| p.as_str())
            .map(|p| format!("Searching \"{p}\"")),
        "tree" => input
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| format!("Tree {p}")),
        "bash" => {
            let cmd = input
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("...");
            let short = if cmd.len() > 60 {
                format!("{}…", &cmd[..57])
            } else {
                cmd.to_string()
            };
            Some(format!("$ {short}"))
        }
        "kv_get" => input
            .get("key")
            .and_then(|k| k.as_str())
            .map(|k| format!("Reading {k}")),
        "kv_set" => input
            .get("key")
            .and_then(|k| k.as_str())
            .map(|k| format!("Setting {k}")),
        _ => None,
    };

    Spinner::start(&context.unwrap_or_else(|| "Executing".to_string()))
}

// ── Chrome ──────────────────────────────────────────────────────────

/// Print token usage summary with cost and session totals.
pub fn print_token_usage(
    input_tokens: u64,
    output_tokens: u64,
    cost_microcents: i64,
    session_tokens: u64,
    session_cost: i64,
) {
    let cost = format_cost(cost_microcents);
    let stotal = format_cost(session_cost);
    println!(
        "\n{}{}  \u{2500} {} in + {} out \u{2502} {} \u{2502} session: {} tok \u{2502} {}{}",
        SetForegroundColor(Color::DarkGrey),
        SetAttribute(Attribute::Dim),
        fmt_tokens(input_tokens),
        fmt_tokens(output_tokens),
        cost,
        fmt_tokens(session_tokens),
        stotal,
        SetAttribute(Attribute::Reset),
    );
}

fn format_cost(microcents: i64) -> String {
    let dollars = microcents as f64 / 1e8;
    format!("${:.4}", dollars)
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
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
    println!();
    let logo_lines = [
        r"  ██╗███╗   ██╗███████╗██╗███╗   ██╗██╗████████╗██╗   ██╗",
        r"  ██║████╗  ██║██╔════╝██║████╗  ██║██║╚══██╔══╝╚██╗ ██╔╝",
        r"  ██║██╔██╗ ██║█████╗  ██║██╔██╗ ██║██║   ██║    ╚████╔╝ ",
        r"  ██║██║╚██╗██║██╔══╝  ██║██║╚██╗██║██║   ██║     ╚██╔╝  ",
        r"  ██║██║ ╚████║██║     ██║██║ ╚████║██║   ██║      ██║   ",
        r"  ╚═╝╚═╝  ╚═══╝╚═╝     ╚═╝╚═╝  ╚═══╝╚═╝   ╚═╝      ╚═╝   ",
    ];
    for line in &logo_lines {
        println!(
            "{}{}{}{}",
            SetForegroundColor(Color::Magenta),
            SetAttribute(Attribute::Bold),
            line,
            SetAttribute(Attribute::Reset),
        );
    }
    println!(
        "  {}AI Coding Agent \u{2022} {} \u{2022} {}{}",
        SetForegroundColor(Color::DarkGrey),
        env!("CARGO_PKG_VERSION"),
        model,
        ResetColor,
    );
    println!();
    println!(
        "  {}db: {db_path}{}",
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
    println!(
        "  {}Type {}/help{} for commands \u{2022} {}Ctrl+C{} to cancel \u{2022} {}Ctrl+D{} to exit{}",
        SetForegroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White),
        SetForegroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White),
        SetForegroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White),
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
}

// ── MCP & Skills ─────────────────────────────────────────────────────

/// Print MCP server status at startup.
pub fn print_mcp_status(name: &str, tool_count: usize) {
    println!(
        "  {}mcp: {name} ({tool_count} tools){}",
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
}

/// Print MCP server error at startup.
pub fn print_mcp_error(name: &str, error: &str) {
    eprintln!(
        "  {}{}mcp: {name} — {error}{}",
        SetForegroundColor(Color::Red),
        SetAttribute(Attribute::Dim),
        SetAttribute(Attribute::Reset),
    );
}

/// Print the list of available skills.
pub fn print_skills_list(skills: &[(&str, &str)]) {
    if skills.is_empty() {
        println!("No skills loaded.");
        return;
    }
    println!(
        "{}{}Available skills:{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    for (name, desc) in skills {
        println!(
            "  {}  /{name}{}  — {desc}",
            SetForegroundColor(Color::Cyan),
            ResetColor,
        );
    }
}

/// Print configured MCP server list (for `mcp list` command).
pub fn print_mcp_server_list(servers: &[(String, crate::mcp_client::McpServerEntry)]) {
    if servers.is_empty() {
        println!("No MCP servers configured.");
        println!(
            "Add one with: infinity-agent mcp add <name> <command> [args...]"
        );
        return;
    }
    println!(
        "{}{}Configured MCP servers:{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    for (name, entry) in servers {
        let args = if entry.args.is_empty() {
            String::new()
        } else {
            format!(" {}", entry.args.join(" "))
        };
        println!(
            "  {}  {name}{} — {}{}{}",
            SetForegroundColor(Color::Cyan),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            format!("{}{args}", entry.command),
            ResetColor,
        );
    }
}

// ── Memory ──────────────────────────────────────────────────────────

/// Print memory system status at startup.
pub fn print_memory_status(provider_count: usize, reflection: bool) {
    let reflect_str = if reflection { " + reflection" } else { "" };
    println!(
        "  {}memory: {provider_count} providers{reflect_str}{}",
        SetForegroundColor(Color::DarkGrey),
        ResetColor,
    );
}

/// Print startup status block with green checkmarks (after banner).
pub fn print_startup_status(
    session_id: &str,
    msg_count: usize,
    is_resume: bool,
    memory_info: Option<&str>,
    tool_names: &[&str],
    mcp_info: &[(String, usize)],
) {
    println!();

    // Session line
    let session_short = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };
    if is_resume {
        println!(
            "  {}\u{2713}{} Session {}{}{} resumed ({msg_count} messages)",
            SetForegroundColor(Color::Green),
            ResetColor,
            SetForegroundColor(Color::Cyan),
            session_short,
            ResetColor,
        );
    } else {
        println!(
            "  {}\u{2713}{} Session {}{}{} started",
            SetForegroundColor(Color::Green),
            ResetColor,
            SetForegroundColor(Color::Cyan),
            session_short,
            ResetColor,
        );
    }

    // Memory line
    if let Some(info) = memory_info {
        println!(
            "  {}\u{2713}{} Memory: {}",
            SetForegroundColor(Color::Green),
            ResetColor,
            info,
        );
    }

    // Tools line
    if !tool_names.is_empty() {
        println!(
            "  {}\u{2713}{} Tools: {}",
            SetForegroundColor(Color::Green),
            ResetColor,
            tool_names.join(", "),
        );
    }

    // MCP line
    for (name, count) in mcp_info {
        println!(
            "  {}\u{2713}{} MCP: {name} ({count} tools)",
            SetForegroundColor(Color::Green),
            ResetColor,
        );
    }

    println!();
}

/// Print memory stats for the /memory command.
pub fn print_memory_stats(stats: &[(String, String)]) {
    if stats.is_empty() {
        println!("Memory system is not enabled.");
        return;
    }
    println!(
        "{}{}Memory stats:{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    for (name, info) in stats {
        println!(
            "  {}  {name}{}: {info}",
            SetForegroundColor(Color::Cyan),
            ResetColor,
        );
    }
}

/// Print playbook entries for `memory show`.
#[allow(dead_code)]
pub fn print_playbook_entries(entries: &[(String, String, i32, i32)]) {
    if entries.is_empty() {
        println!("No playbook entries yet.");
        return;
    }
    println!(
        "{}{}Playbook entries:{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    for (id, content, helpful, harmful) in entries {
        let score = helpful - harmful;
        let color = if score > 0 { Color::Green } else if score < 0 { Color::Red } else { Color::DarkGrey };
        println!(
            "  {}{id}{} [{}{score:+}{}] {content}",
            SetForegroundColor(Color::DarkGrey),
            ResetColor,
            SetForegroundColor(color),
            ResetColor,
        );
    }
}

// ── Search & Compaction ─────────────────────────────────────────────

/// Print BM25 search results.
pub fn print_search_results(query: &str, results: &[crate::memory::search::SearchResult]) {
    if results.is_empty() {
        println!("No results for: \"{query}\"");
        return;
    }
    println!(
        "{}{}Search results for \"{query}\":{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    println!();
    for (i, result) in results.iter().enumerate() {
        let score_color = if result.bm25_score > 1.0 { Color::Green } else { Color::DarkGrey };
        println!(
            "  {}{}. [{}]{} {}{:.2}{} {}",
            SetForegroundColor(Color::DarkGrey),
            i + 1,
            result.provider,
            ResetColor,
            SetForegroundColor(score_color),
            result.bm25_score,
            ResetColor,
            result.snippet,
        );
        println!(
            "     {}key: {}{}",
            SetForegroundColor(Color::DarkGrey),
            result.key,
            ResetColor,
        );
    }
}

/// Print compaction report.
pub fn print_compaction_report(report: &crate::memory::compaction::CompactionReport) {
    println!(
        "{}{}Compaction complete:{}",
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        SetAttribute(Attribute::Reset),
    );
    println!(
        "  {}  duplicates removed{}: {}",
        SetForegroundColor(Color::Cyan),
        ResetColor,
        report.duplicates_removed,
    );
    println!(
        "  {}  episodes compressed{}: {}",
        SetForegroundColor(Color::Cyan),
        ResetColor,
        report.episodes_compressed,
    );
    println!(
        "  {}  tier changes{}: {}",
        SetForegroundColor(Color::Cyan),
        ResetColor,
        report.tiers_rebalanced,
    );
}

// ── Helpers ─────────────────────────────────────────────────────────

fn tool_display_name(name: &str) -> String {
    // MCP tools: "server__tool" → "server:tool"
    if let Some(idx) = name.find("__") {
        let server = &name[..idx];
        let tool = &name[idx + 2..];
        return format!("{server}:{tool}");
    }

    match name {
        "read_file" => "Read".to_string(),
        "write_file" => "Write".to_string(),
        "list_dir" => "List".to_string(),
        "search" => "Search".to_string(),
        "tree" => "Tree".to_string(),
        "bash" => "Bash".to_string(),
        "kv_get" => "KV Get".to_string(),
        "kv_set" => "KV Set".to_string(),
        _ => name.to_string(),
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
