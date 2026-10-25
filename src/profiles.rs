//! Isolated profile directories and the atomic profile swap.
//!
//! Design: every account is a real directory under
//! `%APPDATA%\ClaudeSyncTray\profiles\`, and Claude Desktop's live data path is
//! an NTFS **junction** pointing at the active profile. Switching is therefore
//! a junction re-point (instant, no data copy); all writes Claude makes end up
//! in the active profile automatically.
//!
//! The only time data moves is the first run against an *existing* real
//! `%APPDATA%\Claude`, which gets adopted into the active profile.

use std::fs;
use std::io;
use std::os::windows::fs::{symlink_dir, symlink_file};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude::{self, LaunchTarget};
use crate::config::Config;
use crate::paths;
use crate::process::{self, KillReport};
use crate::sessions;

/// `ERROR_ACCESS_DENIED` / `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`.
/// Anthropic's Electron app keeps these open while flushing session JSON.
fn is_transient(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

const RETRY_DELAYS_MS: &[u64] = &[50, 100, 200, 400, 800, 1200, 2000, 3000];

pub(crate) fn with_retry<T>(label: &str, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut last: Option<io::Error> = None;
    for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
        match op() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_transient(&error) {
                    return Err(error);
                }
                log_warn!("{label}: transient lock (attempt {}): {error}", attempt + 1);
                last = Some(error);
                std::thread::sleep(Duration::from_millis(*delay));
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other(format!("{label} failed after retries"))))
}

fn friendly(context: &str, error: &io::Error) -> String {
    if is_transient(error) {
        format!(
            "{context}: Claude Desktop still has files open ({error}).\n\
             Quit Claude completely, then try the switch again."
        )
    } else {
        format!("{context}: {error}")
    }
}

fn is_dir_empty(path: &Path) -> io::Result<bool> {
    Ok(fs::read_dir(path)?.next().is_none())
}

/// True when `dir` holds more than the `claude_desktop_config.json` that MCP sharing
/// writes into every profile, i.e. it carries a real session worth protecting.
fn holds_account_data(dir: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(dir)? {
        if entry?.file_name() != "claude_desktop_config.json" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        with_retry("remove directory", || fs::remove_dir_all(path))
    } else {
        with_retry("remove file", || fs::remove_file(path))
    }
}

pub(crate) fn copy_dir_recursive(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            match fs::read_link(&from) {
                Ok(target) => {
                    let _ = symlink_dir(&target, &to).or_else(|_| symlink_file(&target, &to));
                }
                Err(_) => {
                    with_retry("copy file", || fs::copy(&from, &to))?;
                }
            }
        } else if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            with_retry("copy file", || fs::copy(&from, &to))?;
        }
    }
    Ok(())
}

fn move_entry(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(from)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        copy_dir_recursive(from, to)?;
        with_retry("remove source directory", || fs::remove_dir_all(from))
    } else {
        with_retry("copy file", || fs::copy(from, to))?;
        with_retry("remove source file", || fs::remove_file(from))
    }
}

/// Moves everything from `source` into `destination`.
///
/// On name conflicts `prefer_source` decides the winner: adoption of the live
/// Claude directory uses `true` (the data the user is actually using wins),
/// while every other merge keeps the profile as the source of truth.
fn merge_dir_contents(source: &Path, destination: &Path, prefer_source: bool) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let is_dir = entry.file_type()?.is_dir();
        if to.exists() {
            if is_dir && to.is_dir() {
                merge_dir_contents(&from, &to, prefer_source)?;
                with_retry("remove merged directory", || fs::remove_dir(&from))?;
            } else if prefer_source {
                log_warn!(
                    "replacing {} with the live copy {}",
                    to.display(),
                    from.display()
                );
                remove_entry(&to)?;
                move_entry(&from, &to)?;
            } else {
                log_warn!(
                    "keeping existing {} and discarding unmanaged {}",
                    to.display(),
                    from.display()
                );
                remove_entry(&from)?;
            }
        } else {
            move_entry(&from, &to)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
}

impl Profile {
    pub fn has_session_data(&self) -> bool {
        !is_dir_empty(&self.dir).unwrap_or(true)
    }

    /// True if Electron cookie/storage databases exist for Claude Desktop in this profile.
    pub fn has_desktop_auth(&self) -> bool {
        self.dir.join("Network").join("Cookies").is_file()
            || self.dir.join("Cookies").is_file()
            || self.dir.join("Local Storage").is_dir()
    }

    /// True if a Claude Code CLI credential snapshot exists for this profile.
    pub fn has_cli_auth(&self) -> bool {
        self.dir
            .join("claude_code_auth")
            .join(".credentials.json")
            .is_file()
    }

