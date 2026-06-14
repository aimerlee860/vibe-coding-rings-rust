use chrono::{Duration, Local, NaiveDate};
use std::time::Instant;

use crate::config::{Goals, StreakCache};
use crate::providers::{
    ClaudeCodeProvider, CodexProvider, GeminiProvider, OpenCodeProvider, AgentProvider,
    HourlyData, local_date_to_utc_ms_range, merge_intervals, interval_ms_in_range,
};

// ── 10-second data cache ───────────────────────────────────────────────────────

const CACHE_TTL_SECS: u64 = 10;

struct DayCache {
    refreshed_at: Instant,
    date: NaiveDate,
    metrics: DayMetrics,
    hourly: HourlyData,
    per_provider: Vec<ProviderDayData>,
}

struct HistoryCache {
    refreshed_at: Instant,
    days: usize,
    data: Vec<DayMetrics>,
}

static DAY_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<DayCache>>> =
    std::sync::OnceLock::new();
static HISTORY_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<HistoryCache>>> =
    std::sync::OnceLock::new();

// ── Provider registry ─────────────────────────────────────────────────────────

pub fn providers() -> Vec<(&'static str, Box<dyn AgentProvider>)> {
    vec![
        ("claude_code", Box::new(ClaudeCodeProvider)),
        ("codex", Box::new(CodexProvider)),
        ("gemini", Box::new(GeminiProvider)),
        ("opencode", Box::new(OpenCodeProvider)),
    ]
}


// ── Day metrics ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct DayMetrics {
    pub date: String,
    pub tokens: u64,
    pub tool_calls: u64,
    pub focus_min: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_pct: Option<f64>,
}

