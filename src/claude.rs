//! Locating Claude Desktop: its data directory, its executable, and how to start it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::{paths, process};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveKind {
    /// `%APPDATA%\Claude`
    Classic,
    /// `%LOCALAPPDATA%\Packages\<package family name>\LocalCache\Roaming\Claude`
    Packaged { package_family: String },
    /// User override.
    Override,
}

#[derive(Debug, Clone)]
pub enum LaunchTarget {
    Exe(PathBuf),
    /// `shell:AppsFolder\<AUMID>` for MSIX/Store installs.
    AppsFolder(String),
    Shell(String),
}

fn dir_mtime(path: &PathBuf) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Packaged (MSIX/Store) Claude data directories that actually exist.
fn packaged_live_dirs() -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let Some(base) = paths::local_app_data().map(|p| p.join("Packages")) else {
        return found;
    };
    let Ok(entries) = fs::read_dir(&base) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_ascii_lowercase().starts_with("claude_") {
            continue;
        }
        let live = entry
            .path()
            .join("LocalCache")
            .join("Roaming")
            .join("Claude");
        if live.is_dir() {
            found.push((live, name));
        }
    }
    found
}

/// Picks the Claude data directory that is most likely the live one.
///
/// Prefers whichever candidate was written to most recently, which correctly
/// handles a leftover Store package directory after the classic build is used.
pub fn resolve_live_dir(override_path: Option<&str>) -> (PathBuf, LiveKind) {
    if let Some(path) = override_path {
        if !path.trim().is_empty() {
            return (PathBuf::from(path), LiveKind::Override);
        }
    }

    let classic = paths::claude_roaming_dir();
    let mut candidates: Vec<(PathBuf, LiveKind, SystemTime)> = Vec::new();
    if classic.is_dir() {
        candidates.push((classic.clone(), LiveKind::Classic, dir_mtime(&classic)));
    }
    for (dir, package_family) in packaged_live_dirs() {
        let mtime = dir_mtime(&dir);
        candidates.push((dir, LiveKind::Packaged { package_family }, mtime));
    }

    if candidates.is_empty() {
        log_info!(
            "no existing Claude data directory found; defaulting to {}",
            classic.display()
        );
        return (classic, LiveKind::Classic);
    }

    candidates.sort_by_key(|(_, _, mtime)| *mtime);
    let (dir, kind, _) = candidates.pop().expect("candidates is non-empty");
    log_info!("using Claude data directory {} ({:?})", dir.display(), kind);
    (dir, kind)
}

fn version_key(version: &str) -> u64 {
    let mut key = 0u64;
    for (i, part) in version.split(['.', '-', '+']).take(3).enumerate() {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        let value: u64 = digits.parse().unwrap_or(0).min(999);
        key += value * 1_000_000u64.pow(2u32.saturating_sub(i as u32));
    }
    key
}

