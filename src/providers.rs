use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
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
        // Avoid cloning - collect timestamps and sort in-place
        let mut ts_sorted: Vec<i64> = ts_list.iter().copied().collect();
        ts_sorted.sort_unstable();  // faster than sort() for integers
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

pub fn merge_intervals(intervals: &mut [(i64, i64)]) -> Vec<(i64, i64)> {
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

pub fn interval_ms_in_range(merged: &[(i64, i64)], lo: i64, hi: i64) -> i64 {
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

/// Build deduped focus intervals (merged across sessions) from per-session timestamps.
pub fn sessions_to_merged_blocks(sessions: &Sessions) -> Vec<(i64, i64)> {
    let mut blocks = sessions_to_focus_blocks(sessions);
    merge_intervals(&mut blocks)
}

// ── JSONL reading helpers (streaming) ───────────────────────────────────────────

/// Process JSONL file line by line, calling callback for each parsed entry.
/// More memory-efficient than reading entire file.
pub fn process_jsonl<F>(path: &Path, mut callback: F)
where
    F: FnMut(&Value),
{
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let reader = BufReader::new(file);
    for line in reader.lines().filter_map(|l| l.ok()) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<Value>(&line) {
            callback(&entry);
        }
    }
}

/// Legacy function for backwards compatibility (uses streaming internally)
#[allow(dead_code)]
pub fn read_jsonl(path: &Path) -> Vec<Value> {
    let mut entries = Vec::new();
    process_jsonl(path, |entry| entries.push(entry.clone()));
    entries
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
    // Use streaming instead of loading all entries
    process_jsonl(history_file, |entry| {
        let ts = match entry.get("timestamp") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            _ => return,
        };
        if !(start_ms..end_ms).contains(&ts) {
            return;
        }
        if filter_slashcmds {
            if let Some(Value::String(display)) = entry.get("display") {
                if display.trim().starts_with('/') {
                    return;
                }
            }
        }
        let sid = match entry.get("sessionId") {
            Some(Value::String(s)) => s.clone(),
            _ => "__nosid__".to_string(),
        };
        sessions.entry(sid).or_default().push(ts);
    });
    sessions
}

// ── File iteration helper ─────────────────────────────────────────────────────

/// Maximum age of files to consider (days). Files older than this are skipped entirely.
const MAX_FILE_AGE_DAYS: f64 = 8.0; // 7 days + 1 day buffer for timezone

