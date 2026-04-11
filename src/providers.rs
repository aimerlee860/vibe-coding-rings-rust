use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;

// ── Constants ─────────────────────────────────────────────────────────────────

const IDLE_GAP_MS: i64 = 30 * 60 * 1000;     // 30 min gap → new focus block
const TRAIL_BUFFER_MS: i64 = 5 * 60 * 1000;   // 5 min credit after last message

// ── Shared helpers ────────────────────────────────────────────────────────────

pub fn local_date_to_utc_ms_range(target: chrono::NaiveDate) -> (i64, i64) {
    let tz = Local;
    let local_start = tz
        .with_ymd_and_hms(target.year(), target.month(), target.day(), 0, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    let local_end = local_start + Duration::days(1);
    let start_ms = local_start.timestamp_millis();
    let end_ms = local_end.timestamp_millis();
    (start_ms, end_ms)
}

pub fn ms_to_local_hour(ms: i64) -> u32 {
    let secs = ms / 1000;
    let dt: DateTime<Local> = Utc.timestamp_opt(secs, 0).unwrap().with_timezone(&Local);
    dt.hour()
}

pub fn iso_to_ms(ts_raw: &str) -> Option<i64> {
    // Handle ISO 8601 with optional Z suffix
    let s = ts_raw.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&s)
        .ok()
        .map(|dt| dt.timestamp_millis())
        .or_else(|| {
            // Try parsing as epoch seconds
            ts_raw.parse::<f64>().ok().map(|f| (f * 1000.0) as i64)
        })
}

// ── Focus block calculation ───────────────────────────────────────────────────

type Sessions = HashMap<String, Vec<i64>>;

fn sessions_to_focus_blocks(sessions: &Sessions) -> Vec<(i64, i64)> {
    let mut blocks: Vec<(i64, i64)> = Vec::new();
    for ts_list in sessions.values() {
        if ts_list.is_empty() {
            continue;
        }
        let mut ts_sorted = ts_list.clone();
        ts_sorted.sort();
        let mut blk_start = ts_sorted[0];
        let mut blk_end = ts_sorted[0];
        for &ts in &ts_sorted[1..] {
            if ts - blk_end > IDLE_GAP_MS {
                blocks.push((blk_start, blk_end + TRAIL_BUFFER_MS));
                blk_start = ts;
            }
            blk_end = ts;
        }
        blocks.push((blk_start, blk_end + TRAIL_BUFFER_MS));
    }
    blocks
}

fn merge_intervals(intervals: &mut [(i64, i64)]) -> Vec<(i64, i64)> {
    if intervals.is_empty() {
        return Vec::new();
    }
    intervals.sort();
    let mut result = vec![intervals[0]];
    for &(s, e) in &intervals[1..] {
        let last = result.last_mut().unwrap();
        if s <= last.1 {
            last.1 = last.1.max(e);
        } else {
            result.push((s, e));
        }
    }
    result
}

fn interval_ms_in_range(merged: &[(i64, i64)], lo: i64, hi: i64) -> i64 {
    let mut total: i64 = 0;
    for &(s, e) in merged {
        let cs = s.max(lo);
        let ce = e.min(hi);
        if cs < ce {
            total += ce - cs;
        }
    }
    total
}

pub fn focus_from_sessions(sessions: &Sessions, start_ms: i64, end_ms: i64) -> f64 {
    if sessions.is_empty() {
        return 0.0;
    }
    let mut blocks = sessions_to_focus_blocks(sessions);
    let merged = merge_intervals(&mut blocks);
    interval_ms_in_range(&merged, start_ms, end_ms) as f64 / 60_000.0
}

