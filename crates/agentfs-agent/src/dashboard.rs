use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use agentfs_core::AgentFS;

use crate::memory::MemoryManager;

// ── State ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    db: Arc<AgentFS>,
    memory: Arc<MemoryManager>,
}

// ── Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct TokensResponse {
    summary: agentfs_core::analytics::UsageSummary,
    by_model: Vec<agentfs_core::analytics::ModelBreakdown>,
}

#[derive(Serialize)]
struct EventsResponse {
    recent: Vec<agentfs_core::events::Event>,
    by_type: Vec<(String, i64)>,
}

#[derive(Serialize)]
struct MemoryResponse {
    tiers: TierCounts,
    pressure: crate::memory::tiers::MemoryPressure,
    providers: Vec<ProviderStats>,
}

#[derive(Serialize)]
struct TierCounts {
    hot: usize,
    warm: usize,
    cold: usize,
}

#[derive(Serialize)]
struct ProviderStats {
    name: String,
    count: usize,
}

#[derive(serde::Deserialize)]
struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
}

// ── Session detail response types ──────────────────────────────────

#[derive(Serialize)]
struct SessionToolCall {
    id: i64,
    tool_name: String,
    status: String,
    input: Option<String>,
    output: Option<String>,
    error_msg: Option<String>,
    started_at: String,
    ended_at: Option<String>,
}

#[derive(Serialize)]
struct SessionTokenRecord {
    id: i64,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost_microcents: i64,
    recorded_at: String,
}

// ── Memory browse types (parsed from KV JSON) ─────────────────────

#[derive(Serialize, Deserialize)]
struct DashPlaybookEntry {
    id: String,
    category: String,
    content: String,
    helpful: i32,
    harmful: i32,
    source_session: String,
    created: String,
    updated: String,
}

#[derive(Serialize, Deserialize)]
struct DashEpisode {
    session_id: String,
    summary: String,
    key_decisions: Vec<String>,
    tools_used: Vec<String>,
    outcome: String,
    created: String,
}

#[derive(Serialize, Deserialize)]
struct DashToolPattern {
    tool: String,
    patterns: Vec<DashToolPatternEntry>,
    common_errors: Vec<DashCommonError>,
}

#[derive(Serialize, Deserialize)]
struct DashToolPatternEntry {
    pattern: String,
    helpful: i32,
}

#[derive(Serialize, Deserialize)]
struct DashCommonError {
    error: String,
    frequency: i32,
}

// ── Handlers ────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}

