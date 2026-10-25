//! Copies account data from one Claude data folder (a profile or a backup) into another.
//!
//! A Desktop data folder is mostly weight that is not the account: the Linux sandbox
//! (`vm_bundles`, `claude-code-vm`), downloaded CLI builds and Chromium caches make up
//! several GB, while the session history and settings are a few MB. Every top-level entry
//! is therefore classified, and a transfer is planned per entry so the caller can show it
//! before anything is written.
//!
//! Nothing here deletes or overwrites a login: an entry the target already has is only ever
//! merged when it is history (newest copy wins) or runtime data (missing files are added).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::profiles::with_retry;

/// What a top-level entry of a Claude data folder is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Cookies and tokens: copying these makes the target the same sign-in.
    Login,
    Settings,
    /// Code and Cowork session lists and their workspaces.
    History,
    /// The Linux sandbox and downloaded CLI builds. Large, rebuilt by Claude on demand.
    Runtime,
    /// Chromium caches, logs and crash dumps. Rebuilt by Claude.
    Cache,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Login => "LOGIN",
            Kind::Settings => "SETTINGS",
            Kind::History => "HISTORY",
            Kind::Runtime => "RUNTIME",
            Kind::Cache => "CACHE",
            Kind::Other => "OTHER",
        }
    }
}

pub fn classify(name: &str) -> Kind {
    match name.to_ascii_lowercase().as_str() {
        "local storage" | "session storage" | "network" | "indexeddb" | "webstorage"
        | "local state" | "preferences" | "config.json" | "ant-did" | "ant-device-registry.json"
        | "ccd-ids.json" | "buddy-tokens.json" | "claude_code_auth" | "dips" | "dips-wal"
        | "sharedstorage" | "cookies" | "cookies-journal" | "shared dictionary" => Kind::Login,
        "claude_desktop_config.json" | "window-state.json" | "extensions-blocklist.json"
        | "ccd-session-presets.json" | "cowork-enabled-cli-ops.json"
        | "remote-control-state.json" => Kind::Settings,
        "claude-code-sessions" | "local-agent-mode-sessions" | "git-worktrees.json"
        | "plan-usage-history.json" | "scratch-workspaces" | "side-session-notes"
        | "spaces-present" => Kind::History,
        "vm_bundles" | "claude-code-vm" | "claude-code" => Kind::Runtime,
        "cache" | "code cache" | "gpucache" | "dawngraphitecache" | "dawnwebgpucache"
        | "gpupersistentcache" | "grshadercache" | "shadercache" | "crashpad" | "logs"
        | "sentry" | "videodecodestats" | "shared_proto_db" | "preview-last-frames"
        | "terminal-shell-integration" | "lockfile" | "declarative_performance_observer.db"
        | "declarative_performance_observer.db-journal" => Kind::Cache,
        _ => Kind::Other,
    }
}

/// One line on what a well-known entry is, for the inventory.
pub fn describe(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "vm_bundles" => "Linux sandbox image (Cowork)",
        "claude-code-vm" => "Linux sandbox for Claude Code",
        "claude-code" => "Downloaded Claude Code builds",
        "claude-code-sessions" => "Code session list",
        "local-agent-mode-sessions" => "Cowork sessions",
        "claude_code_auth" => "Saved Code CLI login",
        "config.json" => "Signed-in Desktop account",
        "claude_desktop_config.json" => "MCP servers and app settings",
        "local storage" | "session storage" | "indexeddb" | "network" | "cookies"
        | "local state" => "Sign-in and site data",
        "cache" | "code cache" | "gpucache" | "dawngraphitecache" | "dawnwebgpucache"
        | "grshadercache" | "shadercache" => "Browser cache, rebuilt automatically",
        "logs" => "Log files",
        "crashpad" => "Crash reports",
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Settings and history only: the few MB that are the user's own.
    Minimal,
    /// Everything except caches and logs, Linux sandbox included.
    Full,
}

