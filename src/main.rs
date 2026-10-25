// Release builds are a pure tray app: no console window. Debug builds keep the
// console so logs are visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("claude-sync-tray is a Windows-only application.");

#[macro_use]
mod logging;
mod claude;
mod config;
mod dither;
mod icon;
mod logo;
mod paths;
mod process;
mod profiles;
mod sessions;
mod transfer;
mod startup;
mod tray;
mod ui;

use config::Config;
use profiles::ProfileManager;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "/?")
    {
        print_help();
        return;
    }

    // `--switch N` takes a 1-based account number; `switch_index` is zero-based.
    let switch_index = match parse_switch(&args) {
        Ok(index) => index,
        Err(message) => {
            process::message_box("ClaudeSyncTray", &message);
            std::process::exit(2);
        }
    };
    let start_hidden = args
        .iter()
        .any(|arg| arg == "--tray" || arg == "--minimized" || arg == "--hidden");

    let _ = std::fs::create_dir_all(paths::app_root());
    logging::init(&paths::log_file());
    log_info!("claude-sync-tray v{VERSION} starting; args={args:?}");

    let guard = match process::acquire_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            if let Some(index) = switch_index {
                match process::send_command(&format!("switch {index}")) {
                    Ok(()) => log_info!("handed switch command to the running tray instance"),
                    Err(error) => log_error!("could not hand off switch command: {error}"),
                }
            } else {
                let _ = process::send_command("show");
                log_info!("another instance is already running; requested dashboard focus");
            }
            return;
        }
        Err(error) => {
            log_error!("could not acquire the single-instance lock: {error}");
            return;
        }
    };

    if let Some(index) = switch_index {
        let code = tray::headless_switch(index);
        drop(guard);
        std::process::exit(code);
    }

    let cfg = Config::load_or_default();

    // Re-apply the Run entry every launch so a moved executable never leaves a
    // stale path behind, and so disabling it is honoured.
    if let Err(error) = startup::set_enabled(cfg.start_with_windows) {
        log_warn!("could not sync the Windows startup entry: {error}");
    }

    let manager = match ProfileManager::new(&cfg) {
        Ok(manager) => manager,
        Err(error) => {
            log_error!("could not initialise profile manager: {error}");
            process::message_box(
                "ClaudeSyncTray",
                &format!("Could not initialise profile storage:\n{error}"),
            );
            std::process::exit(1);
        }
    };

    // Adopting a pre-existing real `%APPDATA%\Claude` must never happen while
    // Claude is running; defer that to the next switch instead.
    if process::find_claude_desktop().is_empty() {
        if let Err(error) = manager.ensure_live_linked(true) {
            log_warn!("could not prepare the profile link: {error}");
        }
    } else {
        log_info!("Claude is running; leaving the data directory as-is until the next switch");
    }

    ui::run(cfg, manager, start_hidden, guard);
}

/// Zero-based profile index requested by `--switch N` (N is 1-based on the command line).
fn parse_switch(args: &[String]) -> Result<Option<usize>, String> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let value = if arg == "--switch" || arg == "-s" {
            index += 1;
            Some(args.get(index).cloned().unwrap_or_default())
        } else {
            arg.strip_prefix("--switch=").map(str::to_string)
        };
        if let Some(value) = value {
            return match value.trim().parse::<usize>() {
                Ok(number) if number >= 1 => Ok(Some(number - 1)),
                _ => Err(format!(
                    "--switch expects an account number starting at 1 (got '{}').",
                    value.trim()
                )),
            };
        }
        index += 1;
    }
    Ok(None)
}

fn print_help() {
    println!(
        "ClaudeSyncTray {VERSION}\n\
         GPUI desktop dashboard & system-tray profile switcher for Claude Desktop.\n\n\
         USAGE:\n\
         \x20 claude-sync-tray                 Open dashboard & run system tray (single instance).\n\
         \x20 claude-sync-tray --tray          Start hidden in the system tray.\n\
         \x20 claude-sync-tray --switch <N>    Switch to account N (1-based) and exit.\n\
         \x20 claude-sync-tray --help          Show this help.\n\n\
         PROFILES:\n\
         \x20 {}\\\n\
         \x20 Manage profiles directly in the GPUI dashboard or edit config.json.\n",
        paths::profiles_root().display()
    );
}

#[cfg(test)]
mod tests {
    use super::parse_switch;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn switch_numbers_are_one_based_on_the_command_line() {
        assert_eq!(parse_switch(&args(&["--switch", "1"])), Ok(Some(0)));
        assert_eq!(parse_switch(&args(&["--switch=3"])), Ok(Some(2)));
        assert_eq!(parse_switch(&args(&["--tray", "-s", "2"])), Ok(Some(1)));
    }

    #[test]
    fn switch_is_optional() {
        assert_eq!(parse_switch(&args(&["--tray"])), Ok(None));
        assert_eq!(parse_switch(&args(&[])), Ok(None));
    }

    #[test]
    fn invalid_switch_values_are_rejected() {
        assert!(parse_switch(&args(&["--switch", "0"])).is_err());
        assert!(parse_switch(&args(&["--switch", "abc"])).is_err());
        assert!(parse_switch(&args(&["--switch"])).is_err());
    }
}