async fn api_info(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.info().await {
        Ok(info) => Json(serde_json::to_value(info).unwrap()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_sessions(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.sessions.list_recent(50).await {
        Ok(sessions) => Json(sessions).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_tokens(State(state): State<AppState>) -> impl IntoResponse {
    let summary = state.db.analytics.summary().await;
    let by_model = state.db.analytics.by_model().await;

    match (summary, by_model) {
        (Ok(s), Ok(m)) => Json(TokensResponse {
            summary: s,
            by_model: m,
        })
        .into_response(),
        (Err(e), _) | (_, Err(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn api_tools(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.tools.stats().await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_events(State(state): State<AppState>) -> impl IntoResponse {
    let recent = state.db.events.recent(100).await;
    let by_type = state.db.events.count_by_type().await;

    match (recent, by_type) {
        (Ok(r), Ok(bt)) => Json(EventsResponse {
            recent: r,
            by_type: bt,
        })
        .into_response(),
        (Err(e), _) | (_, Err(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn api_memory(State(state): State<AppState>) -> impl IntoResponse {
    let tiers = state.memory.tier_counts().await.unwrap_or((0, 0, 0));
    let pressure = state
        .memory
        .memory_pressure()
        .await
        .unwrap_or(crate::memory::tiers::MemoryPressure::Low);

    // Get provider entry counts from KV prefixes
    let playbook_count = state
        .db
        .kv
        .list_prefix("memory:playbook:")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    let episode_count = state
        .db
        .kv
        .list_prefix("memory:episode:")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    let tool_count = state
        .db
        .kv
        .list_prefix("memory:tool_pattern:")
        .await
        .map(|v| v.len())
        .unwrap_or(0);

    Json(MemoryResponse {
        tiers: TierCounts {
            hot: tiers.0,
            warm: tiers.1,
            cold: tiers.2,
        },
        pressure,
        providers: vec![
            ProviderStats {
                name: "playbook".into(),
                count: playbook_count,
            },
            ProviderStats {
                name: "episodes".into(),
                count: episode_count,
            },
            ProviderStats {
                name: "tool_patterns".into(),
                count: tool_count,
            },
        ],
    })
}

async fn api_memory_search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return Json(Vec::<crate::memory::search::SearchResult>::new()).into_response();
    }
    let limit = params.limit.unwrap_or(10).min(50);

    match state.memory.search(&query, limit).await {
        Ok(results) => Json(results).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// ── Session deep-dive handlers ──────────────────────────────────────

async fn api_session_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.db.sessions.get(&id).await {
        Ok(session) => Json(session).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn api_session_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.db.events.by_session(&id, 500).await {
        Ok(events) => Json(events).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_session_tokens(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let reader = match state.db.readers().acquire().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let result = (|| -> std::result::Result<Vec<SessionTokenRecord>, rusqlite::Error> {
        let mut stmt = reader.conn().prepare(
            "SELECT id, model, input_tokens, output_tokens, cache_read_tokens, \
             cache_write_tokens, cost_microcents, recorded_at \
             FROM token_usage WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([&id], |row| {
            Ok(SessionTokenRecord {
                id: row.get(0)?,
                model: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                cache_read_tokens: row.get(4)?,
                cache_write_tokens: row.get(5)?,
                cost_microcents: row.get(6)?,
                recorded_at: row.get(7)?,
            })
        })?
        .collect();
        rows
    })();
    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_session_tools_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let reader = match state.db.readers().acquire().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let result = (|| -> std::result::Result<Vec<SessionToolCall>, rusqlite::Error> {
        let mut stmt = reader.conn().prepare(
            "SELECT id, tool_name, status, input, output, error_msg, started_at, ended_at \
             FROM tool_calls WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([&id], |row| {
            Ok(SessionToolCall {
                id: row.get(0)?,
                tool_name: row.get(1)?,
                status: row.get(2)?,
                input: row.get(3)?,
                output: row.get(4)?,
                error_msg: row.get(5)?,
                started_at: row.get(6)?,
                ended_at: row.get(7)?,
            })
        })?
        .collect();
        rows
    })();
    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_session_learnings(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let entries = match state.db.kv.list_prefix("memory:playbook:").await {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut learnings: Vec<DashPlaybookEntry> = entries
        .into_iter()
        .filter_map(|kv| serde_json::from_str::<DashPlaybookEntry>(&kv.value).ok())
        .filter(|entry| entry.source_session == id)
        .collect();
    learnings.sort_by(|a, b| (b.helpful - b.harmful).cmp(&(a.helpful - a.harmful)));
    Json(learnings).into_response()
}

// ── Agent brain handlers ────────────────────────────────────────────

async fn api_memory_playbook(State(state): State<AppState>) -> impl IntoResponse {
    let entries = match state.db.kv.list_prefix("memory:playbook:").await {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut playbook: Vec<DashPlaybookEntry> = entries
        .into_iter()
        .filter_map(|kv| serde_json::from_str::<DashPlaybookEntry>(&kv.value).ok())
        .collect();
    playbook.sort_by(|a, b| (b.helpful - b.harmful).cmp(&(a.helpful - a.harmful)));
    Json(playbook).into_response()
}

async fn api_memory_episodes(State(state): State<AppState>) -> impl IntoResponse {
    let entries = match state.db.kv.list_prefix("memory:episode:").await {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut episodes: Vec<DashEpisode> = entries
        .into_iter()
        .filter_map(|kv| serde_json::from_str::<DashEpisode>(&kv.value).ok())
        .collect();
    episodes.sort_by(|a, b| b.created.cmp(&a.created));
    Json(episodes).into_response()
}

async fn api_memory_tool_patterns(State(state): State<AppState>) -> impl IntoResponse {
    let entries = match state.db.kv.list_prefix("memory:tool_pattern:").await {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let patterns: Vec<DashToolPattern> = entries
        .into_iter()
        .filter_map(|kv| serde_json::from_str::<DashToolPattern>(&kv.value).ok())
        .collect();
    Json(patterns).into_response()
}

async fn api_sessions_costs(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.analytics.by_session().await {
        Ok(costs) => Json(costs).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Server ──────────────────────────────────────────────────────────

pub async fn run_dashboard(
    db: Arc<AgentFS>,
    memory: Arc<MemoryManager>,
    port: u16,
) -> anyhow::Result<()> {
    let state = AppState { db, memory };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/info", get(api_info))
        .route("/api/sessions", get(api_sessions))
        .route("/api/tokens", get(api_tokens))
        .route("/api/tools", get(api_tools))
        .route("/api/events", get(api_events))
        .route("/api/memory", get(api_memory))
        .route("/api/memory/search", get(api_memory_search))
        .route("/api/memory/playbook", get(api_memory_playbook))
        .route("/api/memory/episodes", get(api_memory_episodes))
        .route("/api/memory/tool-patterns", get(api_memory_tool_patterns))
        .route("/api/sessions/costs", get(api_sessions_costs))
        .route("/api/sessions/{id}", get(api_session_detail))
        .route("/api/sessions/{id}/events", get(api_session_events))
        .route("/api/sessions/{id}/tokens", get(api_session_tokens))
        .route("/api/sessions/{id}/tools", get(api_session_tools_detail))
        .route("/api/sessions/{id}/learnings", get(api_session_learnings))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Dashboard running at http://localhost:{port}");

    // Open browser
    let url = format!("http://localhost:{port}");
    if let Err(e) = open::that(&url) {
        eprintln!("Could not open browser: {e}");
        println!("Open manually: {url}");
    }

    axum::serve(listener, app).await?;
    Ok(())
}