impl Mode {
    pub fn selects(self, kind: Kind) -> bool {
        match self {
            Mode::Minimal => matches!(kind, Kind::Settings | Kind::History),
            Mode::Full => kind != Kind::Cache,
        }
    }
}

/// One top-level entry of a data folder, measured without following links.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub bytes: u64,
    pub files: u64,
    /// A symlink or junction; never copied, since the target is not ours to duplicate.
    pub is_link: bool,
}

fn measure(path: &Path) -> (u64, u64) {
    let Ok(entries) = fs::read_dir(path) else {
        return (0, 0);
    };
    let (mut bytes, mut files) = (0, 0);
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            let (b, f) = measure(&entry.path());
            bytes += b;
            files += f;
        } else if let Ok(meta) = entry.metadata() {
            bytes += meta.len();
            files += 1;
        }
    }
    (bytes, files)
}

/// Lists and sizes the top-level entries of `dir`. Walks the whole tree, so run it off the
/// UI thread.
pub fn scan(dir: &Path) -> Vec<Entry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Entry> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let kind = entry.file_type().ok()?;
            if kind.is_symlink() {
                return Some(Entry { name, is_dir: kind.is_dir(), bytes: 0, files: 0, is_link: true });
            }
            if kind.is_dir() {
                let (bytes, files) = measure(&entry.path());
                Some(Entry { name, is_dir: true, bytes, files, is_link: false })
            } else {
                let bytes = entry.metadata().map_or(0, |meta| meta.len());
                Some(Entry { name, is_dir: false, bytes, files: 1, is_link: false })
            }
        })
        .collect();
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    found
}

/// True for a folder that holds Claude Desktop data, as opposed to some other backup.
pub fn looks_like_claude_dir(dir: &Path) -> bool {
    ["config.json", "claude-code-sessions", "Local State", "claude_desktop_config.json"]
        .iter()
        .any(|marker| dir.join(marker).exists())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The target has nothing by this name.
    Copy,
    /// Both have it; files the target lacks are added, and history keeps the newer copy.
    Merge,
    /// Both have it and it is left exactly as it is, so a login is never overwritten.
    Keep,
    /// Not selected, or a link.
    Skip,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Copy => "COPY",
            Action::Merge => "MERGE",
            Action::Keep => "KEEP",
            Action::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub name: String,
    pub kind: Kind,
    pub is_dir: bool,
    pub bytes: u64,
    pub files: u64,
    /// The target's entry is a link to a shared folder.
    pub target_is_link: bool,
    pub selected: bool,
    pub is_link: bool,
    pub action: Action,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

/// Totals shown above the per-folder list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub copy_items: usize,
    pub merge_items: usize,
    pub keep_items: usize,
    pub skip_items: usize,
    /// Bytes that will be read from the source (an upper bound for merges).
    pub bytes: u64,
    pub skipped_bytes: u64,
}

impl Plan {
    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        for item in &self.items {
            match item.action {
                Action::Copy => {
                    summary.copy_items += 1;
                    summary.bytes += item.bytes;
                }
                Action::Merge => {
                    summary.merge_items += 1;
                    summary.bytes += item.bytes;
                }
                Action::Keep => summary.keep_items += 1,
                Action::Skip => {
                    summary.skip_items += 1;
                    summary.skipped_bytes += item.bytes;
                }
            }
        }
        summary
    }

    pub fn writes_anything(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.action, Action::Copy | Action::Merge))
    }
}

fn merge_allowed(kind: Kind, is_dir: bool) -> bool {
    is_dir && matches!(kind, Kind::History | Kind::Runtime)
}

