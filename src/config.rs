use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// Config directory — ~/Library/Application Support/VibeCodingRings/
fn config_dir() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("VibeCodingRings")
}

fn config_file() -> PathBuf {
    // In development mode, use local config.json
    if std::env::var("VIBE_DEV").is_ok() {
        PathBuf::from("config.json")
    } else {
        config_dir().join("config.json")
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

// Agent data directories
pub fn claude_dir() -> PathBuf {
    home_dir().join(".claude")
}

pub fn codex_dir() -> PathBuf {
    home_dir().join(".codex")
}

pub fn gemini_dir() -> PathBuf {
    home_dir().join(".gemini")
}

pub fn opencode_dir() -> PathBuf {
    home_dir().join(".local/share/opencode")
}

pub fn opencode_db() -> PathBuf {
    opencode_dir().join("opencode.db")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goals {
    #[serde(default = "default_tokens")]
    pub tokens: u64,
    #[serde(default = "default_focus_min")]
    pub focus_min: u64,
    #[serde(default = "default_tool_calls")]
    pub tool_calls: u64,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default = "default_agents")]
    pub enabled_agents: Vec<String>,
    #[serde(default)]
    pub streak_cache: Option<StreakCache>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreakCache {
    pub base_streak: u32,      // 昨天结束时的连续达标天数
    pub last_date: String,     // 最后计算的日期 (YYYY-MM-DD)
}

fn default_tokens() -> u64 { 1_000_000 }
fn default_focus_min() -> u64 { 120 }
fn default_tool_calls() -> u64 { 50 }
fn default_lang() -> String { "en".to_string() }
fn default_agents() -> Vec<String> { vec!["claude_code".to_string()] }

impl Default for Goals {
    fn default() -> Self {
        Goals {
            tokens: default_tokens(),
            focus_min: default_focus_min(),
            tool_calls: default_tool_calls(),
            lang: default_lang(),
            enabled_agents: default_agents(),
            streak_cache: None,
        }
    }
}

pub fn load_config() -> Goals {
    let path = config_file();
    if path.exists() {
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(goals) = serde_json::from_str(&data) {
                return goals;
            }
        }
    }
    Goals::default()
}

pub fn save_config(goals: &Goals) {
    let path = config_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(goals) {
        let _ = fs::write(&path, json);
    }
}