fn registry_app_path() -> Option<PathBuf> {
    for exe in ["claude.exe", "Claude.exe"] {
        let subkey = format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
        if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(&subkey) {
            if let Ok(value) = key.get_value::<String, _>("") {
                let path = PathBuf::from(value);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Best-effort discovery of the Claude Desktop executable.
pub fn discover_exe() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(running) = process::running_desktop_image_path() {
        candidates.push(running);
    }

    if let Some(local) = paths::local_app_data() {
        let install_root = local.join("AnthropicClaude");
        if let Ok(entries) = fs::read_dir(&install_root) {
            let mut versions: Vec<(u64, PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(version) = name.strip_prefix("app-") {
                    let exe = entry.path().join("claude.exe");
                    if exe.is_file() {
                        versions.push((version_key(version), exe));
                    }
                }
            }
            versions.sort();
            if let Some((_, exe)) = versions.pop() {
                candidates.push(exe);
            }
        }
        for relative in [
            r"AnthropicClaude\claude.exe",
            r"AnthropicClaude\Claude.exe",
            r"Programs\Claude\Claude.exe",
            r"Programs\claude\claude.exe",
        ] {
            let path = local.join(relative);
            if path.is_file() {
                candidates.push(path);
            }
        }
    }

    if let Some(program_files) = paths::program_files() {
        for relative in [r"Claude\Claude.exe", r"Claude\claude.exe"] {
            let path = program_files.join(relative);
            if path.is_file() {
                candidates.push(path);
            }
        }
    }

    if let Some(path) = registry_app_path() {
        candidates.push(path);
    }

    let chosen = candidates.into_iter().find(|p| p.is_file());
    match &chosen {
        Some(path) => log_info!("Claude executable: {}", path.display()),
        None => log_warn!("could not locate claude.exe; configure `launch.exe` in config.json"),
    }
    chosen
}

pub fn resolve_launch_target(cfg: &crate::config::Config, kind: &LiveKind) -> LaunchTarget {
    if let Some(launch) = &cfg.launch {
        if let Some(command) = launch.command.as_ref().filter(|c| !c.trim().is_empty()) {
            return LaunchTarget::Shell(command.clone());
        }
        if let Some(exe) = launch.exe.as_ref().filter(|c| !c.trim().is_empty()) {
            return LaunchTarget::Exe(PathBuf::from(exe));
        }
    }
    // A Store app started from its raw exe runs without package identity, so its
    // AppData writes bypass the packaged Roaming folder that the junction replaces.
    // Always go through the AppsFolder id for packaged installs.
    if let LiveKind::Packaged { package_family } = kind {
        return LaunchTarget::AppsFolder(format!("{package_family}!Claude"));
    }
    if let Some(exe) = discover_exe() {
        return LaunchTarget::Exe(exe);
    }
    LaunchTarget::Exe(
        paths::local_app_data()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AnthropicClaude")
            .join("claude.exe"),
    )
}

pub fn launch(target: &LaunchTarget) -> std::io::Result<()> {
    match target {
        LaunchTarget::Exe(exe) => {
            if !exe.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Claude executable not found at {}", exe.display()),
                ));
            }
            Command::new(exe).spawn().map(|_| ())
        }
        LaunchTarget::AppsFolder(aumid) => {
            log_info!("launching packaged Claude via {aumid}");
            process::spawn_detached({
                let mut cmd = Command::new("explorer.exe");
                cmd.arg(format!("shell:AppsFolder\\{aumid}"));
                cmd
            })
        }
        LaunchTarget::Shell(command) => {
            log_info!("launching Claude via command: {command}");
            process::spawn_detached({
                let mut cmd = Command::new("cmd.exe");
                cmd.args(["/C", command]);
                cmd
            })
        }
    }
}

/// Launches a separate Claude Desktop instance isolated to `profile_dir` via `--user-data-dir`.
/// This allows running multiple Claude accounts side-by-side without closing the active session.
pub fn launch_with_profile(
    target: &LaunchTarget,
    profile_dir: &std::path::Path,
) -> std::io::Result<()> {
    let exe = match target {
        LaunchTarget::Exe(exe) if exe.is_file() => exe.clone(),
        _ => discover_exe().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Side-by-side launch requires a direct claude.exe path (configure `launch.exe` in Settings)",
            )
        })?,
    };
    let user_data_arg = format!("--user-data-dir={}", profile_dir.display());
    log_info!(
        "launching side-by-side Claude at {} with {}",
        exe.display(),
        user_data_arg
    );
    Command::new(&exe).arg(user_data_arg).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::version_key;

    #[test]
    fn versions_sort_numerically_not_lexically() {
        assert!(version_key("1.10.0") > version_key("1.9.9"));
        assert!(version_key("1.2.3") > version_key("1.2.2"));
        assert!(version_key("2.0.0") > version_key("1.99.99"));
        assert_eq!(version_key("1.2.3"), version_key("1.2.3"));
    }
}