/// Decides what happens to every source entry. `overrides` holds the entries the user ticked
/// or unticked against the mode's default.
pub fn plan(
    source: &[Entry],
    target: &[Entry],
    mode: Mode,
    overrides: &HashMap<String, bool>,
) -> Plan {
    let target_sizes: HashMap<&str, &Entry> = target
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    let items = source
        .iter()
        .map(|entry| {
            let kind = classify(&entry.name);
            let selected = overrides
                .get(&entry.name)
                .copied()
                .unwrap_or_else(|| mode.selects(kind));
            let existing = target_sizes.get(entry.name.as_str()).copied();
            let action = if !selected || entry.is_link {
                Action::Skip
            } else if existing.is_none() {
                Action::Copy
            } else if existing.is_some_and(|found| found.is_link) {
                // A shared folder: writing into it would change every account at once.
                Action::Keep
            } else if merge_allowed(kind, entry.is_dir) {
                Action::Merge
            } else {
                Action::Keep
            };
            PlanItem {
                name: entry.name.clone(),
                kind,
                is_dir: entry.is_dir,
                bytes: entry.bytes,
                files: entry.files,
                target_is_link: existing.is_some_and(|found| found.is_link),
                selected,
                is_link: entry.is_link,
                action,
            }
        })
        .collect();
    Plan { items }
}

/// What happened to one entry.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub name: String,
    pub kind: Kind,
    pub action: Action,
    pub added: u64,
    pub updated: u64,
    pub kept: u64,
    pub bytes: u64,
    pub failed: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn failed(&self) -> u64 {
        self.outcomes.iter().map(|o| o.failed).sum()
    }

    pub fn added(&self) -> u64 {
        self.outcomes.iter().map(|o| o.added).sum()
    }

    pub fn updated(&self) -> u64 {
        self.outcomes.iter().map(|o| o.updated).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.outcomes.iter().map(|o| o.bytes).sum()
    }
}

#[derive(Default)]
struct Tally {
    added: u64,
    updated: u64,
    kept: u64,
    bytes: u64,
    failed: u64,
    first_error: Option<String>,
}

