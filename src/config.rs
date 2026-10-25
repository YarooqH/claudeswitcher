//! Persisted user settings.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    /// Absolute, or relative to `%APPDATA%\ClaudeSyncTray`.
    pub dir: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// Explicit path to `claude.exe`. Skips auto-detection when set.
    #[serde(default)]
    pub exe: Option<String>,
    /// Arbitrary shell command, e.g. `explorer.exe shell:AppsFolder<AUMID>`.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Index into `profiles`. Only a fallback: the junction target wins on load.
    #[serde(default)]
    pub active: Option<usize>,
    #[serde(default)]
    pub profiles: Vec<ProfileConfig>,
    /// Override the detected Claude Desktop data directory.
    #[serde(default)]
    pub live_override: Option<String>,
    #[serde(default)]
    pub launch: Option<LaunchConfig>,
    /// Mirrors the HKCU Run key; re-applied on startup so a moved exe stays correct.
    #[serde(default)]
    pub start_with_windows: bool,
    /// Keeps `claude_desktop_config.json` (MCP servers & app settings) synced across all profiles.
    #[serde(default = "default_true")]
    pub share_mcp_config: bool,
    /// Snapshots and swaps Claude Code CLI (`~/.claude/.credentials.json` & `oauthAccount`) per profile.
    #[serde(default = "default_true")]
    pub sync_claude_code: bool,
    /// Mirrors Claude Desktop's local Code session list into every account, so each login shows
    /// the same history. Off by default: it writes into account folders other than the active one.
    #[serde(default)]
    pub share_sessions: bool,
}

fn default_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            active: Some(0),
            profiles: vec![
                ProfileConfig {
                    name: "Account 1".into(),
                    dir: r"profiles\account_1".into(),
                },
                ProfileConfig {
                    name: "Account 2".into(),
                    dir: r"profiles\account_2".into(),
                },
                ProfileConfig {
                    name: "Account 3".into(),
                    dir: r"profiles\account_3".into(),
                },
            ],
            live_override: None,
            launch: None,
            start_with_windows: false,
            share_mcp_config: true,
            sync_claude_code: true,
            share_sessions: false,
        }
    }
}

impl Config {
    pub fn load_or_default() -> Self {
        let path = paths::config_file();
        let mut cfg = match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Config>(&raw) {
                Ok(cfg) => cfg,
                Err(e) => {
                    log_warn!("config.json is invalid ({e}); using defaults");
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        if cfg.profiles.is_empty() {
            cfg.profiles = Config::default().profiles;
        }
        cfg
    }

    pub fn save(&self) {
        let path = paths::config_file();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                let tmp = path.with_extension("json.tmp");
                if fs::write(&tmp, json).is_ok() {
                    let _ = fs::remove_file(&path);
                    if let Err(e) = fs::rename(&tmp, &path) {
                        log_warn!("could not persist config: {e}");
                    }
                }
            }
            Err(e) => log_warn!("could not serialise config: {e}"),
        }
    }

    /// Resolves a profile directory against the app root.
    pub fn resolve_dir(&self, profile: &ProfileConfig) -> PathBuf {
        let normalized = profile.dir.replace('/', "\\");
        let raw = PathBuf::from(normalized);
        if raw.is_absolute() {
            raw
        } else {
            paths::app_root().join(raw)
        }
    }
}