pub fn iter_session_files(dir: &Path, start_ms: i64, end_ms: i64) -> Vec<PathBuf> {
    // Global cutoff: ignore files older than MAX_FILE_AGE_DAYS from now
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as f64;
    let global_mtime_lo = now_secs - MAX_FILE_AGE_DAYS * 86_400.0;

    // Query-specific window: ±1 day for timezone safety
    let query_mtime_lo = (start_ms as f64 / 1000.0) - 1.0 * 86_400.0;
    let query_mtime_hi = (end_ms as f64 / 1000.0) + 1.0 * 86_400.0;

    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let mtime = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as f64;
                    // Skip files older than global cutoff
                    if mtime < global_mtime_lo {
                        continue;
                    }
                    // Check query-specific window
                    if query_mtime_lo <= mtime && mtime <= query_mtime_hi {
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
    /// Collect all data in one pass (more efficient than separate calls)
    fn collect_all(&self, target: chrono::NaiveDate) -> DayData;
}

/// Combined data for a single day - collected in one file traversal
#[derive(Clone)]
pub struct DayData {
    pub tokens: u64,
    pub tools: u64,
    pub focus_min: f64,                // This provider's focus minutes (per-provider display)
    pub focus_blocks: Vec<(i64, i64)>, // Merged focus intervals (cross-provider dedup at aggregate layer)
    pub hourly: HourlyData,            // focus[] filled by aggregate layer from merged blocks
}

#[derive(Clone)]
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

impl Default for DayData {
    fn default() -> Self {
        DayData {
            tokens: 0,
            tools: 0,
            focus_min: 0.0,
            focus_blocks: Vec::new(),
            hourly: HourlyData::default(),
        }
    }
}

// ── Claude Code provider ──────────────────────────────────────────────────────

pub struct ClaudeCodeProvider;

impl AgentProvider for ClaudeCodeProvider {
    fn is_available(&self) -> bool {
        config::claude_dir().exists()
    }

    /// Combined collection - single file traversal for efficiency
    fn collect_all(&self, target: chrono::NaiveDate) -> DayData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = DayData::default();
        let mut sessions: Sessions = HashMap::new();
        let projects_dir = config::claude_dir().join("projects");

        if projects_dir.exists() {
            for file in iter_session_files(&projects_dir, start_ms, end_ms) {
                // One session per file; use filename (uuid) as the session key.
                let session_key = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("__nosid__")
                    .to_string();
                process_jsonl(&file, |entry| {
                    let ts_raw = match entry.get("timestamp") {
                        Some(Value::String(s)) => s.as_str(),
                        _ => return,
                    };
                    let ts_ms = match iso_to_ms(ts_raw) {
                        Some(ms) => ms,
                        None => return,
                    };
                    if !(start_ms..end_ms).contains(&ts_ms) {
                        return;
                    }
                    if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                        return;
                    }

                    // Focus only needs the timestamp of assistant activity — collect it
                    // before the usage guard so messages without usage still count as focus time.
                    sessions.entry(session_key.clone()).or_default().push(ts_ms);

                    let msg = match entry.get("message") {
                        Some(Value::Object(m)) => m,
                        _ => return,
                    };
                    let usage = match msg.get("usage") {
                        Some(Value::Object(u)) => u,
                        _ => return,
                    };

                    let hour = ms_to_local_hour(ts_ms) as usize;
                    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache_creation = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let total_tokens = input_tokens + cache_read + cache_creation + output_tokens;

                    data.tokens += total_tokens;
                    data.hourly.tokens[hour] += total_tokens;

                    if let Some(Value::Array(content)) = msg.get("content") {
                        for block in content {
                            if let Value::Object(b) = block {
                                if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    data.tools += 1;
                                    data.hourly.tools[hour] += 1;
                                }
                            }
                        }
                    }
                });
            }
        }

        // Focus from assistant message timestamps — same source as tokens/tools,
        // so the focus window covers actual agent work time, not just user prompts.
        let merged = sessions_to_merged_blocks(&sessions);
        data.focus_blocks = merged.clone();
        data.focus_min = interval_ms_in_range(&merged, start_ms, end_ms) as f64 / 60_000.0;

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

    fn collect_all(&self, target: chrono::NaiveDate) -> DayData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = DayData::default();

        for file in iter_session_files(&config::codex_dir(), start_ms, end_ms) {
            process_jsonl(&file, |entry| {
                if !self.is_assistant(entry) {
                    return;
                }
                let ts_ms = match self.parse_ts(entry) {
                    Some(ms) => ms,
                    None => return,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    return;
                }
                let hour = ms_to_local_hour(ts_ms) as usize;
                let tokens = self.extract_tokens(entry);
                let tools = self.extract_tool_calls(entry);

                data.tokens += tokens;
                data.tools += tools;
                data.hourly.tokens[hour] += tokens;
                data.hourly.tools[hour] += tools;
            });
        }

        // Focus from history timestamps
        let history_file = config::codex_dir().join("history.jsonl");
        let sessions = read_history_sessions(&history_file, start_ms, end_ms, false);
        let merged = sessions_to_merged_blocks(&sessions);
        data.focus_blocks = merged.clone();
        data.focus_min = interval_ms_in_range(&merged, start_ms, end_ms) as f64 / 60_000.0;

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

    fn collect_all(&self, target: chrono::NaiveDate) -> DayData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = DayData::default();

        for file in iter_session_files(&config::gemini_dir(), start_ms, end_ms) {
            process_jsonl(&file, |entry| {
                if !self.is_model_response(entry) {
                    return;
                }
                let ts_ms = match self.parse_ts(entry) {
                    Some(ms) => ms,
                    None => return,
                };
                if !(start_ms..end_ms).contains(&ts_ms) {
                    return;
                }
                let hour = ms_to_local_hour(ts_ms) as usize;
                let tokens = self.extract_tokens(entry);
                let tools = self.extract_tool_calls(entry);

                data.tokens += tokens;
                data.tools += tools;
                data.hourly.tokens[hour] += tokens;
                data.hourly.tools[hour] += tools;
            });
        }

        // Focus from history timestamps
        for name in &["history.jsonl", "history"] {
            let history = config::gemini_dir().join(name);
            if history.exists() {
                let sessions = read_history_sessions(&history, start_ms, end_ms, false);
                let merged = sessions_to_merged_blocks(&sessions);
                data.focus_blocks = merged.clone();
                data.focus_min = interval_ms_in_range(&merged, start_ms, end_ms) as f64 / 60_000.0;
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
        config::opencode_db().exists()
    }

    fn collect_all(&self, target: chrono::NaiveDate) -> DayData {
        let (start_ms, end_ms) = local_date_to_utc_ms_range(target);
        let mut data = DayData::default();

        let db_path = config::opencode_db();
        let conn = match rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(_) => return data,
        };

        // ── Tokens from message table (per-message, with hourly breakdown) ──
        let mut stmt = match conn.prepare(
            "SELECT json_extract(data, '$.tokens.input'), \
                    json_extract(data, '$.tokens.cache.read'), \
                    json_extract(data, '$.tokens.cache.write'), \
                    json_extract(data, '$.tokens.output'), \
                    json_extract(data, '$.time.created') \
             FROM message \
             WHERE json_extract(data, '$.role') = 'assistant' \
               AND json_extract(data, '$.time.created') >= ? \
               AND json_extract(data, '$.time.created') < ?",
        ) {
            Ok(s) => s,
            Err(_) => return data,
        };

        let rows = stmt.query_map(rusqlite::params![start_ms, end_ms], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as u64,  // input
                row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,  // cache_read
                row.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as u64,  // cache_write
                row.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as u64,  // output
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),                 // timestamp
            ))
        });

        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (input, cache_read, cache_write, output, ts) = row;
                let total = input + cache_read + cache_write + output;
                if total == 0 {
                    continue;
                }
                let hour = ms_to_local_hour(ts) as usize;
                data.tokens += total;
                data.hourly.tokens[hour] += total;
            }
        }

        // ── Tool calls from part table ──
        if let Ok(mut stmt) = conn.prepare(
            "SELECT time_created FROM part \
             WHERE json_extract(data, '$.type') = 'tool' \
               AND time_created >= ? AND time_created < ?",
        ) {
            let rows = stmt.query_map(rusqlite::params![start_ms, end_ms], |row| {
                row.get::<_, i64>(0)
            });
            if let Ok(rows) = rows {
                for row in rows.flatten() {
                    let hour = ms_to_local_hour(row) as usize;
                    data.tools += 1;
                    data.hourly.tools[hour] += 1;
                }
            }
        }

        // ── Focus from message timestamps grouped by session ──
        if let Ok(mut stmt) = conn.prepare(
            "SELECT session_id, time_created FROM message \
             WHERE json_extract(data, '$.role') = 'assistant' \
               AND time_created >= ? AND time_created < ?",
        ) {
            let mut sessions: Sessions = HashMap::new();
            let rows = stmt.query_map(rusqlite::params![start_ms, end_ms], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            });
            if let Ok(rows) = rows {
                for row in rows.flatten() {
                    sessions.entry(row.0).or_default().push(row.1);
                }
            }
            let merged = sessions_to_merged_blocks(&sessions);
            data.focus_blocks = merged.clone();
            data.focus_min = interval_ms_in_range(&merged, start_ms, end_ms) as f64 / 60_000.0;
        }

        data
    }
}