impl Tally {
    fn fail(&mut self, path: &Path, error: &io::Error) {
        self.failed += 1;
        log_warn!("transfer: could not copy {}: {error}", path.display());
        if self.first_error.is_none() {
            self.first_error = Some(format!("{}: {error}", path.display()));
        }
    }
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Copies beside the destination and renames, so a reader never sees half a file.
fn copy_file(source: &Path, destination: &Path) -> io::Result<u64> {
    let staging = destination.with_extension("transfer-tmp");
    let result = with_retry("copy file", || fs::copy(source, &staging))
        .and_then(|bytes| fs::rename(&staging, destination).map(|()| bytes));
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

/// Copies `source` into `destination`, adding what is missing. With `newer_wins` a file that
/// already exists is replaced when the source copy is newer; otherwise it is kept.
fn sync_tree(source: &Path, destination: &Path, newer_wins: bool, tally: &mut Tally) {
    if let Err(error) = fs::create_dir_all(destination) {
        tally.fail(destination, &error);
        return;
    }
    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) => {
            tally.fail(source, &error);
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if kind.is_dir() {
            sync_tree(&from, &to, newer_wins, tally);
            continue;
        }
        let exists = to.exists();
        if exists && !(newer_wins && modified(&from) > modified(&to)) {
            tally.kept += 1;
            continue;
        }
        match copy_file(&from, &to) {
            Ok(bytes) => {
                tally.bytes += bytes;
                if exists {
                    tally.updated += 1;
                } else {
                    tally.added += 1;
                }
            }
            Err(error) => tally.fail(&from, &error),
        }
    }
}

/// Carries out the plan. Best effort per entry: one locked file or full disk fails that entry
/// and the rest still run. `progress` is told which entry is being copied.
pub fn execute(
    plan: &Plan,
    source: &Path,
    target: &Path,
    progress: &dyn Fn(&str),
) -> Report {
    let mut report = Report::default();
    if let Err(error) = fs::create_dir_all(target) {
        log_error!("transfer: cannot create {}: {error}", target.display());
    }
    for item in &plan.items {
        if !matches!(item.action, Action::Copy | Action::Merge) {
            continue;
        }
        progress(&item.name);
        let from = source.join(&item.name);
        let to = target.join(&item.name);
        let mut tally = Tally::default();
        if item.is_dir {
            // History is the only kind where a newer copy should replace an older one.
            sync_tree(&from, &to, item.kind == Kind::History, &mut tally);
        } else {
            match copy_file(&from, &to) {
                Ok(bytes) => {
                    tally.added = 1;
                    tally.bytes = bytes;
                }
                Err(error) => tally.fail(&from, &error),
            }
        }
        report.outcomes.push(Outcome {
            name: item.name.clone(),
            kind: item.kind,
            action: item.action,
            added: tally.added,
            updated: tally.updated,
            kept: tally.kept,
            bytes: tally.bytes,
            failed: tally.failed,
            error: tally.first_error,
        });
    }
    log_info!(
        "transfer {} -> {}: {} file(s) added, {} updated, {} failed",
        source.display(),
        target.display(),
        report.added(),
        report.updated(),
        report.failed()
    );
    report
}

/// Folders that hold no account data and are identical for every account, so one copy can
/// serve all profiles.
pub const SHARED_RUNTIME: [&str; 3] = ["vm_bundles", "claude-code-vm", "claude-code"];

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareState {
    /// A real folder inside the profile.
    Local,
    /// A junction into the shared folder.
    Shared,
    /// Nothing there.
    Missing,
}

fn links_to(path: &Path, target: &Path) -> bool {
    junction::exists(path).unwrap_or(false)
        && junction::get_target(path).is_ok_and(|found| crate::paths::same_path(&found, target))
}

#[cfg(test)]
pub fn share_state(profile: &Path, shared_root: &Path, name: &str) -> ShareState {
    let path = profile.join(name);
    if links_to(&path, &shared_root.join(name)) {
        ShareState::Shared
    } else if fs::symlink_metadata(&path).is_ok() {
        ShareState::Local
    } else {
        ShareState::Missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    /// Moved into the shared folder and linked back.
    Moved,
    /// Had nothing; now linked to the shared folder.
    Linked,
    AlreadyShared,
    /// Has its own copy and the shared folder already exists, so nothing was deleted.
    KeptOwnCopy,
    /// Link removed (and, for the first account, the folder moved back).
    Restored,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ShareResult {
    pub profile: String,
    pub name: String,
    pub outcome: ShareOutcome,
}

fn remove_link(path: &Path) -> io::Result<()> {
    with_retry("remove junction", || junction::delete(path))?;
    // `junction::delete` leaves an empty directory behind.
    with_retry("remove junction folder", || fs::remove_dir(path))
}

/// Moves each profile's sandbox and CLI builds into `shared_root` and links them back, so the
/// 11 GB Linux sandbox exists once. Nothing is ever deleted: a profile with its own copy while a
/// shared one exists keeps it. Claude must be closed.
pub fn share_runtime(profiles: &[(String, PathBuf)], shared_root: &Path) -> Vec<ShareResult> {
    let mut results = Vec::new();
    let mut note = |profile: &str, name: &str, outcome: ShareOutcome| {
        results.push(ShareResult {
            profile: profile.to_string(),
            name: name.to_string(),
            outcome,
        });
    };
    for name in SHARED_RUNTIME {
        let shared = shared_root.join(name);
        // First pass moves real folders in; a second links the accounts that had none.
        for (label, dir) in profiles {
            let path = dir.join(name);
            if links_to(&path, &shared) {
                note(label, name, ShareOutcome::AlreadyShared);
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                note(label, name, ShareOutcome::KeptOwnCopy);
                continue;
            }
            if shared.exists() {
                note(label, name, ShareOutcome::KeptOwnCopy);
                continue;
            }
            let moved = fs::create_dir_all(shared_root)
                .and_then(|()| with_retry("move to shared folder", || fs::rename(&path, &shared)));
            if let Err(error) = moved {
                note(label, name, ShareOutcome::Failed(error.to_string()));
                continue;
            }
            match with_retry("link shared folder", || junction::create(&shared, &path)) {
                Ok(()) => note(label, name, ShareOutcome::Moved),
                Err(error) => {
                    // Put it back rather than leave the account without its folder.
                    let _ = fs::rename(&shared, &path);
                    note(label, name, ShareOutcome::Failed(error.to_string()));
                }
            }
        }
        if !shared.exists() {
            continue;
        }
        for (label, dir) in profiles {
            let path = dir.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                continue;
            }
            match with_retry("link shared folder", || junction::create(&shared, &path)) {
                Ok(()) => note(label, name, ShareOutcome::Linked),
                Err(error) => note(label, name, ShareOutcome::Failed(error.to_string())),
            }
        }
    }
    log_info!(
        "shared runtime folders: {} change(s)",
        results
            .iter()
            .filter(|r| !matches!(r.outcome, ShareOutcome::AlreadyShared | ShareOutcome::KeptOwnCopy))
            .count()
    );
    results
}

/// Undoes `share_runtime`: every link is removed and the shared folder goes back to the first
/// account that used it. Other accounts get an empty slot, which Claude refills when needed.
pub fn unshare_runtime(profiles: &[(String, PathBuf)], shared_root: &Path) -> Vec<ShareResult> {
    let mut results = Vec::new();
    for name in SHARED_RUNTIME {
        let shared = shared_root.join(name);
        let mut restore_to: Option<(&str, PathBuf)> = None;
        for (label, dir) in profiles {
            let path = dir.join(name);
            if !links_to(&path, &shared) {
                continue;
            }
            let outcome = match remove_link(&path) {
                Ok(()) => {
                    if restore_to.is_none() {
                        restore_to = Some((label.as_str(), path));
                    }
                    ShareOutcome::Restored
                }
                Err(error) => ShareOutcome::Failed(error.to_string()),
            };
            results.push(ShareResult { profile: label.clone(), name: name.to_string(), outcome });
        }
        if let Some((label, path)) = restore_to {
            if shared.exists() {
                if let Err(error) = with_retry("restore folder", || fs::rename(&shared, &path)) {
                    results.push(ShareResult {
                        profile: label.to_string(),
                        name: name.to_string(),
                        outcome: ShareOutcome::Failed(error.to_string()),
                    });
                }
            }
        }
    }
    results
}

/// Human-readable size: `11.3 GB`, `564 MB`, `12 KB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cst-xfer-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(root: &Path, rel: &str, content: &str) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    fn age(path: &Path, seconds_ago: u64) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
            .unwrap();
    }

