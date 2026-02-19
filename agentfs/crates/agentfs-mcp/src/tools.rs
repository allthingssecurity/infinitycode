use serde_json::{json, Value};

/// Return the list of all tool definitions for tools/list.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool("agentfs_init", "Create a new AgentFS database. Creates the SQLite file and initializes the schema.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the new database file" }
            },
            "required": ["path"]
        })),
        tool("agentfs_read_file", "Read the contents of a file. Returns the file content as UTF-8 text.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "File path within the filesystem (e.g., /docs/readme.md)" }
            },
            "required": ["db", "path"]
        })),
        tool("agentfs_write_file", "Write data to a file. Creates parent directories automatically. Overwrites if file exists.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "File path within the filesystem" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["db", "path", "content"]
        })),
        tool("agentfs_append_file", "Append data to a file. Creates the file if it doesn't exist.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "File path within the filesystem" },
                "content": { "type": "string", "description": "Content to append" }
            },
            "required": ["db", "path", "content"]
        })),
        tool("agentfs_delete_file", "Delete a file.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "File path to delete" }
            },
            "required": ["db", "path"]
        })),
        tool("agentfs_list_dir", "List directory contents. Returns entries with name, inode, and type.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "Directory path (default: /)", "default": "/" }
            },
            "required": ["db"]
        })),
        tool("agentfs_mkdir", "Create a directory. Creates intermediate parent directories as needed.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "Directory path to create" }
            },
            "required": ["db", "path"]
        })),
        tool("agentfs_stat", "Get metadata for a file or directory (inode, mode, size, timestamps).", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "Path to stat" }
            },
            "required": ["db", "path"]
        })),
        tool("agentfs_tree", "Get a recursive tree listing of the filesystem.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "Root path for the tree (default: /)", "default": "/" }
            },
            "required": ["db"]
        })),
        tool("agentfs_rename", "Rename or move a file/directory. Overwrites destination if it exists (POSIX semantics).", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "from": { "type": "string", "description": "Source path" },
                "to": { "type": "string", "description": "Destination path" }
            },
            "required": ["db", "from", "to"]
        })),
        tool("agentfs_remove_tree", "Recursively remove a directory and all contents.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "path": { "type": "string", "description": "Directory path to remove" }
            },
            "required": ["db", "path"]
        })),
        tool("agentfs_search", "Search for files/directories matching a glob pattern (* and ? supported).", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "pattern": { "type": "string", "description": "Glob pattern to match (e.g., *.rs, config*)" }
            },
            "required": ["db", "pattern"]
        })),
        tool("agentfs_kv_get", "Get a value from the key-value store.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "key": { "type": "string", "description": "Key to retrieve" }
            },
            "required": ["db", "key"]
        })),
        tool("agentfs_kv_set", "Set a key-value pair. Creates or updates.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "key": { "type": "string", "description": "Key to set" },
                "value": { "type": "string", "description": "Value to store" }
            },
            "required": ["db", "key", "value"]
        })),
        tool("agentfs_kv_delete", "Delete a key from the key-value store.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "key": { "type": "string", "description": "Key to delete" }
            },
            "required": ["db", "key"]
        })),
        tool("agentfs_kv_list", "List key-value pairs with an optional prefix filter.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "prefix": { "type": "string", "description": "Optional key prefix to filter by", "default": "" }
            },
            "required": ["db"]
        })),
        tool("agentfs_info", "Get database stats: schema version, file counts, sizes, token usage, session counts.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" }
            },
            "required": ["db"]
        })),
        tool("agentfs_record_usage", "Record token usage for analytics. Track costs across models and sessions.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "model": { "type": "string", "description": "Model name (e.g., claude-opus-4-6)" },
                "input_tokens": { "type": "integer", "description": "Number of input tokens" },
                "output_tokens": { "type": "integer", "description": "Number of output tokens" },
                "session_id": { "type": "string", "description": "Optional session ID" },
                "cache_read_tokens": { "type": "integer", "description": "Cache read tokens (default: 0)", "default": 0 },
                "cache_write_tokens": { "type": "integer", "description": "Cache write tokens (default: 0)", "default": 0 },
                "cost_microcents": { "type": "integer", "description": "Cost in microcents (default: 0)", "default": 0 }
            },
            "required": ["db", "model", "input_tokens", "output_tokens"]
        })),
        tool("agentfs_session_start", "Start a new agent session for tracking.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "session_id": { "type": "string", "description": "Unique session identifier" },
                "agent_name": { "type": "string", "description": "Name of the agent (e.g., planner, coder)" },
                "provider": { "type": "string", "description": "Provider name (e.g., anthropic, openai)" },
                "metadata": { "type": "string", "description": "Optional JSON metadata" }
            },
            "required": ["db", "session_id"]
        })),
        tool("agentfs_session_end", "End an agent session.", json!({
            "type": "object",
            "properties": {
                "db": { "type": "string", "description": "Path to the database file" },
                "session_id": { "type": "string", "description": "Session ID to end" },
                "status": { "type": "string", "description": "Final status: completed or failed", "default": "completed" }
            },
            "required": ["db", "session_id"]
        })),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}
