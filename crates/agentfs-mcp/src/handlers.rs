use agentfs_core::analytics::TokenRecord;
use agentfs_core::AgentFS;
use serde_json::{json, Value};

/// Extract a required string parameter.
fn get_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required parameter: {key}"))
}

/// Extract an optional string parameter.
fn get_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Extract an optional integer parameter.
fn get_opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

// ── Filesystem handlers ────────────────────────────────────────────

pub async fn handle_read_file(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    let data = db.fs.read_file(&path).await.map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&data);
    Ok(json!({ "content": text }))
}

pub async fn handle_write_file(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    let content = get_str(args, "content")?;
    db.fs
        .write_file(&path, content.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "written": content.len(), "path": path }))
}

pub async fn handle_append_file(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    let content = get_str(args, "content")?;
    db.fs
        .append_file(&path, content.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "appended": content.len(), "path": path }))
}

pub async fn handle_delete_file(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    db.fs.remove_file(&path).await.map_err(|e| e.to_string())?;
    Ok(json!({ "deleted": path }))
}

pub async fn handle_list_dir(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_opt_str(args, "path").unwrap_or_else(|| "/".to_string());
    let entries = db.fs.readdir(&path).await.map_err(|e| e.to_string())?;
    let items: Vec<Value> = entries
        .iter()
        .map(|e| {
            let ftype = if (e.mode & 0o170000) == 0o040000 {
                "dir"
            } else {
                "file"
            };
            json!({ "name": e.name, "ino": e.ino, "type": ftype })
        })
        .collect();
    Ok(json!({ "entries": items }))
}

pub async fn handle_mkdir(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    db.fs.mkdir(&path).await.map_err(|e| e.to_string())?;
    Ok(json!({ "created": path }))
}

pub async fn handle_stat(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    let st = db.fs.stat(&path).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&st).unwrap())
}

pub async fn handle_tree(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_opt_str(args, "path").unwrap_or_else(|| "/".to_string());
    let tree = db.fs.tree(&path).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&tree).unwrap())
}

pub async fn handle_rename(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let from = get_str(args, "from")?;
    let to = get_str(args, "to")?;
    db.fs.rename(&from, &to).await.map_err(|e| e.to_string())?;
    Ok(json!({ "renamed": { "from": from, "to": to } }))
}

pub async fn handle_remove_tree(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let path = get_str(args, "path")?;
    db.fs.remove_tree(&path).await.map_err(|e| e.to_string())?;
    Ok(json!({ "removed": path }))
}

pub async fn handle_search(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let pattern = get_str(args, "pattern")?;
    let results = db.fs.search(&pattern).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&results).unwrap())
}

// ── Key-Value handlers ─────────────────────────────────────────────

pub async fn handle_kv_get(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let key = get_str(args, "key")?;
    let entry = db.kv.get(&key).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&entry).unwrap())
}

pub async fn handle_kv_set(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let key = get_str(args, "key")?;
    let value = get_str(args, "value")?;
    db.kv.set(&key, &value).await.map_err(|e| e.to_string())?;
    Ok(json!({ "set": key }))
}

pub async fn handle_kv_delete(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let key = get_str(args, "key")?;
    db.kv.delete(&key).await.map_err(|e| e.to_string())?;
    Ok(json!({ "deleted": key }))
}

pub async fn handle_kv_list(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let prefix = get_opt_str(args, "prefix").unwrap_or_default();
    let entries = db.kv.list_prefix(&prefix).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&entries).unwrap())
}

// ── Platform handlers ──────────────────────────────────────────────

pub async fn handle_info(db: &AgentFS, _args: &Value) -> Result<Value, String> {
    let info = db.info().await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&info).unwrap())
}

pub async fn handle_record_usage(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let record = TokenRecord {
        id: None,
        session_id: get_opt_str(args, "session_id"),
        tool_call_id: get_opt_i64(args, "tool_call_id"),
        model: get_str(args, "model")?,
        input_tokens: args.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        output_tokens: args.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        cache_read_tokens: args.get("cache_read_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        cache_write_tokens: args.get("cache_write_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        cost_microcents: args.get("cost_microcents").and_then(|v| v.as_i64()).unwrap_or(0),
        recorded_at: None,
    };
    let id = db.analytics.record_usage(record).await.map_err(|e| e.to_string())?;
    Ok(json!({ "recorded_id": id }))
}

pub async fn handle_session_start(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let session_id = get_str(args, "session_id")?;
    let agent_name = get_opt_str(args, "agent_name");
    let provider = get_opt_str(args, "provider");
    let metadata = get_opt_str(args, "metadata");
    let session = db
        .sessions
        .start(
            &session_id,
            agent_name.as_deref(),
            provider.as_deref(),
            metadata.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&session).unwrap())
}

pub async fn handle_session_end(db: &AgentFS, args: &Value) -> Result<Value, String> {
    let session_id = get_str(args, "session_id")?;
    let status = get_opt_str(args, "status").unwrap_or_else(|| "completed".to_string());
    db.sessions
        .end(&session_id, &status)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ended": session_id, "status": status }))
}

/// Dispatch a tool call to the appropriate handler.
pub async fn dispatch(tool_name: &str, db: &AgentFS, args: &Value) -> Result<Value, String> {
    match tool_name {
        "agentfs_read_file" => handle_read_file(db, args).await,
        "agentfs_write_file" => handle_write_file(db, args).await,
        "agentfs_append_file" => handle_append_file(db, args).await,
        "agentfs_delete_file" => handle_delete_file(db, args).await,
        "agentfs_list_dir" => handle_list_dir(db, args).await,
        "agentfs_mkdir" => handle_mkdir(db, args).await,
        "agentfs_stat" => handle_stat(db, args).await,
        "agentfs_tree" => handle_tree(db, args).await,
        "agentfs_rename" => handle_rename(db, args).await,
        "agentfs_remove_tree" => handle_remove_tree(db, args).await,
        "agentfs_search" => handle_search(db, args).await,
        "agentfs_kv_get" => handle_kv_get(db, args).await,
        "agentfs_kv_set" => handle_kv_set(db, args).await,
        "agentfs_kv_delete" => handle_kv_delete(db, args).await,
        "agentfs_kv_list" => handle_kv_list(db, args).await,
        "agentfs_info" => handle_info(db, args).await,
        "agentfs_record_usage" => handle_record_usage(db, args).await,
        "agentfs_session_start" => handle_session_start(db, args).await,
        "agentfs_session_end" => handle_session_end(db, args).await,
        _ => Err(format!("unknown tool: {tool_name}")),
    }
}