    /// The account Claude Desktop is signed in to in this profile. Desktop rewrites
    /// `lastKnownAccountUuid` on every login, so it follows a logout/login done inside the app.
    pub fn desktop_account_id(&self) -> Option<String> {
        let raw = fs::read_to_string(self.dir.join("config.json")).ok()?;
        let config: serde_json::Value = serde_json::from_str(&raw).ok()?;
        config
            .get("lastKnownAccountUuid")?
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }

    /// Short form of `desktop_account_id` for places that have no email to show.
    pub fn desktop_account_short(&self) -> Option<String> {
        self.desktop_account_id()
            .map(|id| id.chars().take(8).collect())
    }

    /// The account recorded in this profile's Claude Code CLI snapshot.
    pub fn cli_account(&self) -> Option<CliAccount> {
        let raw = fs::read_to_string(
            self.dir
                .join("claude_code_auth")
                .join("oauth_account.json"),
        )
        .ok()?;
        parse_cli_account(&serde_json::from_str(&raw).ok()?)
    }

    /// The Claude Code login that names this profile's account: its saved snapshot, or `live`
    /// (the login in `~/.claude.json` right now, only meaningful for the active profile) when
    /// the snapshot is missing or belongs to another account and `live` matches the account
    /// Desktop is signed in to. A snapshot is only taken when switching away from a profile, so
    /// without this a freshly signed-in account would have no email until then.
    pub fn identity(&self, live: Option<&CliAccount>) -> Option<CliAccount> {
        let saved = self.cli_account();
        let Some(desktop) = self.desktop_account_id() else {
            return saved;
        };
        let names_desktop =
            |account: &CliAccount| account.account_uuid.as_deref() == Some(desktop.as_str());
        if saved.as_ref().is_some_and(names_desktop) {
            return saved;
        }
        live.filter(|account| names_desktop(account)).cloned().or(saved)
    }

    /// The email of the account Desktop is signed in to, when it is known. The CLI snapshot is
    /// the only place an email is stored, and it is taken from `~/.claude.json`, which does not
    /// change when Desktop signs in to another account; so it only counts while it names the
    /// same account Desktop reports.
    pub fn account_email(&self, live: Option<&CliAccount>) -> Option<String> {
        let cli = self.identity(live)?;
        let email = cli.email?;
        match (self.desktop_account_id(), cli.account_uuid) {
            (Some(desktop), Some(cli)) if desktop != cli => None,
            _ => Some(email),
        }
    }
}

/// Code sessions Desktop has stored for one account folder of a profile.
#[derive(Debug, Clone, Default)]
pub struct AccountData {
    pub account_uuid: String,
    pub email: Option<String>,
    pub sessions: usize,
    pub archived: usize,
    /// Unix milliseconds of the newest session activity.
    pub last_activity_ms: Option<u64>,
    /// Titles of the most recently active sessions, newest first.
    pub recent: Vec<String>,
}

/// What a profile folder holds, as far as it can be read without Claude running.
#[derive(Debug, Clone, Default)]
pub struct ProfileContents {
    pub accounts: Vec<AccountData>,
    pub mcp_servers: usize,
    pub cli_email: Option<String>,
}

const RECENT_TITLES: usize = 3;

impl Profile {
    /// Reads the session index files, so callers should cache the result.
    pub fn contents(&self, live: Option<&CliAccount>) -> ProfileContents {
        let cli = self.identity(live);
        let root = self.dir.join(sessions::SESSIONS_DIR);
        let mut accounts = Vec::new();
        let mut folders: Vec<_> = fs::read_dir(&root)
            .map(|entries| entries.flatten().collect())
            .unwrap_or_default();
        folders.sort_by_key(|entry| entry.file_name());
        for folder in folders {
            if !folder.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let account_uuid = folder.file_name().to_string_lossy().into_owned();
            let email = cli
                .as_ref()
                .filter(|cli| cli.account_uuid.as_deref() == Some(account_uuid.as_str()))
                .and_then(|cli| cli.email.clone());
            let mut data = AccountData {
                account_uuid,
                email,
                ..AccountData::default()
            };
            let mut titled: Vec<(u64, String)> = Vec::new();
            for org in fs::read_dir(folder.path()).into_iter().flatten().flatten() {
                for file in fs::read_dir(org.path()).into_iter().flatten().flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("local_") || !name.ends_with(".json") {
                        continue;
                    }
                    let Some(session) = fs::read_to_string(file.path())
                        .ok()
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                    else {
                        continue;
                    };
                    data.sessions += 1;
                    if session.get("isArchived").and_then(|v| v.as_bool()) == Some(true) {
                        data.archived += 1;
                    }
                    let activity = session.get("lastActivityAt").and_then(|v| v.as_u64());
                    data.last_activity_ms = data.last_activity_ms.max(activity);
                    if let Some(title) = session.get("title").and_then(|v| v.as_str()) {
                        titled.push((activity.unwrap_or(0), title.to_string()));
                    }
                }
            }
            titled.sort_by(|a, b| b.0.cmp(&a.0));
            data.recent = titled
                .into_iter()
                .take(RECENT_TITLES)
                .map(|(_, title)| title)
                .collect();
            accounts.push(data);
        }

