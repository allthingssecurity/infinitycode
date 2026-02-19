use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::error::{AgentError, Result};

// ── JSON-RPC types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

// ── Config types ─────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct McpServerEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// ── McpServer — a single connected MCP server ───────────────────────

pub struct McpServer {
    name: String,
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: AtomicU64,
    tools: Vec<Value>,
}

impl McpServer {
    /// Spawn a subprocess and perform MCP handshake (initialize + tools/list).
    pub async fn spawn(name: &str, config: &McpServerEntry) -> Result<Self> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            AgentError::Mcp(format!("Failed to spawn MCP server '{name}': {e}"))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            AgentError::Mcp(format!("No stdin for MCP server '{name}'"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AgentError::Mcp(format!("No stdout for MCP server '{name}'"))
        })?;

        let mut server = Self {
            name: name.to_string(),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            tools: Vec::new(),
        };

        // Initialize handshake
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "infinity-agent",
                "version": "0.1.0"
            }
        });

        let init_result = server.send_request("initialize", Some(init_params)).await?;
        if init_result.is_none() {
            return Err(AgentError::Mcp(format!(
                "MCP server '{name}' returned null for initialize"
            )));
        }

        // Send notifications/initialized (no id — it's a notification)
        server.send_notification("notifications/initialized", None).await?;

        // Discover tools
        let tools_result = server.send_request("tools/list", None).await?;
        if let Some(result) = tools_result {
            if let Some(tools_array) = result.get("tools").and_then(|t| t.as_array()) {
                server.tools = tools_array.clone();
            }
        }

        Ok(server)
    }

    /// Send a JSON-RPC request (with id) and wait for response.
    async fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Option<Value>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method: method.to_string(),
            params,
        };

        let mut line = serde_json::to_string(&request)
            .map_err(|e| AgentError::Mcp(format!("Failed to serialize request: {e}")))?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            AgentError::Mcp(format!("Failed to write to MCP server '{}': {e}", self.name))
        })?;
        self.stdin.flush().await.map_err(|e| {
            AgentError::Mcp(format!("Failed to flush MCP server '{}': {e}", self.name))
        })?;

        // Read response with timeout
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.read_response(),
        )
        .await
        .map_err(|_| {
            AgentError::Mcp(format!(
                "Timeout waiting for response from MCP server '{}'",
                self.name
            ))
        })??;

        if let Some(err) = response.error {
            return Err(AgentError::Mcp(format!(
                "MCP server '{}' error: {}",
                self.name, err.message
            )));
        }

        Ok(response.result)
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: None,
            method: method.to_string(),
            params,
        };

        let mut line = serde_json::to_string(&request)
            .map_err(|e| AgentError::Mcp(format!("Failed to serialize notification: {e}")))?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            AgentError::Mcp(format!("Failed to write to MCP server '{}': {e}", self.name))
        })?;
        self.stdin.flush().await.map_err(|e| {
            AgentError::Mcp(format!("Failed to flush MCP server '{}': {e}", self.name))
        })?;

        Ok(())
    }

    /// Read a single JSON-RPC response line from stdout.
    async fn read_response(&mut self) -> Result<JsonRpcResponse> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = self.stdout.read_line(&mut line).await.map_err(|e| {
                AgentError::Mcp(format!(
                    "Failed to read from MCP server '{}': {e}",
                    self.name
                ))
            })?;

            if bytes == 0 {
                return Err(AgentError::Mcp(format!(
                    "MCP server '{}' closed stdout unexpectedly",
                    self.name
                )));
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Skip notifications (no id field or id is null)
            if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                if val.get("id").is_none() || val.get("id") == Some(&Value::Null) {
                    // This is a server notification, skip it
                    continue;
                }
            }

            return serde_json::from_str(trimmed).map_err(|e| {
                AgentError::Mcp(format!(
                    "Invalid JSON from MCP server '{}': {e}\nLine: {trimmed}",
                    self.name
                ))
            });
        }
    }

    /// Call a tool on this MCP server, return text content.
    pub async fn call_tool(&mut self, tool_name: &str, arguments: &Value) -> Result<String> {
        let params = json!({
            "name": tool_name,
            "arguments": arguments,
        });

        let result = self.send_request("tools/call", Some(params)).await?;

        match result {
            Some(val) => {
                // Check for isError
                if val.get("isError") == Some(&Value::Bool(true)) {
                    let text = extract_text_content(&val);
                    return Err(AgentError::Mcp(format!(
                        "Tool '{}' error: {}",
                        tool_name,
                        if text.is_empty() { "unknown error" } else { &text }
                    )));
                }
                let text = extract_text_content(&val);
                Ok(text)
            }
            None => Ok("(no result)".to_string()),
        }
    }

    /// Convert MCP tool schemas to Anthropic tool definitions, prefixed with server name.
    pub fn tool_definitions_for_anthropic(&self) -> Vec<Value> {
        self.tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                let description = tool.get("description").and_then(|d| d.as_str()).unwrap_or("");

                // MCP uses camelCase "inputSchema", Anthropic wants "input_schema"
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

                Some(json!({
                    "name": format!("{}__{}", self.name, name),
                    "description": description,
                    "input_schema": input_schema,
                }))
            })
            .collect()
    }

    /// Number of tools this server exposes.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Shut down the server subprocess.
    pub async fn shutdown(&mut self) {
        // Best-effort: send shutdown notification
        let _ = self.send_notification("notifications/cancelled", None).await;
        let _ = self.child.kill().await;
    }
}