    #[test]
    fn entries_are_classified_by_what_they_are_for() {
        assert_eq!(classify("vm_bundles"), Kind::Runtime);
        assert_eq!(classify("claude-code-vm"), Kind::Runtime);
        assert_eq!(classify("Cache"), Kind::Cache);
        assert_eq!(classify("Local Storage"), Kind::Login);
        assert_eq!(classify("claude-code-sessions"), Kind::History);
        assert_eq!(classify("claude_desktop_config.json"), Kind::Settings);
        assert_eq!(classify("something-new"), Kind::Other);
    }

    #[test]
    fn minimal_takes_settings_and_history_and_leaves_the_sandbox() {
        let source = scratch("min-src");
        put(&source, "claude-code-sessions/a/o/local_1.json", "s");
        put(&source, "claude_desktop_config.json", "{}");
        put(&source, "vm_bundles/claudevm.bundle/rootfs", "big");
        put(&source, "Local Storage/x", "login");
        put(&source, "Cache/c", "c");

        let plan = plan(&scan(&source), &[], Mode::Minimal, &HashMap::new());
        let action = |name: &str| plan.items.iter().find(|i| i.name == name).unwrap().action;

        assert_eq!(action("claude-code-sessions"), Action::Copy);
        assert_eq!(action("claude_desktop_config.json"), Action::Copy);
        assert_eq!(action("vm_bundles"), Action::Skip);
        assert_eq!(action("Local Storage"), Action::Skip);
        assert_eq!(action("Cache"), Action::Skip);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn full_includes_the_sandbox_but_not_caches_and_overrides_win() {
        let source = scratch("full-src");
        put(&source, "vm_bundles/x", "big");
        put(&source, "Cache/c", "c");
        put(&source, "Local Storage/x", "login");

        let none = HashMap::new();
        let full = plan(&scan(&source), &[], Mode::Full, &none);
        let action = |plan: &Plan, name: &str| plan.items.iter().find(|i| i.name == name).unwrap().action;
        assert_eq!(action(&full, "vm_bundles"), Action::Copy);
        assert_eq!(action(&full, "Cache"), Action::Skip);

        let mut overrides = HashMap::new();
        overrides.insert("vm_bundles".to_string(), false);
        overrides.insert("Cache".to_string(), true);
        let tuned = plan(&scan(&source), &[], Mode::Full, &overrides);
        assert_eq!(action(&tuned, "vm_bundles"), Action::Skip);
        assert_eq!(action(&tuned, "Cache"), Action::Copy);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn a_login_the_target_already_has_is_kept_and_history_is_merged() {
        let (source, target) = (scratch("keep-src"), scratch("keep-dst"));
        put(&source, "Local Storage/leveldb/1", "source-login");
        put(&source, "claude-code-sessions/a/o/local_new.json", "new");
        put(&source, "claude-code-sessions/a/o/local_shared.json", "newer");
        age(&source.join("claude-code-sessions/a/o/local_shared.json"), 5);
        put(&target, "Local Storage/leveldb/1", "target-login");
        put(&target, "claude-code-sessions/a/o/local_shared.json", "older");
        age(&target.join("claude-code-sessions/a/o/local_shared.json"), 600);
        put(&target, "claude-code-sessions/a/o/local_mine.json", "mine");

        let plan = plan(&scan(&source), &scan(&target), Mode::Full, &HashMap::new());
        let report = execute(&plan, &source, &target, &|_| {});

        assert_eq!(report.failed(), 0);
        assert_eq!(fs::read_to_string(target.join("Local Storage/leveldb/1")).unwrap(), "target-login");
        let sessions = target.join("claude-code-sessions/a/o");
        assert_eq!(fs::read_to_string(sessions.join("local_new.json")).unwrap(), "new");
        assert_eq!(fs::read_to_string(sessions.join("local_shared.json")).unwrap(), "newer");
        assert_eq!(fs::read_to_string(sessions.join("local_mine.json")).unwrap(), "mine");
        let history = report.outcomes.iter().find(|o| o.name == "claude-code-sessions").unwrap();
        assert_eq!((history.added, history.updated), (1, 1));
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn runtime_data_adds_missing_files_but_never_overwrites() {
        let (source, target) = (scratch("rt-src"), scratch("rt-dst"));
        put(&source, "claude-code/2.1.299/claude.exe", "new-build");
        put(&source, "claude-code/2.1.295/claude.exe", "from-source");
        put(&target, "claude-code/2.1.295/claude.exe", "from-target");
        age(&target.join("claude-code/2.1.295/claude.exe"), 9000);

        let plan = plan(&scan(&source), &scan(&target), Mode::Full, &HashMap::new());
        execute(&plan, &source, &target, &|_| {});

        assert_eq!(fs::read_to_string(target.join("claude-code/2.1.299/claude.exe")).unwrap(), "new-build");
        assert_eq!(fs::read_to_string(target.join("claude-code/2.1.295/claude.exe")).unwrap(), "from-target");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn a_dry_plan_changes_nothing() {
        let (source, target) = (scratch("dry-src"), scratch("dry-dst"));
        put(&source, "claude-code-sessions/a/o/local_1.json", "s");

        let plan = plan(&scan(&source), &scan(&target), Mode::Minimal, &HashMap::new());

        assert!(plan.writes_anything());
        assert!(scan(&target).is_empty());
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn sharing_moves_the_sandbox_once_and_links_every_account() {
        let base = scratch("share");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "vm_bundles/claudevm.bundle/rootfs", "linux");
        put(&one, "config.json", "{}");
        fs::create_dir_all(&two).unwrap();
        let shared = base.join("shared");
        let profiles = vec![("One".to_string(), one.clone()), ("Two".to_string(), two.clone())];

        let first = share_runtime(&profiles, &shared);

        assert!(first.iter().any(|r| r.profile == "One" && r.outcome == ShareOutcome::Moved));
        assert!(first.iter().any(|r| r.profile == "Two" && r.name == "vm_bundles" && r.outcome == ShareOutcome::Linked));
        assert_eq!(share_state(&one, &shared, "vm_bundles"), ShareState::Shared);
        assert_eq!(share_state(&two, &shared, "vm_bundles"), ShareState::Shared);
        assert_eq!(fs::read_to_string(two.join("vm_bundles/claudevm.bundle/rootfs")).unwrap(), "linux");
        // Folders nobody had are not invented.
        assert_eq!(share_state(&one, &shared, "claude-code"), ShareState::Missing);

        let second = share_runtime(&profiles, &shared);
        assert!(second.iter().all(|r| r.outcome == ShareOutcome::AlreadyShared));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sharing_never_deletes_a_second_real_copy() {
        let base = scratch("share-keep");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "vm_bundles/a", "from-one");
        put(&two, "vm_bundles/a", "from-two");
        let shared = base.join("shared");
        let profiles = vec![("One".to_string(), one), ("Two".to_string(), two.clone())];

        let results = share_runtime(&profiles, &shared);

        assert!(results.iter().any(|r| r.profile == "Two" && r.outcome == ShareOutcome::KeptOwnCopy));
        assert_eq!(fs::read_to_string(two.join("vm_bundles/a")).unwrap(), "from-two");
        assert_eq!(share_state(&two, &shared, "vm_bundles"), ShareState::Local);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unsharing_gives_the_folder_back_to_the_first_account() {
        let base = scratch("unshare");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "vm_bundles/a", "linux");
        fs::create_dir_all(&two).unwrap();
        let shared = base.join("shared");
        let profiles = vec![("One".to_string(), one.clone()), ("Two".to_string(), two.clone())];
        share_runtime(&profiles, &shared);

        let results = unshare_runtime(&profiles, &shared);

        assert!(results.iter().all(|r| r.outcome == ShareOutcome::Restored));
        assert_eq!(share_state(&one, &shared, "vm_bundles"), ShareState::Local);
        assert_eq!(fs::read_to_string(one.join("vm_bundles/a")).unwrap(), "linux");
        assert_eq!(share_state(&two, &shared, "vm_bundles"), ShareState::Missing);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_shared_target_folder_is_kept_not_merged_into() {
        let base = scratch("keep-link");
        let (source, target) = (base.join("src"), base.join("dst"));
        put(&source, "vm_bundles/x", "other");
        put(&base, "shared/vm_bundles/y", "shared");
        fs::create_dir_all(&target).unwrap();
        junction::create(base.join("shared/vm_bundles"), target.join("vm_bundles")).unwrap();

        let plan = plan(&scan(&source), &scan(&target), Mode::Full, &HashMap::new());

        assert_eq!(plan.items[0].action, Action::Keep);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(12 * 1024), "12.0 KB");
        assert_eq!(format_bytes(564 * 1024 * 1024), "564 MB");
        assert_eq!(format_bytes(11 * 1024 * 1024 * 1024 + 300 * 1024 * 1024), "11.3 GB");
    }
}
