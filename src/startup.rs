//! "Start with Windows" via the per-user Run key.

use std::io;
use std::path::{Path, PathBuf};

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

use crate::paths;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "ClaudeSyncTray";

pub fn exe_path() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// Run-key command: logon should land in the tray, not pop the dashboard open.
fn run_command(exe: &Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

/// Executable path inside a stored Run-key command, with or without quotes/arguments.
fn stored_exe(command: &str) -> &str {
    let raw = command.trim();
    if let Some(rest) = raw.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        raw.split(" --").next().unwrap_or(raw).trim()
    }
}

fn run_key(access: u32) -> io::Result<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.create_subkey_with_flags(RUN_KEY, access)
        .map(|(key, _)| key)
}

pub fn stored_command() -> Option<String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .ok()?;
    key.get_value::<String, _>(VALUE_NAME).ok()
}

/// True when the Run entry exists *and* points at this executable.
pub fn is_enabled() -> bool {
    let Some(stored) = stored_command() else {
        return false;
    };
    let Some(current) = exe_path().ok() else {
        return false;
    };
    paths::same_path(Path::new(stored_exe(&stored)), &current)
}

/// Adds or removes the Run entry. Kept in sync on every launch so a moved
/// executable never leaves a stale registry value behind.
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let key = run_key(KEY_READ | KEY_WRITE)?;
    if enabled {
        let command = run_command(&exe_path()?);
        key.set_value(VALUE_NAME, &command)?;
        log_info!("autostart enabled: {command}");
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => log_info!("autostart disabled"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                log_warn!("could not remove autostart entry: {error}");
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_command, stored_exe};
    use std::path::Path;

    #[test]
    fn run_command_starts_hidden_in_the_tray() {
        let command = run_command(Path::new(r"C:\Program Files\Tray\claude-sync-tray.exe"));
        assert_eq!(
            command,
            r#""C:\Program Files\Tray\claude-sync-tray.exe" --tray"#
        );
    }

    #[test]
    fn stored_exe_handles_quoted_unquoted_and_legacy_entries() {
        let exe = r"C:\Program Files\Tray\claude-sync-tray.exe";
        assert_eq!(stored_exe(&run_command(Path::new(exe))), exe);
        // Entries written before `--tray` was added.
        assert_eq!(stored_exe(&format!("\"{exe}\"")), exe);
        assert_eq!(stored_exe(r"C:\Tray\app.exe --tray"), r"C:\Tray\app.exe");
    }
}
