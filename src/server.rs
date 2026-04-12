use axum::extract::Query;
use axum::{extract::State, routing::get, routing::post, Json, Router};
use chrono::NaiveDate;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;

use crate::config::{load_config, save_config};
use crate::data_collector::{
    agent_meta, calc_streak, collect_day_metrics, collect_history, collect_hourly, providers,
};

pub const PORT: u16 = 9876;

// ── Shared state (goals-changed callbacks) ────────────────────────────────────

pub struct AppState {
    pub goals_changed_callbacks: Vec<Box<dyn Fn() + Send + Sync>>,
}

pub type SharedState = Arc<RwLock<AppState>>;

#[allow(dead_code)]
pub fn make_state() -> SharedState {
    Arc::new(RwLock::new(AppState {
        goals_changed_callbacks: Vec::new(),
    }))
}

// ── Request models ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GoalsIn {
    tokens: u64,
    focus_min: u64,
    tool_calls: u64,
}

#[derive(Deserialize)]
struct LangIn {
    lang: String,
}

#[derive(Deserialize)]
struct AgentsIn {
    enabled: Vec<String>,
}

#[derive(Deserialize)]
struct HourlyQuery {
    metric: Option<String>,
    d: Option<String>,
}

// ── API routes ────────────────────────────────────────────────────────────────

pub fn build_router(static_dir: String, state: SharedState) -> Router {
    let api = Router::new()
        .route("/api/today", get(api_today))
        .route("/api/history", get(api_history))
        .route("/api/goals", get(api_get_goals).post(api_set_goals))
        .route("/api/agents", get(api_get_agents).post(api_set_agents))
        .route("/api/lang", post(api_set_lang))
        .route("/api/hourly", get(api_hourly))
        .with_state(state);

    let static_service = ServeDir::new(&static_dir).append_index_html_on_directories(true);

    Router::new().merge(api).fallback_service(static_service)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn api_today(State(_state): State<SharedState>) -> Json<serde_json::Value> {
    let goals = load_config();
    let today = chrono::Local::now().date_naive();
    let metrics = collect_day_metrics(today, &goals);
    let history = collect_history(&goals, 7);
    let streak = calc_streak(&history);

    Json(serde_json::json!({
        "metrics": {
            "date": metrics.date,
            "tokens": metrics.tokens,
            "tool_calls": metrics.tool_calls,
            "focus_min": metrics.focus_min,
            "token_pct": metrics.token_pct,
            "tool_pct": metrics.tool_pct,
            "focus_pct": metrics.focus_pct,
        },
        "streak": streak,
        "goals": {
            "tokens": goals.tokens,
            "focus_min": goals.focus_min,
            "tool_calls": goals.tool_calls,
        },
    }))
}

async fn api_history() -> Json<Vec<serde_json::Value>> {
    let goals = load_config();
    let history = collect_history(&goals, 7);
    Json(
        history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "date": m.date,
                    "tokens": m.tokens,
                    "tool_calls": m.tool_calls,
                    "focus_min": m.focus_min,
                    "token_pct": m.token_pct,
                    "tool_pct": m.tool_pct,
                    "focus_pct": m.focus_pct,
                })
            })
            .collect(),
    )
}

async fn api_get_goals() -> Json<serde_json::Value> {
    let goals = load_config();
    Json(serde_json::json!({
        "tokens": goals.tokens,
        "focus_min": goals.focus_min,
        "tool_calls": goals.tool_calls,
    }))
}

async fn api_set_goals(
    State(state): State<SharedState>,
    Json(body): Json<GoalsIn>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if body.tokens < 10_000 || body.focus_min < 1 || body.tool_calls < 1 {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let mut goals = load_config();
    goals.tokens = body.tokens;
    goals.focus_min = body.focus_min;
    goals.tool_calls = body.tool_calls;
    save_config(&goals);

    let state = state.read().await;
    for cb in &state.goals_changed_callbacks {
        cb();
    }

    Ok(Json(serde_json::json!({
        "tokens": goals.tokens,
        "focus_min": goals.focus_min,
        "tool_calls": goals.tool_calls,
    })))
}

async fn api_get_agents() -> Json<Vec<serde_json::Value>> {
    let goals = load_config();
    let provs = providers();
    let meta = agent_meta();
    Json(
        meta.iter()
            .map(|m| {
                let available = provs
                    .iter()
                    .find(|(k, _)| *k == m.id)
                    .map(|(_, p)| p.is_available())
                    .unwrap_or(false);
                serde_json::json!({
                    "id": m.id,
                    "label": m.label,
                    "dir": m.dir,
                    "enabled": goals.enabled_agents.contains(&m.id.to_string()),
                    "available": available,
                })
            })
            .collect(),
    )
}

async fn api_set_agents(
    State(state): State<SharedState>,
    Json(body): Json<AgentsIn>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let valid: Vec<String> = agent_meta()
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    let enabled: Vec<String> = body.enabled.into_iter().filter(|a| valid.contains(a)).collect();
    if enabled.is_empty() {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let mut goals = load_config();
    goals.enabled_agents = enabled;
    save_config(&goals);

    let state = state.read().await;
    for cb in &state.goals_changed_callbacks {
        cb();
    }

    Ok(Json(serde_json::json!({ "enabled": goals.enabled_agents })))
}

async fn api_set_lang(
    State(state): State<SharedState>,
    Json(body): Json<LangIn>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if body.lang != "zh" && body.lang != "en" {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let mut goals = load_config();
    goals.lang = body.lang;
    save_config(&goals);

    let state = state.read().await;
    for cb in &state.goals_changed_callbacks {
        cb();
    }

    Ok(Json(serde_json::json!({ "lang": goals.lang })))
}

async fn api_hourly(Query(query): Query<HourlyQuery>) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let metric = query.metric.as_deref().unwrap_or("tokens");
    if !["tokens", "tools", "focus"].contains(&metric) {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let target = query
        .d
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Local::now().date_naive());

    let goals = load_config();
    let hourly = collect_hourly(target, &goals);
    let day_metrics = collect_day_metrics(target, &goals);

    let (hourly_data, total, goal_val) = match metric {
        "tokens" => (&hourly.tokens, day_metrics.tokens, goals.tokens),
        "tools" => (&hourly.tools, day_metrics.tool_calls, goals.tool_calls),
        "focus" => {
            return Ok(Json(serde_json::json!({
                "metric": metric,
                "date": target.to_string(),
                "hourly": hourly.focus,
                "total": day_metrics.focus_min,
                "goal": goals.focus_min,
            })));
        }
        _ => return Err(axum::http::StatusCode::BAD_REQUEST),
    };

    Ok(Json(serde_json::json!({
        "metric": metric,
        "date": target.to_string(),
        "hourly": hourly_data,
        "total": total,
        "goal": goal_val,
    })))
}

// ── Start server ──────────────────────────────────────────────────────────────

pub fn start_server(static_dir: String, state: SharedState) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let router = build_router(static_dir, state);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], PORT));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind port 9876");
        if let Err(e) = axum::serve(listener, router).await {
            eprintln!("[VCR] Server error: {e}");
        }
    });
}