pub fn focus_hourly_from_sessions(sessions: &Sessions, start_ms: i64, end_ms: i64) -> Vec<f64> {
    if sessions.is_empty() {
        return vec![0.0; 24];
    }
    let mut blocks = sessions_to_focus_blocks(sessions);
    let merged = merge_intervals(&mut blocks);
    let hour_ms = 3_600_000;
    (0..24)
        .map(|h| {
            interval_ms_in_range(&merged, start_ms + h as i64 * hour_ms, start_ms + (h + 1) as i64 * hour_ms) as f64
                / 60_000.0
        })
        .collect()
}

// ── JSONL reading helpers ─────────────────────────────────────────────────────

pub fn read_jsonl(path: &Path) -> Vec<Value> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn read_history_sessions(
    history_file: &Path,
    start_ms: i64,
    end_ms: i64,
    filter_slashcmds: bool,
) -> Sessions {
    let mut sessions: Sessions = HashMap::new();
    if !history_file.exists() {
        return sessions;
    }
    for entry in read_jsonl(history_file) {
        let ts = match entry.get("timestamp") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            _ => continue,
        };
        if !(start_ms..end_ms).contains(&ts) {
            continue;
        }
        if filter_slashcmds {
            if let Some(Value::String(display)) = entry.get("display") {
                if display.trim().starts_with('/') {
                    continue;
                }
            }
        }
        let sid = match entry.get("sessionId") {
            Some(Value::String(s)) => s.clone(),
            _ => "__nosid__".to_string(),
        };
        sessions.entry(sid).or_default().push(ts);
    }
    sessions
}

// ── File iteration helper ─────────────────────────────────────────────────────

pub fn iter_session_files(dir: &Path, start_ms: i64, end_ms: i64) -> Vec<PathBuf> {
    let mtime_lo = (start_ms as f64 / 1000.0) - 2.0 * 86_400.0;
    let mtime_hi = (end_ms as f64 / 1000.0) + 2.0 * 86_400.0;
    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(meta) = fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let mtime = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as f64;
                    if mtime_lo <= mtime && mtime <= mtime_hi {
                        files.push(path.to_path_buf());
                    }
                }
            }
        }
    }
    files
}

// ── Provider trait ────────────────────────────────────────────────────────────

pub trait AgentProvider: Send + Sync {
    fn is_available(&self) -> bool;
    fn collect_tokens_and_tools(&self, target: chrono::NaiveDate) -> (u64, u64);
    fn collect_focus_minutes(&self, target: chrono::NaiveDate) -> f64;
    fn collect_hourly(&self, target: chrono::NaiveDate) -> HourlyData;
}

pub struct HourlyData {
    pub tokens: Vec<u64>,
    pub tools: Vec<u64>,
    pub focus: Vec<f64>,
}

impl Default for HourlyData {
    fn default() -> Self {
        HourlyData {
            tokens: vec![0; 24],
            tools: vec![0; 24],
            focus: vec![0.0; 24],
        }
    }
}

// ── Claude Code provider ──────────────────────────────────────────────────────

pub struct ClaudeCodeProvider;

impl AgentProvider for ClaudeCodeProvider {
    fn is_available(&self) -> bool {
        config::claude_dir().exists()
    }

