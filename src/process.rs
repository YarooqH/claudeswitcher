//! Windows process control, single-instance locking and shell helpers.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    CREATE_NO_WINDOW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsIconic, IsWindowVisible, MessageBoxW, PostMessageW,
    SetForegroundWindow, ShowWindow, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, SW_HIDE,
    SW_RESTORE, SW_SHOW, WM_CLOSE,
};

/// `SYNCHRONIZE` is only exported by `windows-sys` under the FileSystem feature,
/// but it is a valid process access right for `WaitForSingleObject`.
const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

const CLAUDE_EXE_NAMES: &[&str] = &["claude.exe"];

#[derive(Debug, Default, Clone, Copy)]
pub struct KillReport {
    pub found: usize,
    pub graceful: usize,
    pub forced: usize,
}

pub struct InstanceGuard {
    _file: File,
}

/// Exclusive lock held for the process lifetime. Returns `None` when another
/// instance already owns it.
pub fn acquire_instance() -> io::Result<Option<InstanceGuard>> {
    let path = crate::paths::instance_lock_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceGuard { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Writes a command for the running tray instance to pick up.
pub fn send_command(text: &str) -> io::Result<()> {
    let path = crate::paths::command_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, text)?;
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp, &path)
}

/// Reads and consumes a pending command file.
pub fn take_command() -> Option<String> {
    let path = crate::paths::command_file();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(text)
}

fn utf16_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// All PIDs whose image name matches (case-insensitively) one of `names`.
fn find_processes(names: &[&str]) -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let snapshot: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return pids;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = utf16_to_string(&entry.szExeFile);
                if entry.th32ProcessID != 0 && names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    pids
}

struct EnumContext {
    pid: u32,
}

unsafe extern "system" fn enum_windows_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
    let ctx = &*(lparam as *const EnumContext);
    let mut window_pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut window_pid);
    if window_pid == ctx.pid {
        // Ask politely; Electron/Claude flushes its session store on WM_CLOSE.
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
    1
}

fn request_close(pid: u32) {
    let ctx = EnumContext { pid };
    unsafe {
        EnumWindows(Some(enum_windows_cb), &ctx as *const EnumContext as LPARAM);
    }
}

fn is_alive(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE_ACCESS, 0, pid);
        if handle.is_null() {
            return false;
        }
        let result = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        result == WAIT_TIMEOUT
    }
}

fn force_kill(pid: u32) {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

fn image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32_768];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..len as usize],
        )))
    }
}

/// True when `dir` (an executable's folder) looks like an Electron app install.
fn is_desktop_dir(dir: &Path) -> bool {
    dir.join("resources").is_dir()
}

/// PIDs of Claude *Desktop* processes only.
///
/// The Claude Code CLI is also called `claude.exe`, so the image name alone
/// cannot tell them apart. Desktop is an Electron app and always has a
/// `resources` directory next to its executable; the CLI is a standalone binary.
pub fn find_claude_desktop() -> Vec<u32> {
    let mut checked_dirs: HashMap<PathBuf, bool> = HashMap::new();
    find_processes(CLAUDE_EXE_NAMES)
        .into_iter()
        .filter(|&pid| {
            let Some(path) = image_path(pid) else {
                return false;
            };
            let Some(dir) = path.parent() else {
                return false;
            };
            *checked_dirs
                .entry(dir.to_path_buf())
                .or_insert_with(|| is_desktop_dir(dir))
        })
        .collect()
}

/// Executable path of a running Claude Desktop process.
pub fn running_desktop_image_path() -> Option<PathBuf> {
    find_claude_desktop().into_iter().find_map(image_path)
}

/// Gracefully closes every Claude Desktop process, escalating to a hard kill
/// after `grace`. Claude Code CLI sessions are left alone. Returns a summary so
/// the caller can log/report it.
pub fn terminate_claude(grace: Duration) -> KillReport {
    let pids = find_claude_desktop();
    let mut report = KillReport {
        found: pids.len(),
        ..Default::default()
    };
    if pids.is_empty() {
        return report;
    }

    log_info!("closing {} Claude process(es): {:?}", pids.len(), pids);
    for &pid in &pids {
        request_close(pid);
    }

    let deadline = Instant::now() + grace;
    let mut remaining: Vec<u32> = pids.clone();
    while !remaining.is_empty() {
        remaining.retain(|&pid| is_alive(pid));
        if remaining.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    for &pid in &remaining {
        log_warn!("forcing termination of Claude pid {pid}");
        force_kill(pid);
    }
    report.forced = remaining.len();
    report.graceful = report.found - report.forced;

    // Give the OS a moment to release file handles after a hard kill.
    let settle_deadline = Instant::now() + Duration::from_millis(1500);
    let mut alive: Vec<u32> = remaining.clone();
    while !alive.is_empty() && Instant::now() < settle_deadline {
        alive.retain(|&pid| is_alive(pid));
        if !alive.is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    report
}

/// Opens a directory in Explorer, creating it first when missing.
pub fn open_folder(path: &Path) -> io::Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    let normalized = path.to_string_lossy().replace('/', "\\");
    let mut cmd = Command::new("explorer.exe");
    cmd.arg(&normalized);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().map(|_| ())
}

/// Detached spawn that never flashes a console window.
pub fn spawn_detached(mut cmd: Command) -> io::Result<()> {
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().map(|_| ())
}

pub fn message_box(title: &str, text: &str) {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let text = wide(text);
    let title = wide(title);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

pub fn open_path(path: &Path) {
    if path.is_dir() {
        if let Err(error) = open_folder(path) {
            log_warn!("could not open {}: {error}", path.display());
        }
        return;
    }

    let command = if path.exists() {
        let normalized = path.to_string_lossy().replace('/', "\\");
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "start", "", &normalized]);
        command
    } else if let Some(parent) = path.parent() {
        let normalized = parent.to_string_lossy().replace('/', "\\");
        let mut command = Command::new("explorer.exe");
        command.arg(&normalized);
        command
    } else {
        return;
    };
    let _ = spawn_detached(command);
}

fn window_hwnd(window: &gpui::Window) -> Option<HWND> {
    let raw = HasWindowHandle::window_handle(window).ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

pub fn hide_window(window: &gpui::Window) {
    if let Some(hwnd) = window_hwnd(window) {
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

pub fn show_window(window: &gpui::Window) {
    if let Some(hwnd) = window_hwnd(window) {
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            } else {
                ShowWindow(hwnd, SW_SHOW);
            }
            SetForegroundWindow(hwnd);
        }
        window.activate_window();
    }
}

/// True when the window is on screen (not hidden to the tray or minimised).
pub fn window_visible(window: &gpui::Window) -> bool {
    window_hwnd(window).is_some_and(|hwnd| unsafe {
        IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0
    })
}

pub fn toggle_window(window: &gpui::Window) {
    if let Some(hwnd) = window_hwnd(window) {
        let visible = unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 };
        if visible {
            hide_window(window);
        } else {
            show_window(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_desktop_dir;
    use std::fs;

    #[test]
    fn electron_install_is_desktop_but_standalone_cli_is_not() {
        let base = std::env::temp_dir().join(format!("cst-{}-desktop", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let desktop = base.join("app");
        let cli = base.join("bin");
        fs::create_dir_all(desktop.join("resources")).unwrap();
        fs::create_dir_all(&cli).unwrap();
        fs::write(cli.join("claude.exe"), b"").unwrap();

        assert!(is_desktop_dir(&desktop));
        assert!(!is_desktop_dir(&cli));
        let _ = fs::remove_dir_all(&base);
    }
}