        let mcp_servers = fs::read_to_string(self.dir.join("claude_desktop_config.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|config| config.get("mcpServers")?.as_object().map(|m| m.len()))
            .unwrap_or(0);

        ProfileContents {
            accounts,
            mcp_servers,
            cli_email: cli.and_then(|cli| cli.email),
        }
    }
}

/// Identity fields of a profile's Claude Code CLI snapshot (`oauth_account.json`).
#[derive(Debug, Clone)]
pub struct CliAccount {
    pub account_uuid: Option<String>,
    pub org_uuid: Option<String>,
    pub email: Option<String>,
}

fn parse_cli_account(value: &serde_json::Value) -> Option<CliAccount> {
    let account = value.get("oauthAccount")?;
    let field = |key: &str| {
        account
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(CliAccount {
        account_uuid: field("accountUuid"),
        org_uuid: field("organizationUuid"),
        email: field("emailAddress"),
    })
}

/// The Claude Code login in `~/.claude.json` right now, i.e. the active account's.
pub fn live_cli_account() -> Option<CliAccount> {
    let raw = fs::read_to_string(paths::claude_code_state_file()?).ok()?;
    parse_cli_account(&serde_json::from_str(&raw).ok()?)
}

#[derive(Debug, Clone)]
pub enum LiveState {
    Missing,
    Junction(PathBuf),
    Symlink,
    RealDir,
    Other,
}

#[derive(Debug, Clone)]
pub struct SwitchOutcome {
    pub profile: String,
    pub killed: KillReport,
    pub launched: bool,
    pub launch_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileManager {
    pub app_root: PathBuf,
    pub store_root: PathBuf,
    pub live: PathBuf,
    profiles: Vec<Profile>,
    active: usize,
    launch_target: LaunchTarget,
    pub share_mcp_config: bool,
    pub sync_claude_code: bool,
    pub share_sessions: bool,
}

/// Automatic session-list snapshots kept in `backups\`.
const SESSION_BACKUPS_KEPT: usize = 10;

impl ProfileManager {
    pub fn new(cfg: &Config) -> io::Result<Self> {
        let app_root = paths::app_root();
        let store_root = paths::profiles_root();
        let (live, live_kind) = claude::resolve_live_dir(cfg.live_override.as_deref());
        let profiles: Vec<Profile> = cfg
            .profiles
            .iter()
            .map(|profile| Profile {
                name: profile.name.clone(),
                dir: cfg.resolve_dir(profile),
            })
            .collect();
        let active = cfg
            .active
            .unwrap_or(0)
            .min(profiles.len().saturating_sub(1));
        let launch_target = claude::resolve_launch_target(cfg, &live_kind);

        let mut manager = Self {
            app_root,
            store_root,
            live,
            profiles,
            active,
            launch_target,
            share_mcp_config: cfg.share_mcp_config,
            sync_claude_code: cfg.sync_claude_code,
            share_sessions: cfg.share_sessions,
        };
        manager.ensure_layout()?;
        manager.refresh_active_from_junction();
        if manager.share_mcp_config {
            let _ = manager.sync_mcp_config();
        }
        if manager.sync_claude_code {
            let _ = manager.save_cli_auth_for(manager.active);
        }
        Ok(manager)
    }

    /// Re-syncs profiles, live path, and launch target after `Config` is edited in the UI.
    pub fn reload_from_config(&mut self, cfg: &Config) -> io::Result<()> {
        let (live, live_kind) = claude::resolve_live_dir(cfg.live_override.as_deref());
        self.live = live;
        self.profiles = cfg
            .profiles
            .iter()
            .map(|profile| Profile {
                name: profile.name.clone(),
                dir: cfg.resolve_dir(profile),
            })
            .collect();
        self.active = self.active.min(self.profiles.len().saturating_sub(1));
        self.launch_target = claude::resolve_launch_target(cfg, &live_kind);
        self.share_mcp_config = cfg.share_mcp_config;
        self.sync_claude_code = cfg.sync_claude_code;
        self.share_sessions = cfg.share_sessions;
        self.ensure_layout()?;
        self.refresh_active_from_junction();
        if self.share_mcp_config {
            let _ = self.sync_mcp_config();
        }
        Ok(())
    }

    pub fn ensure_layout(&self) -> io::Result<()> {
        fs::create_dir_all(&self.app_root)?;
        fs::create_dir_all(&self.store_root)?;
        for profile in &self.profiles {
            fs::create_dir_all(&profile.dir)?;
        }
        Ok(())
    }

    /// Adopts the junction target as the active profile when it maps to one of ours.
    pub fn refresh_active_from_junction(&mut self) {
        if let Ok(LiveState::Junction(target)) = self.live_state() {
            if let Some(index) = self
                .profiles
                .iter()
                .position(|profile| paths::same_path(&profile.dir, &target))
            {
                self.active = index;
            }
        }
    }

    pub fn launch_target(&self) -> &LaunchTarget {
        &self.launch_target
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_profile(&self) -> &Profile {
        &self.profiles[self.active.min(self.profiles.len().saturating_sub(1))]
    }

    pub fn active_dir(&self) -> PathBuf {
        self.active_profile().dir.clone()
    }

    pub fn live_state(&self) -> io::Result<LiveState> {
        let metadata = match fs::symlink_metadata(&self.live) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LiveState::Missing),
            Err(error) => return Err(error),
        };
        // Junctions report as directories to `symlink_metadata`, so test them first.
        if junction::exists(&self.live).unwrap_or(false) {
            let target = junction::get_target(&self.live).unwrap_or_default();
            return Ok(LiveState::Junction(target));
        }
        if metadata.file_type().is_symlink() {
            return Ok(LiveState::Symlink);
        }
        if metadata.is_dir() {
            return Ok(LiveState::RealDir);
        }
        Ok(LiveState::Other)
    }

    /// True when the live path is a junction already pointing at `profile_dir`.
    pub fn live_targets(&self, profile_dir: &Path) -> bool {
        matches!(
            self.live_state(),
            Ok(LiveState::Junction(target)) if paths::same_path(&target, profile_dir)
        )
    }

    fn remove_link(&self) -> io::Result<()> {
        if junction::exists(&self.live).unwrap_or(false) {
            with_retry("remove junction", || junction::delete(&self.live))?;
            // `junction::delete` only strips the reparse point and leaves an empty directory
            // behind, which would make the next `junction::create` fail with ERROR_ALREADY_EXISTS.
            // `remove_dir` refuses a non-empty directory, so this can never destroy data.
            return with_retry("remove junction directory", || {
                match fs::remove_dir(&self.live) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    other => other,
                }
            });
        }
        let metadata = match fs::symlink_metadata(&self.live) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            let points_at_dir = fs::metadata(&self.live)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            return with_retry("remove symlink", || {
                if points_at_dir {
                    fs::remove_dir(&self.live)
                } else {
                    fs::remove_file(&self.live)
                }
            });
        }
        if metadata.is_dir() {
            return Err(io::Error::other(format!(
                "refusing to delete real directory {} — it should have been adopted first",
                self.live.display()
            )));
        }
        with_retry("remove stray file", || fs::remove_file(&self.live))
    }

