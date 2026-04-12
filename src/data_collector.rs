use chrono::{Duration, Local, NaiveDate};

use crate::config::{Goals, StreakCache};
use crate::providers::{
    ClaudeCodeProvider, CodexProvider, GeminiProvider, OpenCodeProvider, AgentProvider,
    HourlyData,
};

// ── Provider registry ─────────────────────────────────────────────────────────

pub fn providers() -> Vec<(&'static str, Box<dyn AgentProvider>)> {
    vec![
        ("claude_code", Box::new(ClaudeCodeProvider)),
        ("codex", Box::new(CodexProvider)),
        ("gemini", Box::new(GeminiProvider)),
        ("opencode", Box::new(OpenCodeProvider)),
    ]
}

fn active_providers(goals: &Goals) -> Vec<Box<dyn AgentProvider>> {
    providers()
        .into_iter()
        .filter(|(key, p)| goals.enabled_agents.contains(&key.to_string()) && p.is_available())
        .map(|(_, p)| p)
        .collect()
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
    let provs = active_providers(goals);
    let mut total_tokens: u64 = 0;
    let mut total_tools: u64 = 0;
    let mut total_focus: f64 = 0.0;
    let mut hourly_tokens = vec![0u64; 24];
    let mut hourly_tools = vec![0u64; 24];
    let mut hourly_focus = vec![0.0f64; 24];

    for p in &provs {
        let data = p.collect_all(target);
        total_tokens += data.tokens;
        total_tools += data.tools;
        total_focus += data.focus_min;
        for h in 0..24 {
            hourly_tokens[h] += data.hourly.tokens[h];
            hourly_tools[h] += data.hourly.tools[h];
            hourly_focus[h] += data.hourly.focus[h];
        }
    }

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

pub fn collect_history(goals: &Goals, days: usize) -> Vec<DayMetrics> {
    let today = Local::now().date_naive();
    (0..days)
        .map(|i| collect_day_metrics(today - Duration::days(i as i64), goals))
        .collect()
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

/// Collect hourly data - no caching, always fresh data
pub fn collect_hourly(target: NaiveDate, goals: &Goals) -> HourlyData {
    collect_day_data(target, goals).1
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
        AgentMeta { id: "opencode", label: "OpenCode", dir: "~/.opencode" },
    ]
}
