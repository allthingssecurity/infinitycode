use std::sync::Arc;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

use agentfs_core::AgentFS;

use crate::error::{AgentError, Result};
use crate::mcp_client::McpManager;

/// Executes tool calls against AgentFS and the host shell.
pub struct ToolExecutor {
    pub db: AgentFS,
    pub session_id: String,
    pub mcp: Option<Arc<Mutex<McpManager>>>,
}

impl ToolExecutor {
    pub fn new(db: AgentFS, session_id: String) -> Self {
        Self {
            db,
            session_id,
            mcp: None,
        }
    }

    pub fn with_mcp(mut self, mcp: Arc<Mutex<McpManager>>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Execute a tool call and return the result as a string.
    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<String> {
        // Log tool start
        let tc_id = self
            .db
            .tools
            .start(tool_name, Some(&input.to_string()))
            .await
            .ok();

        let result = if McpManager::is_mcp_tool(tool_name) {
            // Route to MCP server
            match &self.mcp {
                Some(mcp) => {
                    let mut manager = mcp.lock().await;
                    manager.call_tool(tool_name, input).await
                }
                None => Err(AgentError::Mcp(format!(
                    "MCP tool '{tool_name}' called but no MCP manager available"
                ))),
            }
        } else {
            match tool_name {
                "read_file" => self.exec_read_file(input).await,
                "write_file" => self.exec_write_file(input).await,
                "list_dir" => self.exec_list_dir(input).await,
                "search" => self.exec_search(input).await,
                "tree" => self.exec_tree(input).await,
                "bash" => self.exec_bash(input).await,
                "kv_get" => self.exec_kv_get(input).await,
                "kv_set" => self.exec_kv_set(input).await,
                _ => Err(AgentError::Tool(format!("Unknown tool: {tool_name}"))),
            }
        };

        // Log result
        match &result {
            Ok(output) => {
                if let Some(id) = tc_id {
                    let truncated = if output.len() > 1000 {
                        format!("{}...", &output[..1000])
                    } else {
                        output.clone()
                    };
                    let _ = self.db.tools.success(id, Some(&truncated)).await;
                }
                let _ = self
                    .db
                    .events
                    .log(
                        Some(&self.session_id),
                        &format!("tool:{tool_name}"),
                        input.get("path").and_then(|p| p.as_str()),
                        None,
                    )
                    .await;
            }
            Err(e) => {
                if let Some(id) = tc_id {
                    let _ = self.db.tools.error(id, &e.to_string()).await;
                }
                let _ = self
                    .db
                    .events
                    .log(
                        Some(&self.session_id),
                        &format!("tool_error:{tool_name}"),
                        None,
                        Some(&e.to_string()),
                    )
                    .await;
            }
        }

        result
    }

    async fn exec_read_file(&self, input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| AgentError::Tool("read_file: missing 'path' parameter".to_string()))?;

        let data = self.db.fs.read_file(path).await?;
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    async fn exec_write_file(&self, input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| AgentError::Tool("write_file: missing 'path' parameter".to_string()))?;
        let content = input
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                AgentError::Tool("write_file: missing 'content' parameter".to_string())
            })?;

        self.db.fs.write_file(path, content.as_bytes()).await?;
        Ok(format!("Written {} bytes to {path}", content.len()))
    }

    async fn exec_list_dir(&self, input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("/");

        let entries = self.db.fs.readdir(path).await?;
        let mut output = String::new();
        for entry in &entries {
            let kind = if (entry.mode & 0o170000) == 0o040000 {
                "dir"
            } else {
                "file"
            };
            output.push_str(&format!("[{kind}] {}\n", entry.name));
        }
        if output.is_empty() {
            output = "(empty directory)\n".to_string();
        }
        Ok(output)
    }

    async fn exec_search(&self, input: &Value) -> Result<String> {
        let pattern = input
            .get("pattern")
            .and_then(|p| p.as_str())
            .ok_or_else(|| AgentError::Tool("search: missing 'pattern' parameter".to_string()))?;

        let results = self.db.fs.search(pattern).await?;
        let mut output = String::new();
        for result in &results {
            let kind = if result.is_dir { "dir" } else { "file" };
            output.push_str(&format!("[{kind}] {} ({} bytes)\n", result.path, result.size));
        }
        if output.is_empty() {
            output = "(no matches)\n".to_string();
        }
        Ok(output)
    }

    async fn exec_tree(&self, input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("/");

        let tree = self.db.fs.tree(path).await?;
        let mut output = String::new();
        render_tree_node(&tree, "", true, &mut output);
        Ok(output)
    }

    async fn exec_bash(&self, input: &Value) -> Result<String> {
        let command = input
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| AgentError::Tool("bash: missing 'command' parameter".to_string()))?;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("sh").arg("-c").arg(command).output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("[stderr]\n");
                    result.push_str(&stderr);
                }
                if exit_code != 0 {
                    result.push_str(&format!("\n[exit code: {exit_code}]"));
                }
                if result.is_empty() {
                    result = "(no output)".to_string();
                }
                Ok(result)
            }
            Ok(Err(e)) => Err(AgentError::Tool(format!("bash: failed to execute: {e}"))),
            Err(_) => Err(AgentError::Tool(
                "bash: command timed out after 30 seconds".to_string(),
            )),
        }
    }

    async fn exec_kv_get(&self, input: &Value) -> Result<String> {
        let key = input
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| AgentError::Tool("kv_get: missing 'key' parameter".to_string()))?;

        match self.db.kv.get(key).await {
            Ok(entry) => Ok(entry.value),
            Err(agentfs_core::error::AgentFSError::KeyNotFound { key }) => {
                Ok(format!("(key not found: {key})"))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn exec_kv_set(&self, input: &Value) -> Result<String> {
        let key = input
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| AgentError::Tool("kv_set: missing 'key' parameter".to_string()))?;
        let value = input
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::Tool("kv_set: missing 'value' parameter".to_string()))?;

        self.db.kv.set(key, value).await?;
        Ok(format!("Set key '{key}'"))
    }
}

/// Render a tree node with indentation.
fn render_tree_node(
    node: &agentfs_core::filesystem::TreeNode,
    prefix: &str,
    is_last: bool,
    output: &mut String,
) {
    let connector = if prefix.is_empty() {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let kind = if node.stat.is_dir() { "/" } else { "" };
    output.push_str(&format!("{prefix}{connector}{}{kind}\n", node.name));

    let child_prefix = if prefix.is_empty() {
        String::new()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    let len = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        render_tree_node(child, &child_prefix, i == len - 1, output);
    }
}