    fn create_junction(&self, target: &Path) -> io::Result<()> {
        if let Some(parent) = self.live.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(target)?;
        with_retry("create junction", || junction::create(target, &self.live))?;
        log_info!("linked {} -> {}", self.live.display(), target.display());
        Ok(())
    }

    /// Guarantees `self.live` is a junction into one of our profiles.
    ///
    /// `allow_migrate` must only be true when Claude is not running, otherwise an
    /// existing real data directory would be yanked out from under the app.
    pub fn ensure_live_linked(&self, allow_migrate: bool) -> io::Result<()> {
        match self.live_state()? {
            LiveState::Junction(target) => {
                let known = self
                    .profiles
                    .iter()
                    .any(|profile| paths::same_path(&profile.dir, &target));
                if known && target.exists() {
                    Ok(())
                } else {
                    log_warn!(
                        "junction {} points at unknown/missing {}; repairing",
                        self.live.display(),
                        target.display()
                    );
                    self.remove_link()?;
                    self.create_junction(&self.active_dir())
                }
            }
            LiveState::Missing => self.create_junction(&self.active_dir()),
            LiveState::Symlink => {
                log_warn!(
                    "replacing symlink at {} with a junction",
                    self.live.display()
                );
                self.remove_link()?;
                self.create_junction(&self.active_dir())
            }
            LiveState::RealDir => {
                if !allow_migrate {
                    log_warn!(
                        "{} is a real directory and Claude is running; leaving it untouched",
                        self.live.display()
                    );
                    return Ok(());
                }
                self.adopt_real_dir()?;
                self.create_junction(&self.active_dir())
            }
            LiveState::Other => Err(io::Error::other(format!(
                "unexpected non-directory at {}; move it aside and retry",
                self.live.display()
            ))),
        }
    }

