//! Where everything lives on disk.

use std::path::{Path, PathBuf};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

pub fn app_data() -> Option<PathBuf> {
    env_path("APPDATA")
}

pub fn local_app_data() -> Option<PathBuf> {
    env_path("LOCALAPPDATA")
}

pub fn program_files() -> Option<PathBuf> {
    env_path("ProgramFiles").or_else(|| env_path("ProgramW6432"))
}

pub fn user_profile() -> Option<PathBuf> {
    if let Some(home) = env_path("USERPROFILE") {
        return Some(home);
    }
    match (env_path("HOMEDRIVE"), env_path("HOMEPATH")) {
        (Some(drive), Some(path)) => {
            Some(drive.join(path.to_string_lossy().trim_start_matches(['\\', '/'])))
        }
        _ => None,
    }
}

/// `%APPDATA%\ClaudeSyncTray` — our own state, deliberately a sibling of `%APPDATA%\Claude`.
pub fn app_root() -> PathBuf {
    app_data()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ClaudeSyncTray")
}

pub fn profiles_root() -> PathBuf {
    app_root().join("profiles")
}

/// One copy of the folders every account would otherwise duplicate (the Linux sandbox).
pub fn shared_root() -> PathBuf {
    app_root().join("shared")
}

pub fn backups_root() -> PathBuf {
    app_root().join("backups")
}

pub fn config_file() -> PathBuf {
    app_root().join("config.json")
}

pub fn log_file() -> PathBuf {
    app_root().join("claude-sync-tray.log")
}

pub fn command_file() -> PathBuf {
    app_root().join("command.txt")
}

pub fn instance_lock_file() -> PathBuf {
    app_root().join("instance.lock")
}

/// Shared `claude_desktop_config.json` propagated across all profiles when enabled.
pub fn shared_mcp_config_file() -> PathBuf {
    app_root().join("shared_claude_desktop_config.json")
}

/// Classic (non-packaged) Claude Desktop data directory.
pub fn claude_roaming_dir() -> PathBuf {
    app_data().unwrap_or_else(app_root).join("Claude")
}

/// `~/.claude/projects` — Claude Code project transcripts.
pub fn claude_code_projects() -> Option<PathBuf> {
    user_profile().map(|home| home.join(".claude").join("projects"))
}

/// `~/.claude/.credentials.json` — Claude Code CLI OAuth token storage.
pub fn claude_code_credentials_file() -> Option<PathBuf> {
    user_profile().map(|home| home.join(".claude").join(".credentials.json"))
}

/// `~/.claude.json` — Claude Code CLI root state (contains `oauthAccount`, `userID`, etc.).
pub fn claude_code_state_file() -> Option<PathBuf> {
    user_profile().map(|home| home.join(".claude.json"))
}

/// Case-insensitive path equality with `\\?\` prefixes and separators normalised.
pub fn same_path(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(p: &Path) -> String {
    let raw = p.to_string_lossy();
    let trimmed = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    trimmed
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::same_path;
    use std::path::Path;

    #[test]
    fn paths_compare_case_insensitively_and_ignore_prefix() {
        assert!(same_path(
            Path::new(r"C:\Users\X\Claude"),
            Path::new(r"c:/users/x/claude\")
        ));
        assert!(same_path(Path::new(r"\\?\C:\a"), Path::new(r"C:\a")));
        assert!(!same_path(Path::new(r"C:\a"), Path::new(r"C:\b")));
    }
}
