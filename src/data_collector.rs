use chrono::{Duration, Local, NaiveDate};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Goals;
use crate::providers::{
    ClaudeCodeProvider, CodexProvider, GeminiProvider, OpenCodeProvider, AgentProvider,
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

// ── Cache ─────────────────────────────────────────────────────────────────────

static CACHE: LazyLock<Mutex<HashMap<String, (u64, DayMetrics)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_key(target: &NaiveDate, goals: &Goals) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60;
    format!(
        "{}_{}_{}_{}_{:?}",
        now, target, goals.tokens, goals.focus_min, goals.enabled_agents
    )
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn collect_day_metrics(target: NaiveDate, goals: &Goals) -> DayMetrics {
    let key = cache_key(&target, goals);
    {
        let cache = CACHE.lock().unwrap();
        if let Some((_, cached)) = cache.get(&key) {
            return cached.clone();
        }
    }

    let provs = active_providers(goals);
    let mut tokens: u64 = 0;
    let mut tool_calls: u64 = 0;
    let mut focus_min: f64 = 0.0;

    for p in &provs {
        let (t, tc) = p.collect_tokens_and_tools(target);
        tokens += t;
        tool_calls += tc;
        focus_min += p.collect_focus_minutes(target);
    }

    let m = DayMetrics {
        date: target.to_string(),
        tokens,
        tool_calls,
        focus_min: (focus_min * 10.0).round() / 10.0,
        token_pct: None,
        tool_pct: None,
        focus_pct: None,
    }
    .with_goals(goals);

    {
        let mut cache = CACHE.lock().unwrap();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        cache.insert(key, (ts, m.clone()));
    }

    m
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

pub fn collect_hourly(target: NaiveDate, goals: &Goals) -> crate::providers::HourlyData {
    use crate::providers::HourlyData;
    let provs = active_providers(goals);
    let mut tokens_h = vec![0u64; 24];
    let mut tools_h = vec![0u64; 24];
    let mut focus_h = vec![0.0f64; 24];

    for p in &provs {
        let hourly = p.collect_hourly(target);
        for h in 0..24 {
            tokens_h[h] += hourly.tokens[h];
            tools_h[h] += hourly.tools[h];
            focus_h[h] += hourly.focus[h];
        }
    }

    HourlyData {
        tokens: tokens_h,
        tools: tools_h,
        focus: focus_h,
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
        AgentMeta { id: "opencode", label: "OpenCode", dir: "~/.opencode" },
    ]
}