    /// Moves an unmanaged real data directory into the active profile.
    fn adopt_real_dir(&self) -> io::Result<()> {
        if is_dir_empty(&self.live)? {
            // What a removed junction leaves behind; nothing to adopt.
            return with_retry("remove empty directory", || fs::remove_dir(&self.live));
        }

        let destination = self.active_dir();
        fs::create_dir_all(&destination)?;

        if holds_account_data(&destination)? {
            // The profile already is an account, so a real directory at the live path is a
            // fresh stray (Claude was started while the link was missing). Merging it in with
            // "live wins" would overwrite the signed-in session, so keep it aside instead.
            let stray = self.app_root.join("backups").join(format!(
                "stray-claude-data-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs())
            ));
            log_warn!(
                "{} is a real directory but {} already holds account data; moving it to {} instead of merging",
                self.live.display(),
                destination.display(),
                stray.display()
            );
            if let Some(parent) = stray.parent() {
                fs::create_dir_all(parent)?;
            }
            return move_entry(&self.live, &stray);
        }

        log_info!(
            "adopting existing Claude data {} into {}",
            self.live.display(),
            destination.display()
        );

        if is_dir_empty(&destination)? {
            // Same-volume rename is instant and atomic — the common case.
            let _ = fs::remove_dir(&destination);
            match fs::rename(&self.live, &destination) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    log_info!("fast move unavailable ({error}); copying instead");
                    fs::create_dir_all(&destination)?;
                }
            }
        }

        merge_dir_contents(&self.live, &destination, true)?;
        with_retry("remove adopted source", || fs::remove_dir_all(&self.live))?;
        Ok(())
    }

    fn repoint_to(&self, target: &Path) -> io::Result<()> {
        let previous = match self.live_state()? {
            LiveState::Missing => None,
            LiveState::Junction(previous) => {
                self.remove_link()?;
                Some(previous)
            }
            LiveState::Symlink | LiveState::Other => {
                self.remove_link()?;
                None
            }
            LiveState::RealDir => {
                return Err(io::Error::other(format!(
                    "{} is still a real directory; run Repair before switching",
                    self.live.display()
                )))
            }
        };
        let result = self.create_junction(target);
        if result.is_err() {
            // Never leave Claude without a data directory: put the old link back.
            if let Some(previous) = previous.filter(|dir| dir.exists()) {
                if let Err(error) = self.create_junction(&previous) {
                    log_warn!("could not restore the previous link: {error}");
                }
            }
        }
        result
    }

    fn profile_dirs(&self) -> Vec<PathBuf> {
        self.profiles.iter().map(|profile| profile.dir.clone()).collect()
    }

    /// Snapshots every account's Code session list into `backups\`, keeping the newest few.
    /// Claude should be closed so the files are not changing underneath the copy.
    pub fn backup_sessions(&self) -> io::Result<Option<PathBuf>> {
        sessions::backup(
            &self.profile_dirs(),
            &self.app_root.join("backups"),
            SESSION_BACKUPS_KEPT,
        )
    }

    /// Mirrors the Code session lists across all profiles (see `sessions`). Claude must be
    /// closed. With `backup` the lists are snapshotted first, but only when something would
    /// change; if that snapshot fails nothing is touched.
    pub fn sync_sessions(&self, backup: bool) -> io::Result<sessions::Report> {
        // Accounts known only from a CLI snapshot have no session folder yet; seed it so a
        // login to one of them shows the shared list on first launch.
        let known_folders: Vec<(String, String)> = self
            .profiles
            .iter()
            .filter_map(Profile::cli_account)
            .filter_map(|cli| Some((cli.account_uuid?, cli.org_uuid?)))
            .collect();
        let plan = sessions::plan(&self.profile_dirs(), &known_folders);
        if plan.is_empty() {
            return Ok(sessions::Report::default());
        }
        if backup {
            self.backup_sessions()?;
        }
        let report = sessions::apply(&plan);
        log_info!(
            "shared Code sessions across {} profile(s): {} copied, {} removed, {} failed",
            self.profiles.len(),
            report.copied,
            report.removed,
            report.failed
        );
        Ok(report)
    }

    /// Propagates the newest `claude_desktop_config.json` across all profiles so MCP servers
    /// and global desktop preferences stay consistent across accounts.
    pub fn sync_mcp_config(&self) -> io::Result<usize> {
        let shared = paths::shared_mcp_config_file();
        let mut candidates: Vec<PathBuf> = vec![shared.clone()];
        for profile in &self.profiles {
            candidates.push(profile.dir.join("claude_desktop_config.json"));
        }

        let mut newest: Option<(std::time::SystemTime, Vec<u8>)> = None;
        for path in &candidates {
            let Ok(meta) = fs::metadata(path) else {
                continue;
            };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if newest.as_ref().is_none_or(|(best, _)| mtime > *best) {
                if let Ok(bytes) = fs::read(path) {
                    newest = Some((mtime, bytes));
                }
            }
        }

        let Some((_, bytes)) = newest else {
            return Ok(0);
        };

        let _ = fs::write(&shared, &bytes);
        let mut updated = 0usize;
        for profile in &self.profiles {
            let _ = fs::create_dir_all(&profile.dir);
            let dst = profile.dir.join("claude_desktop_config.json");
            let current = fs::read(&dst).ok();
            if current.as_deref() != Some(bytes.as_slice()) && fs::write(&dst, &bytes).is_ok() {
                updated += 1;
            }
        }
        if updated > 0 {
            log_info!("synced claude_desktop_config.json across {updated} profile(s)");
        }
        Ok(updated)
    }

    /// Snapshots the active Claude Code CLI (`~/.claude/.credentials.json` and auth fields from
    /// `~/.claude.json`) into the specified profile's `claude_code_auth` subfolder.
    pub fn save_cli_auth_for(&self, index: usize) -> io::Result<bool> {
        let Some(profile) = self.profiles.get(index) else {
            return Ok(false);
        };
        let auth_dir = profile.dir.join("claude_code_auth");
        let mut saved_any = false;

        if let Some(creds_src) = paths::claude_code_credentials_file() {
            if creds_src.is_file() {
                fs::create_dir_all(&auth_dir)?;
                fs::copy(&creds_src, auth_dir.join(".credentials.json"))?;
                saved_any = true;
            }
        }

        if let Some(state_src) = paths::claude_code_state_file() {
            if let Ok(raw) = fs::read_to_string(&state_src) {
                if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&raw) {
                    let mut auth_subset = serde_json::Map::new();
                    for key in [
                        "oauthAccount",
                        "userID",
                        "primaryApiKey",
                        "hasCompletedOnboarding",
                    ] {
                        if let Some(val) = map.get(key) {
                            auth_subset.insert(key.to_string(), val.clone());
                        }
                    }
                    if !auth_subset.is_empty() {
                        fs::create_dir_all(&auth_dir)?;
                        let json = serde_json::to_string_pretty(&serde_json::Value::Object(
                            auth_subset,
                        ))
                        .unwrap_or_default();
                        fs::write(auth_dir.join("oauth_account.json"), json)?;
                        saved_any = true;
                    }
                }
            }
        }

        Ok(saved_any)
    }

    /// Restores Claude Code CLI credentials from the target profile's `claude_code_auth` subfolder.
    /// Preserves non-auth fields (`mcpServers`, `projects`, etc.) inside `~/.claude.json`.
    pub fn restore_cli_auth_for(&self, index: usize) -> io::Result<bool> {
        let Some(profile) = self.profiles.get(index) else {
            return Ok(false);
        };
        let auth_dir = profile.dir.join("claude_code_auth");
        let saved_creds = auth_dir.join(".credentials.json");
        let saved_meta = auth_dir.join("oauth_account.json");
        let mut restored_any = false;

        if let Some(live_creds) = paths::claude_code_credentials_file() {
            if saved_creds.is_file() {
                if let Some(parent) = live_creds.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&saved_creds, &live_creds)?;
                restored_any = true;
            } else if self.profiles.iter().any(|p| p.has_cli_auth()) && live_creds.is_file() {
                // Another profile owns CLI auth while this profile is clean; clear active token
                // so the user doesn't stay signed into the previous profile's CLI session.
                let _ = fs::remove_file(&live_creds);
            }
        }

        if saved_meta.is_file() {
            if let Some(live_state) = paths::claude_code_state_file() {
                if let Ok(saved_raw) = fs::read_to_string(&saved_meta) {
                    if let Ok(serde_json::Value::Object(saved_map)) =
                        serde_json::from_str(&saved_raw)
                    {
                        let mut root_map = fs::read_to_string(&live_state)
                            .ok()
                            .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
                            .and_then(|v| match v {
                                serde_json::Value::Object(m) => Some(m),
                                _ => None,
                            })
                            .unwrap_or_default();

                        for (k, v) in saved_map {
                            root_map.insert(k, v);
                        }
                        if let Ok(json) =
                            serde_json::to_string_pretty(&serde_json::Value::Object(root_map))
                        {
                            let tmp = live_state.with_extension("json.tmp");
                            if fs::write(&tmp, json).is_ok() {
                                let _ = fs::remove_file(&live_state);
                                let _ = fs::rename(&tmp, &live_state);
                                restored_any = true;
                            }
                        }
                    }
                }
            }
        }

        if restored_any {
            log_info!(
                "restored Claude Code CLI credentials for '{}'",
                profile.name
            );
        }
        Ok(restored_any)
    }

    /// Launches a second independent Claude Desktop window for `index` using `--user-data-dir`
    /// without terminating the currently running Claude session or changing the NTFS junction.
    pub fn launch_side_by_side(&self, index: usize) -> Result<String, String> {
        let Some(profile) = self.profiles.get(index) else {
            return Err(format!("profile index {index} is out of range"));
        };
        let _ = fs::create_dir_all(&profile.dir);
        if self.share_mcp_config {
            let _ = self.sync_mcp_config();
        }
        claude::launch_with_profile(&self.launch_target, &profile.dir)
            .map(|()| {
                format!(
                    "Launched '{}' in a separate side-by-side window.",
                    profile.name
                )
            })
            .map_err(|e| e.to_string())
    }

    /// Kill Claude, point the live path at `index`, relaunch. Blocking; call it
    /// off the UI thread.
    pub fn switch_to(&mut self, index: usize, launch: bool) -> Result<SwitchOutcome, String> {
        if index >= self.profiles.len() {
            return Err(format!("profile index {index} is out of range"));
        }
        let target = self.profiles[index].dir.clone();
        log_info!(
            "switching to '{}' ({index}) -> {}",
            self.profiles[index].name,
            target.display()
        );

        if self.sync_claude_code {
            let _ = self.save_cli_auth_for(self.active);
        }

        let killed = process::terminate_claude(Duration::from_secs(6));
        // Let the OS release directory handles before we touch the tree.
        std::thread::sleep(Duration::from_millis(250));

        // Cheap insurance, taken while nothing is writing: the session lists are what a bad
        // link or a bad sync would cost the user.
        let backed_up = match self.backup_sessions() {
            Ok(_) => true,
            Err(error) => {
                log_warn!("could not back up Code sessions before switching: {error}");
                false
            }
        };

        if !self.live_targets(&target) {
            self.ensure_live_linked(true)
                .map_err(|error| friendly("preparing the profile link", &error))?;
        }
        if self.share_mcp_config {
            let _ = self.sync_mcp_config();
        }
        if self.share_sessions && backed_up {
            if let Err(error) = self.sync_sessions(false) {
                log_warn!("could not share Code sessions: {error}");
            }
        }
        if !self.live_targets(&target) {
            self.repoint_to(&target)
                .map_err(|error| friendly("swapping the profile link", &error))?;
        }
        self.active = index;

        if self.sync_claude_code {
            let _ = self.restore_cli_auth_for(index);
        }

        let mut launched = false;
        let mut launch_error = None;
        if launch {
            let target = self.launch_target.clone();
            match claude::launch(&target) {
                Ok(()) => launched = true,
                Err(error) => {
                    let message = format!("{error}");
                    log_error!("Claude relaunch failed: {message}");
                    launch_error = Some(message);
                }
            }
        }

        Ok(SwitchOutcome {
            profile: self.profiles[index].name.clone(),
            killed,
            launched,
            launch_error,
        })
    }

    /// Re-establishes the live link after manual tampering.
    pub fn repair(&self) -> Result<String, String> {
        // A healthy junction can be verified without disturbing Claude.
        let healthy = matches!(
            self.live_state(),
            Ok(LiveState::Junction(target)) if target.exists()
        ) && self.live_targets(&self.active_dir());
        if !healthy {
            process::terminate_claude(Duration::from_secs(6));
            std::thread::sleep(Duration::from_millis(250));
        }
        self.ensure_live_linked(true)
            .map_err(|error| friendly("repairing the profile link", &error))?;
        if self.share_mcp_config {
            let _ = self.sync_mcp_config();
        }
        Ok(format!(
            "{} now points at '{}'",
            self.live.display(),
            self.active_profile().name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_dir_contents, LaunchTarget, Profile, ProfileManager};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cst-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn adoption_prefers_the_live_copy_on_conflicts() {
        let base = scratch("adopt");
        let live = base.join("live");
        let profile = base.join("profile");
        fs::create_dir_all(live.join("nested")).unwrap();
        fs::write(live.join("session.json"), b"live").unwrap();
        fs::write(live.join("nested/deep.json"), b"deep").unwrap();
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("session.json"), b"profile").unwrap();

        merge_dir_contents(&live, &profile, true).unwrap();

        assert_eq!(fs::read(profile.join("session.json")).unwrap(), b"live");
        assert_eq!(fs::read(profile.join("nested/deep.json")).unwrap(), b"deep");
        assert!(!live.join("session.json").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn default_merge_keeps_the_profile_copy() {
        let base = scratch("keep");
        let live = base.join("live");
        let profile = base.join("profile");
        fs::create_dir_all(&live).unwrap();
        fs::create_dir_all(&profile).unwrap();
        fs::write(live.join("session.json"), b"live").unwrap();
        fs::write(profile.join("session.json"), b"profile").unwrap();

        merge_dir_contents(&live, &profile, false).unwrap();

        assert_eq!(fs::read(profile.join("session.json")).unwrap(), b"profile");
        assert!(!live.join("session.json").exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// A manager over scratch directories; no config, processes or Claude install involved.
    fn manager_for(base: &Path, live: PathBuf, profile_dirs: &[&Path]) -> ProfileManager {
        ProfileManager {
            app_root: base.to_path_buf(),
            store_root: base.join("profiles"),
            live,
            profiles: profile_dirs
                .iter()
                .enumerate()
                .map(|(index, dir)| Profile {
                    name: format!("p{index}"),
                    dir: dir.to_path_buf(),
                })
                .collect(),
            active: 0,
            launch_target: LaunchTarget::Shell(String::new()),
            share_mcp_config: false,
            sync_claude_code: false,
            share_sessions: false,
        }
    }

    fn profile_with_marker(base: &Path, name: &str) -> PathBuf {
        let dir = base.join("profiles").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("who"), name).unwrap();
        dir
    }

    #[test]
    fn repointing_an_existing_junction_switches_profiles_and_keeps_their_data() {
        let base = scratch("repoint");
        let a = profile_with_marker(&base, "a");
        let b = profile_with_marker(&base, "b");
        let manager = manager_for(&base, base.join("live"), &[&a, &b]);

        manager.create_junction(&a).unwrap();
        assert!(manager.live_targets(&a));
        assert_eq!(fs::read(base.join("live/who")).unwrap(), b"a");

        manager.repoint_to(&b).unwrap();
        assert!(manager.live_targets(&b));
        assert_eq!(fs::read(base.join("live/who")).unwrap(), b"b");

        manager.repoint_to(&a).unwrap();
        assert!(manager.live_targets(&a));

        // Removing the link must never touch what it pointed at.
        assert_eq!(fs::read(a.join("who")).unwrap(), b"a");
        assert_eq!(fs::read(b.join("who")).unwrap(), b"b");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_directory_left_where_the_link_was_is_relinked() {
        let base = scratch("relink");
        let a = profile_with_marker(&base, "a");
        let live = base.join("live");
        fs::create_dir_all(&live).unwrap();
        let manager = manager_for(&base, live, &[&a]);

        manager.ensure_live_linked(true).unwrap();

        assert!(manager.live_targets(&a));
        assert_eq!(fs::read(base.join("live/who")).unwrap(), b"a");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_stray_directory_never_overwrites_a_profile_that_holds_an_account() {
        let base = scratch("stray");
        let a = profile_with_marker(&base, "a");
        let live = base.join("live");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("who"), "stray").unwrap();
        fs::write(live.join("Local State"), "fresh").unwrap();
        let manager = manager_for(&base, live, &[&a]);

        manager.ensure_live_linked(true).unwrap();

        assert!(manager.live_targets(&a));
        assert_eq!(fs::read(a.join("who")).unwrap(), b"a");
        assert!(!a.join("Local State").exists());
        // The stray copy is kept for reference rather than deleted.
        let kept: Vec<_> = fs::read_dir(base.join("backups")).unwrap().collect();
        assert_eq!(kept.len(), 1);
        let kept = kept.into_iter().next().unwrap().unwrap().path();
        assert_eq!(fs::read(kept.join("who")).unwrap(), b"stray");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn first_run_adopts_the_existing_install_into_a_blank_profile() {
        let base = scratch("first-run");
        let a = base.join("profiles").join("a");
        let live = base.join("live");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("who"), "existing").unwrap();
        let manager = manager_for(&base, live, &[&a]);

        manager.ensure_live_linked(true).unwrap();

        assert!(manager.live_targets(&a));
        assert_eq!(fs::read(a.join("who")).unwrap(), b"existing");
        assert!(!base.join("backups").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_signed_in_account_takes_its_email_from_the_live_cli_login() {
        use super::CliAccount;
        let base = scratch("identity");
        fs::write(base.join("config.json"), r#"{"lastKnownAccountUuid":"abc"}"#).unwrap();
        let profile = Profile { name: "p".into(), dir: base.clone() };
        let login = |uuid: &str| CliAccount {
            account_uuid: Some(uuid.into()),
            org_uuid: None,
            email: Some(format!("{uuid}@example.com")),
        };

        // No snapshot yet: only a live login for the same account counts.
        assert_eq!(profile.account_email(None), None);
        assert_eq!(profile.account_email(Some(&login("abc"))).as_deref(), Some("abc@example.com"));
        assert_eq!(profile.account_email(Some(&login("other"))), None);

        // A snapshot of the same account wins over the live one.
        fs::create_dir_all(base.join("claude_code_auth")).unwrap();
        fs::write(
            base.join("claude_code_auth").join("oauth_account.json"),
            r#"{"oauthAccount":{"accountUuid":"abc","emailAddress":"saved@example.com"}}"#,
        )
        .unwrap();
        assert_eq!(profile.account_email(Some(&login("abc"))).as_deref(), Some("saved@example.com"));
        let _ = fs::remove_dir_all(&base);
    }
}