    fn collect_tokens_and_tools(&self, target: chrono::NaiveDate) -> (u64, u64) {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let projects_dir = config::claude_dir().join("projects");
        if !projects_dir.exists() {
            return (0, 0);
        }
        let mut tokens: u64 = 0;
        let mut tool_calls: u64 = 0;
        for file in iter_session_files(&projects_dir, start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                let ts_raw = match entry.get("timestamp") {
                    Some(Value::String(s)) => s.as_str(),
                    _ => continue,
                };
                let ts_ms = match iso_to_ms(ts_raw) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                    continue;
                }
                let msg = match entry.get("message") {
                    Some(Value::Object(m)) => m,
                    _ => continue,
                };
                let usage = match msg.get("usage") {
                    Some(Value::Object(u)) => u,
                    _ => continue,
                };
                tokens += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                tokens += usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                tokens += usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(Value::Array(content)) = msg.get("content") {
                    for block in content {
                        if let Value::Object(b) = block {
                            if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                tool_calls += 1;
                            }
                        }
                    }
                }
            }
        }
        (tokens, tool_calls)
    }

    fn collect_focus_minutes(&self, target: chrono::NaiveDate) -> f64 {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let history_file = config::claude_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, true);
        focus_from_sessions(&sessions, start_ms, end_ms)
    }

    fn collect_hourly(&self, target: chrono::NaiveDate) -> HourlyData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = HourlyData::default();
        let projects_dir = config::claude_dir().join("projects");
        if projects_dir.exists() {
            for file in iter_session_files(&projects_dir, start_ms, end_ms) {
                for entry in read_jsonl(&file) {
                    let ts_raw = match entry.get("timestamp") {
                        Some(Value::String(s)) => s.as_str(),
                        _ => continue,
                    };
                    let ts_ms = match iso_to_ms(ts_raw) {
                        Some(ms) => ms,
                        None => continue,
                    };
                    if !(start_ms..end_ms).contains(&ts_ms) {
                        continue;
                    }
                    if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                        continue;
                    }
                    let hour = ms_to_local_hour(ts_ms) as usize;
                    let msg = match entry.get("message") {
                        Some(Value::Object(m)) => m,
                        _ => continue,
                    };
                    let usage = match msg.get("usage") {
                        Some(Value::Object(u)) => u,
                        _ => continue,
                    };
                    data.tokens[hour] += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    data.tokens[hour] += usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    data.tokens[hour] += usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if let Some(Value::Array(content)) = msg.get("content") {
                        for block in content {
                            if let Value::Object(b) = block {
                                if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    data.tools[hour] += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        let history_file = config::claude_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, true);
        data.focus = focus_hourly_from_sessions(&sessions, start_ms, end_ms);
        data
    }
}

// ── Codex provider ────────────────────────────────────────────────────────────

pub struct CodexProvider;

impl CodexProvider {
    fn parse_ts(&self, entry: &Value) -> Option<i64> {
        for key in &["timestamp", "created_at", "created"] {
            match entry.get(*key) {
                Some(Value::String(s)) => {
                    if let Some(ms) = iso_to_ms(s) {
                        return Some(ms);
                    }
                }
                Some(Value::Number(n)) => {
                    let val = n.as_f64()?;
                    if val > 1_000_000_000.0 {
                        let ts = val as i64;
                        return Some(if ts < 1_000_000_000_000 { ts * 1000 } else { ts });
                    }
                }
                _ => continue,
            }
        }
        None
    }

    fn extract_tokens(&self, entry: &Value) -> u64 {
        let usage = match entry.get("usage") {
            Some(Value::Object(u)) => u,
            _ => return 0,
        };
        if usage.contains_key("input_tokens") {
            return usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                + usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                + usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
        }
        usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
            + usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
    }

    fn extract_tool_calls(&self, entry: &Value) -> u64 {
        let mut count: u64 = 0;
        let msg = entry.get("message").unwrap_or(entry);
        if let Some(Value::Array(content)) = msg.get("content") {
            for block in content {
                if let Value::Object(b) = block {
                    if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        count += 1;
                    }
                }
            }
        }
        if let Some(Value::Array(tool_calls)) = msg.get("tool_calls") {
            count += tool_calls.len() as u64;
        }
        count
    }

    fn is_assistant(&self, entry: &Value) -> bool {
        let role = entry
            .get("role")
            .or_else(|| entry.get("message").and_then(|m| m.get("role")))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        role == "assistant" || entry.get("type").and_then(|v| v.as_str()) == Some("assistant")
    }
}

impl AgentProvider for CodexProvider {
    fn is_available(&self) -> bool {
        config::codex_dir().exists()
    }

    fn collect_tokens_and_tools(&self, target: chrono::NaiveDate) -> (u64, u64) {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut tokens: u64 = 0;
        let mut tool_calls: u64 = 0;
        for file in iter_session_files(&config::codex_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                if !self.is_assistant(&entry) {
                    continue;
                }
                let ts_ms = match self.parse_ts(&entry) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                tokens += self.extract_tokens(&entry);
                tool_calls += self.extract_tool_calls(&entry);
            }
        }
        (tokens, tool_calls)
    }

    fn collect_focus_minutes(&self, target: chrono::NaiveDate) -> f64 {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let history_file = config::codex_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, false);
        focus_from_sessions(&sessions, start_ms, end_ms)
    }

    fn collect_hourly(&self, target: chrono::NaiveDate) -> HourlyData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = HourlyData::default();
        for file in iter_session_files(&config::codex_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                if !self.is_assistant(&entry) {
                    continue;
                }
                let ts_ms = match self.parse_ts(&entry) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                let hour = ms_to_local_hour(ts_ms) as usize;
                data.tokens[hour] += self.extract_tokens(&entry);
                data.tools[hour] += self.extract_tool_calls(&entry);
            }
        }
        let history_file = config::codex_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, false);
        data.focus = focus_hourly_from_sessions(&sessions, start_ms, end_ms);
        data
    }
}

