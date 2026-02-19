mod db_manager;
mod handlers;
mod protocol;
mod tools;

use std::io::BufRead;

use serde_json::{json, Value};
use tracing::debug;

use db_manager::DbManager;
use protocol::{JsonRpcRequest, JsonRpcResponse, INTERNAL_ERROR, METHOD_NOT_FOUND, PARSE_ERROR};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentfs-mcp";
const SERVER_VERSION: &str = "0.1.0";

fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }),
    )
}

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(id, json!({ "tools": tools::tool_definitions() }))
}

async fn handle_tools_call(
    id: Option<Value>,
    params: &Value,
    db_manager: &mut DbManager,
) -> JsonRpcResponse {
    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(name) => name.to_string(),
        None => {
            return JsonRpcResponse::error(id, INTERNAL_ERROR, "missing tool name");
        }
    };

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Special case: agentfs_init creates a new database
    if tool_name == "agentfs_init" {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return tool_result(id, Err("missing required parameter: path".to_string()));
            }
        };
        match db_manager.create(&path).await {
            Ok(_) => {
                return tool_result(id, Ok(json!({ "created": path })));
            }
            Err(e) => {
                return tool_result(id, Err(e));
            }
        }
    }

    // All other tools need a 'db' parameter
    let db_path = match args.get("db").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            return tool_result(id, Err("missing required parameter: db".to_string()));
        }
    };

    let db = match db_manager.get_or_open(&db_path).await {
        Ok(db) => db,
        Err(e) => {
            return tool_result(id, Err(e));
        }
    };

    let result = handlers::dispatch(&tool_name, db, &args).await;
    tool_result(id, result)
}

/// Wrap a tool result in an MCP-style response (content array, isError flag).
fn tool_result(id: Option<Value>, result: Result<Value, String>) -> JsonRpcResponse {
    match result {
        Ok(value) => {
            let text = if value.is_string() {
                value.as_str().unwrap().to_string()
            } else {
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            };
            JsonRpcResponse::success(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }),
            )
        }
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{ "type": "text", "text": e }],
                "isError": true
            }),
        ),
    }
}

#[tokio::main]
async fn main() {
    // Tracing to stderr only — stdout is the protocol channel
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut db_manager = DbManager::new();

    let stdin = std::io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, PARSE_ERROR, format!("parse error: {e}"));
                send_response(&resp);
                continue;
            }
        };

        debug!(method = %request.method, "received request");

        // Notifications (no id) — silently acknowledge
        if request.id.is_none() {
            continue;
        }

        let response = match request.method.as_str() {
            "initialize" => handle_initialize(request.id),
            "tools/list" => handle_tools_list(request.id),
            "tools/call" => handle_tools_call(request.id, &request.params, &mut db_manager).await,
            _ => JsonRpcResponse::error(
                request.id,
                METHOD_NOT_FOUND,
                format!("method not found: {}", request.method),
            ),
        };

        send_response(&response);
    }

    // Graceful shutdown
    db_manager.close_all().await;
}

fn send_response(response: &JsonRpcResponse) {
    if let Ok(json) = serde_json::to_string(response) {
        println!("{json}");
    }
}