impl DayMetrics {
    fn with_goals(mut self, goals: &Goals) -> Self {
        self.token_pct = if goals.tokens > 0 {
            Some(self.tokens as f64 / goals.tokens as f64)
        } else {
            Some(0.0)
        };
        self.tool_pct = if goals.tool_calls > 0 {
            Some(self.tool_calls as f64 / goals.tool_calls as f64)
        } else {
            Some(0.0)
        };
        self.focus_pct = if goals.focus_min > 0 {
            Some(self.focus_min / goals.focus_min as f64)
        } else {
            Some(0.0)
        };
        self
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Collect all data for a day in one pass - returns both metrics and hourly breakdown
fn collect_day_data(target: NaiveDate, goals: &Goals) -> (DayMetrics, HourlyData) {
    // Check cache — skip file I/O if data was collected within TTL
    if let Some(cache) = DAY_CACHE.get() {
        let guard = cache.lock().unwrap();
        if let Some(ref c) = *guard {
            if c.date == target && c.refreshed_at.elapsed().as_secs() < CACHE_TTL_SECS {
                return (c.metrics.clone(), c.hourly.clone());
            }
        }
    }

    let meta: std::collections::HashMap<&str, &str> = agent_meta()
        .iter()
        .map(|m| (m.id, m.label))
        .collect();

    let provs = providers();
    let mut total_tokens: u64 = 0;
    let mut total_tools: u64 = 0;
    let mut all_focus_blocks: Vec<(i64, i64)> = Vec::new();
    let mut hourly_tokens = vec![0u64; 24];
    let mut hourly_tools = vec![0u64; 24];
    let mut per_provider: Vec<ProviderDayData> = Vec::new();

    for (key, p) in &provs {
        // Skip disabled or unavailable providers
        if !goals.enabled_agents.contains(&key.to_string()) || !p.is_available() {
            continue;
        }
        let data = p.collect_all(target);
        total_tokens += data.tokens;
        total_tools += data.tools;
        all_focus_blocks.extend(data.focus_blocks);
        for h in 0..24 {
            hourly_tokens[h] += data.hourly.tokens[h];
            hourly_tools[h] += data.hourly.tools[h];
        }

        // Collect per-provider data (only providers with activity)
        if data.tokens > 0 || data.tools > 0 {
            let label = meta.get(key).map(|s| s.to_string()).unwrap_or_else(|| key.to_string());
            per_provider.push(ProviderDayData {
                id: key.to_string(),
                label,
                tokens: data.tokens,
                tools: data.tools,
                focus_min: data.focus_min,
            });
        }
    }

    // Cross-provider dedup: merge every provider's focus intervals, then measure
    // total minutes and per-hour minutes against the day window.
    let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
    let merged_focus = merge_intervals(&mut all_focus_blocks);
    let total_focus = interval_ms_in_range(&merged_focus, start_ms, end_ms) as f64 / 60_000.0;
    let hour_ms = 3_600_000;
    let hourly_focus: Vec<f64> = (0..24)
        .map(|h| {
            interval_ms_in_range(&merged_focus, start_ms + h as i64 * hour_ms, start_ms + (h + 1) as i64 * hour_ms) as f64
                / 60_000.0
        })
        .collect();

    let metrics = DayMetrics {
        date: target.to_string(),
        tokens: total_tokens,
        tool_calls: total_tools,
        focus_min: (total_focus * 10.0).round() / 10.0,
        token_pct: None,
        tool_pct: None,
        focus_pct: None,
    }.with_goals(goals);

    let hourly = HourlyData {
        tokens: hourly_tokens,
        tools: hourly_tools,
        focus: hourly_focus,
    };

    // Update cache
    let cache = DAY_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    *cache.lock().unwrap() = Some(DayCache {
        refreshed_at: Instant::now(),
        date: target,
        metrics: metrics.clone(),
        hourly: hourly.clone(),
        per_provider,
    });

    (metrics, hourly)
}

/// Collect day metrics - no caching, always fresh data
pub fn collect_day_metrics(target: NaiveDate, goals: &Goals) -> DayMetrics {
    collect_day_data(target, goals).0
}

/// Collect both metrics and hourly data in one pass - avoids duplicate scanning
pub fn collect_day_full(target: NaiveDate, goals: &Goals) -> (DayMetrics, HourlyData) {
    collect_day_data(target, goals)
}

/// Clear all caches (call after config changes like agent toggle)
pub fn clear_caches() {
    if let Some(cache) = DAY_CACHE.get() {
        *cache.lock().unwrap() = None;
    }
    if let Some(cache) = HISTORY_CACHE.get() {
        *cache.lock().unwrap() = None;
    }
}

pub fn collect_history(goals: &Goals, days: usize) -> Vec<DayMetrics> {
    // Check cache — skip file I/O if data was collected within TTL
    if let Some(cache) = HISTORY_CACHE.get() {
        let guard = cache.lock().unwrap();
        if let Some(ref c) = *guard {
            if c.days == days && c.refreshed_at.elapsed().as_secs() < CACHE_TTL_SECS {
                return c.data.clone();
            }
        }
    }

    let today = Local::now().date_naive();
    let history: Vec<DayMetrics> = (0..days)
        .map(|i| collect_day_metrics(today - Duration::days(i as i64), goals))
        .collect();

    // Update cache
    let cache = HISTORY_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    *cache.lock().unwrap() = Some(HistoryCache {
        refreshed_at: Instant::now(),
        days,
        data: history.clone(),
    });

    history
}

pub fn calc_streak(history: &[DayMetrics]) -> u32 {
    let mut streak: u32 = 0;
    for day in history {
        let token_ok = day.token_pct.unwrap_or(0.0) >= 1.0;
        let focus_ok = day.focus_pct.unwrap_or(0.0) >= 1.0;
        let tool_ok = day.tool_pct.unwrap_or(0.0) >= 1.0;
        if token_ok && focus_ok && tool_ok {
            streak += 1;
        } else {
            break;
        }
    }
    streak
}

/// Check if a single day meets all goals
fn day_meets_goals(metrics: &DayMetrics) -> bool {
    let token_ok = metrics.token_pct.unwrap_or(0.0) >= 1.0;
    let focus_ok = metrics.focus_pct.unwrap_or(0.0) >= 1.0;
    let tool_ok = metrics.tool_pct.unwrap_or(0.0) >= 1.0;
    token_ok && focus_ok && tool_ok
}

/// Get cached base streak (streak up to yesterday) and update cache if needed
/// Returns (base_streak, needs_full_recalc)
pub fn get_cached_base_streak(goals: &Goals) -> (u32, bool) {
    let today = Local::now().date_naive();
    let today_str = today.to_string();

    // Check if cache exists and is for today's calculation
    if let Some(cache) = &goals.streak_cache {
        // Cache is valid if last_date == today (meaning we already computed base_streak for yesterday)
        if cache.last_date == today_str {
            return (cache.base_streak, false);
        }

        // If cache is from yesterday, it's still valid for base_streak
        let yesterday = today - Duration::days(1);
        if cache.last_date == yesterday.to_string() {
            return (cache.base_streak, false);
        }
    }

    // Cache is stale or missing - need full recalculation
    (0, true)
}

/// Update streak cache after computing history
pub fn update_streak_cache(history: &[DayMetrics], goals: &mut Goals) {
    let today = Local::now().date_naive();

    // history[0] is today, history[1] is yesterday, etc.
    // base_streak should be streak counting from yesterday backwards
    let base_streak = if history.len() > 1 {
        // Skip today, count streak from yesterday
        let mut streak: u32 = 0;
        for day in &history[1..] {
            if day_meets_goals(day) {
                streak += 1;
            } else {
                break;
            }
        }
        streak
    } else {
        0
    };

    goals.streak_cache = Some(StreakCache {
        base_streak,
        last_date: today.to_string(),
    });
    crate::config::save_config(goals);
}

/// Calculate streak efficiently: use cache + today's data
pub fn calc_streak_fast(today_metrics: &DayMetrics, goals: &Goals) -> u32 {
    let (base_streak, needs_recalc) = get_cached_base_streak(goals);

    if needs_recalc {
        // Cache is stale, need to compute from history
        let history = collect_history(goals, 7);
        let mut goals_mut = goals.clone();
        update_streak_cache(&history, &mut goals_mut);
        return calc_streak(&history);
    }

    // Use cache: if today meets goals, streak = base + 1
    if day_meets_goals(today_metrics) {
        base_streak + 1
    } else {
        0
    }
}

// ── Agent meta ────────────────────────────────────────────────────────────────

pub struct AgentMeta {
    pub id: &'static str,
    pub label: &'static str,
    pub dir: &'static str,
}

pub fn agent_meta() -> Vec<AgentMeta> {
    vec![
        AgentMeta { id: "claude_code", label: "Claude Code", dir: "~/.claude" },
        AgentMeta { id: "codex", label: "Codex", dir: "~/.codex" },
        AgentMeta { id: "gemini", label: "Gemini CLI", dir: "~/.gemini" },
        AgentMeta { id: "opencode", label: "OpenCode", dir: "~/.local/share/opencode" },
    ]
}

// ── Per-provider data ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ProviderDayData {
    #[allow(dead_code)]
    pub id: String,
    pub label: String,
    pub tokens: u64,
    pub tools: u64,
    pub focus_min: f64,
}

pub fn collect_per_provider(target: NaiveDate, goals: &Goals) -> Vec<ProviderDayData> {
    // Ensure cache is populated (collect_day_data fills per_provider)
    collect_day_data(target, goals);
    // Read from cache
    if let Some(cache) = DAY_CACHE.get() {
        let guard = cache.lock().unwrap();
        if let Some(ref c) = *guard {
            if c.date == target {
                return c.per_provider.clone();
            }
        }
    }
    Vec::new()
}