/// Extract text from MCP content array.
fn extract_text_content(val: &Value) -> String {
    if let Some(content) = val.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type")?.as_str()? == "text" {
                    item.get("text")?.as_str()
                } else {
                    None
                }
            })
            .collect();
        texts.join("\n")
    } else {
        String::new()
    }
}

// ── McpManager — manages all connected MCP servers ──────────────────

pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl McpManager {
    /// Load MCP config and spawn all servers. Warns on failure, continues.
    pub async fn from_config() -> Self {
        let mut servers = HashMap::new();
        let config = match load_mcp_config() {
            Some(c) => c,
            None => return Self { servers },
        };

        for (name, entry) in &config.mcp_servers {
            match McpServer::spawn(name, entry).await {
                Ok(server) => {
                    servers.insert(name.clone(), server);
                }
                Err(e) => {
                    crate::display::print_mcp_error(name, &e.to_string());
                }
            }
        }

        Self { servers }
    }

    /// Collect all tool definitions from all servers (already prefixed).
    pub fn all_tool_definitions(&self) -> Vec<Value> {
        let mut defs = Vec::new();
        for server in self.servers.values() {
            defs.extend(server.tool_definitions_for_anthropic());
        }
        defs
    }

    /// Route a prefixed tool call to the correct server.
    pub async fn call_tool(&mut self, prefixed_name: &str, input: &Value) -> Result<String> {
        let (server_name, tool_name) = split_mcp_tool_name(prefixed_name)
            .ok_or_else(|| AgentError::Mcp(format!("Invalid MCP tool name: {prefixed_name}")))?;

        let server = self.servers.get_mut(server_name).ok_or_else(|| {
            AgentError::Mcp(format!("MCP server not found: {server_name}"))
        })?;

        server.call_tool(tool_name, input).await
    }

    /// Check if a tool name is an MCP tool (contains `__`).
    pub fn is_mcp_tool(name: &str) -> bool {
        name.contains("__")
    }

    /// Get a list of (server_name, tool_count) for display.
    pub fn server_summary(&self) -> Vec<(&str, usize)> {
        self.servers
            .values()
            .map(|s| (s.name(), s.tool_count()))
            .collect()
    }

    /// Shut down all servers.
    pub async fn shutdown(&mut self) {
        for server in self.servers.values_mut() {
            server.shutdown().await;
        }
    }
}

/// Split "servername__toolname" into ("servername", "toolname").
fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let idx = name.find("__")?;
    Some((&name[..idx], &name[idx + 2..]))
}

// ── Config helpers ───────────────────────────────────────────────────

/// Load MCP config from `~/.infinity/mcp.json` and `.infinity/mcp.json` (project-local).
/// Project-local entries override global ones.
fn load_mcp_config() -> Option<McpConfigFile> {
    let mut merged = HashMap::new();

    // Global config
    if let Some(home) = dirs::home_dir() {
        let global_path = home.join(".infinity").join("mcp.json");
        if let Ok(content) = std::fs::read_to_string(&global_path) {
            if let Ok(config) = serde_json::from_str::<McpConfigFile>(&content) {
                merged.extend(config.mcp_servers);
            }
        }
    }

    // Project-local config (overrides global)
    let local_path = PathBuf::from(".infinity").join("mcp.json");
    if let Ok(content) = std::fs::read_to_string(&local_path) {
        if let Ok(config) = serde_json::from_str::<McpConfigFile>(&content) {
            merged.extend(config.mcp_servers);
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(McpConfigFile {
            mcp_servers: merged,
        })
    }
}

/// Get path to global MCP config file.
fn global_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".infinity").join("mcp.json"))
}

/// Read or create the global MCP config file.
fn read_or_create_global_config() -> Result<(PathBuf, McpConfigFile)> {
    let path = global_config_path()
        .ok_or_else(|| AgentError::Config("Cannot determine home directory".to_string()))?;

    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let config: McpConfigFile = serde_json::from_str(&content)
            .map_err(|e| AgentError::Config(format!("Invalid mcp.json: {e}")))?;
        Ok((path, config))
    } else {
        Ok((
            path,
            McpConfigFile {
                mcp_servers: HashMap::new(),
            },
        ))
    }
}

/// Add an MCP server entry to the global config.
pub fn add_server_to_config(
    name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<()> {
    let (path, mut config) = read_or_create_global_config()?;

    config.mcp_servers.insert(
        name.to_string(),
        McpServerEntry {
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
        },
    );

    save_config(&path, &config)
}

/// Remove an MCP server entry from the global config.
pub fn remove_server_from_config(name: &str) -> Result<bool> {
    let (path, mut config) = read_or_create_global_config()?;
    let removed = config.mcp_servers.remove(name).is_some();
    if removed {
        save_config(&path, &config)?;
    }
    Ok(removed)
}

/// List all configured servers (from global config).
pub fn list_configured_servers() -> Result<Vec<(String, McpServerEntry)>> {
    let (_path, config) = read_or_create_global_config()?;
    Ok(config.mcp_servers.into_iter().collect())
}

fn save_config(path: &PathBuf, config: &McpConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(path, json)?;
    Ok(())
}
