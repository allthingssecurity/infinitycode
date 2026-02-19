use serde_json::{json, Value};

/// Merge built-in tools with extra tool definitions (e.g. from MCP servers).
pub fn merge_tools(builtin: Vec<Value>, extra: Vec<Value>) -> Vec<Value> {
    let mut all = builtin;
    all.extend(extra);
    all
}

/// Return all tool definitions for Claude.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read a file from the agent workspace filesystem. Returns the file contents as a string.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file (e.g., /src/main.rs)"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Write or create a file in the agent workspace filesystem. Creates parent directories automatically.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file (e.g., /src/main.rs)"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "list_dir",
            "description": "List the contents of a directory in the agent workspace filesystem.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the directory (default: /)",
                        "default": "/"
                    }
                },
                "required": []
            }
        }),
        json!({
            "name": "search",
            "description": "Search for files by name pattern in the agent workspace. Supports * (any chars) and ? (single char) wildcards.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match file names (e.g., *.rs, test_*)"
                    }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "tree",
            "description": "Show a recursive directory tree of the agent workspace filesystem.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Root path for the tree (default: /)",
                        "default": "/"
                    }
                },
                "required": []
            }
        }),
        json!({
            "name": "bash",
            "description": "Execute a shell command on the host system. Commands run with a 30-second timeout. Use for running code, tests, git operations, or any system command.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "kv_get",
            "description": "Read a value from the persistent key-value store.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key to look up"
                    }
                },
                "required": ["key"]
            }
        }),
        json!({
            "name": "kv_set",
            "description": "Write a value to the persistent key-value store. Creates or updates the key.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key to set"
                    },
                    "value": {
                        "type": "string",
                        "description": "The value to store"
                    }
                },
                "required": ["key", "value"]
            }
        }),
    ]
}
