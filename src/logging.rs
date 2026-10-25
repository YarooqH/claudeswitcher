//! Tiny dependency-free logger that writes to a rolling-ish text file.
//!
//! A tray app has no console (release builds use the `windows` subsystem), so
//! the log file is the only way to see what happened.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static SINK: OnceLock<Mutex<File>> = OnceLock::new();

/// Opens (creating if needed) the log file. Safe to call once; later calls win.
pub fn init(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = SINK.set(Mutex::new(file));
    }
    log("INFO", "---- log started ----");
}

pub fn log(level: &str, message: &str) {
    let line = format!("{} [{}] {}\n", timestamp(), level, message);
    if let Some(sink) = SINK.get() {
        if let Ok(mut file) = sink.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
    // Debug builds keep a console, so mirror there too.
    #[cfg(debug_assertions)]
    eprint!("{line}");
}

/// Returns the last `max_lines` entries from the log file for the UI log viewer.
pub fn recent_lines(max_lines: usize) -> Vec<String> {
    let path = crate::paths::log_file();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let all: Vec<&str> = raw.lines().collect();
    let start = all.len().saturating_sub(max_lines);
    all[start..].iter().map(|line| (*line).to_string()).collect()
}

/// Truncates the log file and writes a fresh header entry.
pub fn clear() {
    let path = crate::paths::log_file();
    let _ = std::fs::write(&path, b"");
    log("INFO", "---- log cleared ----");
}

/// `YYYY-MM-DD HH:MM:SS UTC` without pulling in a date crate.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year,
        month,
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::log("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::log("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::log("ERROR", &format!($($arg)*)) };
}