// ── Gemini CLI provider ───────────────────────────────────────────────────────

pub struct GeminiProvider;

impl GeminiProvider {
    fn parse_ts(&self, entry: &Value) -> Option<i64> {
        for key in &["timestamp", "created_at", "createTime"] {
            match entry.get(*key) {
                Some(Value::String(s)) => {
                    if let Some(ms) = iso_to_ms(s) {
                        return Some(ms);
                    }
                }
                Some(Value::Number(n)) => {
                    let val = n.as_f64()?;
                    if val > 1_000_000_000.0 {
                        let ts = val as i64;
                        return Some(if ts < 1_000_000_000_000 { ts * 1000 } else { ts });
                    }
                }
                _ => continue,
            }
        }
        None
    }

    fn extract_tokens(&self, entry: &Value) -> u64 {
        let meta = match entry.get("usageMetadata") {
            Some(Value::Object(m)) => m,
            _ => return 0,
        };
        if let Some(total) = meta.get("totalTokenCount").and_then(|v| v.as_u64()) {
            if total > 0 {
                return total;
            }
        }
        meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
            + meta
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
    }

    fn extract_tool_calls(&self, entry: &Value) -> u64 {
        let mut count: u64 = 0;
        if let Some(Value::Array(candidates)) = entry.get("candidates") {
            for candidate in candidates {
                if let Some(Value::Object(content)) = candidate.get("content") {
                    if let Some(Value::Array(parts)) = content.get("parts") {
                        for part in parts {
                            if let Value::Object(p) = part {
                                if p.contains_key("functionCall") {
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(Value::Array(parts)) = entry.get("parts") {
            for part in parts {
                if let Value::Object(p) = part {
                    if p.contains_key("functionCall") {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    fn is_model_response(&self, entry: &Value) -> bool {
        let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");
        role == "model"
            || role == "assistant"
            || entry.get("candidates").is_some()
            || entry.get("usageMetadata").is_some()
    }
}

impl AgentProvider for GeminiProvider {
    fn is_available(&self) -> bool {
        config::gemini_dir().exists()
    }

    fn collect_tokens_and_tools(&self, target: chrono::NaiveDate) -> (u64, u64) {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut tokens: u64 = 0;
        let mut tool_calls: u64 = 0;
        for file in iter_session_files(&config::gemini_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                if !self.is_model_response(&entry) {
                    continue;
                }
                let ts_ms = match self.parse_ts(&entry) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                tokens += self.extract_tokens(&entry);
                tool_calls += self.extract_tool_calls(&entry);
            }
        }
        (tokens, tool_calls)
    }

    fn collect_focus_minutes(&self, target: chrono::NaiveDate) -> f64 {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        for name in &["history.jsonl", "history"] {
            let history = config::gemini_dir().join(name);
            if history.exists() {
                let sessions = read_history_sessions(&history, start_ms, end_ms, false);
                let result = focus_from_sessions(&sessions, start_ms, end_ms);
                if result > 0.0 {
                    return result;
                }
            }
        }
        0.0
    }

    fn collect_hourly(&self, target: chrono::NaiveDate) -> HourlyData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = HourlyData::default();
        for file in iter_session_files(&config::gemini_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                if !self.is_model_response(&entry) {
                    continue;
                }
                let ts_ms = match self.parse_ts(&entry) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                let hour = ms_to_local_hour(ts_ms) as usize;
                data.tokens[hour] += self.extract_tokens(&entry);
                data.tools[hour] += self.extract_tool_calls(&entry);
            }
        }
        for name in &["history.jsonl", "history"] {
            let history = config::gemini_dir().join(name);
            if history.exists() {
                let sessions = read_history_sessions(&history, start_ms, end_ms, false);
                data.focus = focus_hourly_from_sessions(&sessions, start_ms, end_ms);
                break;
            }
        }
        data
    }
}

// ── OpenCode provider ─────────────────────────────────────────────────────────

pub struct OpenCodeProvider;

impl AgentProvider for OpenCodeProvider {
    fn is_available(&self) -> bool {
        config::opencode_dir().exists()
    }

    fn collect_tokens_and_tools(&self, target: chrono::NaiveDate) -> (u64, u64) {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut tokens: u64 = 0;
        let mut tool_calls: u64 = 0;
        for file in iter_session_files(&config::opencode_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                let ts_raw = match entry.get("timestamp") {
                    Some(Value::String(s)) => s.as_str(),
                    _ => continue,
                };
                let ts_ms = match iso_to_ms(ts_raw) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                    continue;
                }
                let msg = match entry.get("message") {
                    Some(Value::Object(m)) => m,
                    _ => continue,
                };
                let usage = match msg.get("usage") {
                    Some(Value::Object(u)) => u,
                    _ => continue,
                };
                tokens += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                tokens += usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                tokens += usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(Value::Array(content)) = msg.get("content") {
                    for block in content {
                        if let Value::Object(b) = block {
                            if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                tool_calls += 1;
                            }
                        }
                    }
                }
            }
        }
        (tokens, tool_calls)
    }

    fn collect_focus_minutes(&self, target: chrono::NaiveDate) -> f64 {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let history_file = config::opencode_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, false);
        focus_from_sessions(&sessions, start_ms, end_ms)
    }

    fn collect_hourly(&self, target: chrono::NaiveDate) -> HourlyData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = HourlyData::default();
        for file in iter_session_files(&config::opencode_dir(), start_ms, end_ms) {
            for entry in read_jsonl(&file) {
                let ts_raw = match entry.get("timestamp") {
                    Some(Value::String(s)) => s.as_str(),
                    _ => continue,
                };
                let ts_ms = match iso_to_ms(ts_raw) {
                    Some(ms) => ms,
                    None => continue,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    continue;
                }
                if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                    continue;
                }
                let hour = ms_to_local_hour(ts_ms) as usize;
                let msg = match entry.get("message") {
                    Some(Value::Object(m)) => m,
                    _ => continue,
                };
                let usage = match msg.get("usage") {
                    Some(Value::Object(u)) => u,
                    _ => continue,
                };
                data.tokens[hour] += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                data.tokens[hour] += usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                data.tokens[hour] += usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(Value::Array(content)) = msg.get("content") {
                    for block in content {
                        if let Value::Object(b) = block {
                            if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                data.tools[hour] += 1;
                            }
                        }
                    }
                }
            }
        }
        let history_file = config::opencode_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, false);
        data.focus = focus_hourly_from_sessions(&sessions, start_ms, end_ms);
        data
    }
}
