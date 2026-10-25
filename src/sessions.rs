//! Shares Claude Desktop's local "Code" session list between accounts.
//!
//! Desktop lists Code sessions from
//! `claude-code-sessions\<accountId>\<orgId>\local_<id>.json` inside its data
//! directory, so every account only ever sees its own folder. The transcripts those
//! files point at (`cliSessionId`) live in `~/.claude/projects`, which every account
//! already shares; only these small index files are per account. Mirroring them into
//! every account folder makes one history show up under every login.
//!
//! A deleted session leaves a `deleted_<id>` marker instead of its `local_<id>.json`;
//! markers are mirrored too, and the matching session file is removed everywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::profiles::copy_dir_recursive;

pub const SESSIONS_DIR: &str = "claude-code-sessions";
const DELETED_PREFIX: &str = "deleted_";
const BACKUP_PREFIX: &str = "auto-sessions-";

/// What a sync would change; computed first so nothing is touched without a backup.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// `(source, destination)`; the destination is missing or older than the source.
    pub copies: Vec<(PathBuf, PathBuf)>,
    /// Session files whose deletion was recorded in another account.
    pub removals: Vec<PathBuf>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.copies.is_empty() && self.removals.is_empty()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub copied: usize,
    pub removed: usize,
    pub failed: usize,
}

fn session_id(name: &str) -> Option<&str> {
    name.strip_prefix("local_")?.strip_suffix(".json")
}

fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Works out what it takes for every account folder in every profile to hold the
/// newest copy of every session. Account folders that exist in any profile are
/// created in all of them, so signing in to a known account elsewhere shows the
/// shared list straight away. `known_folders` adds `(account, org)` pairs that exist in no
/// profile yet, such as an account that has signed in but not opened Code.
pub fn plan(profiles: &[PathBuf], known_folders: &[(String, String)]) -> Plan {
    let mut keys: BTreeSet<(String, String)> = known_folders.iter().cloned().collect();
    let mut newest: BTreeMap<String, (SystemTime, PathBuf)> = BTreeMap::new();
    let mut markers: BTreeMap<String, PathBuf> = BTreeMap::new();

    for profile in profiles {
        let root = profile.join(SESSIONS_DIR);
        for account in subdirs(&root) {
            for org in subdirs(&root.join(&account)) {
                let Ok(entries) = fs::read_dir(root.join(&account).join(&org)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Ok(name) = entry.file_name().into_string() else {
                        continue;
                    };
                    let path = entry.path();
                    if name.starts_with(DELETED_PREFIX) {
                        markers.entry(name).or_insert(path);
                    } else if session_id(&name).is_some() {
                        let mtime = modified(&path);
                        if newest.get(&name).is_none_or(|(best, _)| mtime > *best) {
                            newest.insert(name, (mtime, path));
                        }
                    }
                }
                keys.insert((account.clone(), org));
            }
        }
    }

    let deleted: BTreeSet<&str> = markers
        .keys()
        .filter_map(|name| name.strip_prefix(DELETED_PREFIX))
        .collect();
    newest.retain(|name, _| session_id(name).is_none_or(|id| !deleted.contains(id)));

    let mut plan = Plan::default();
    for profile in profiles {
        for (account, org) in &keys {
            let dir = profile.join(SESSIONS_DIR).join(account).join(org);
            for (name, source) in &markers {
                let destination = dir.join(name);
                if !destination.exists() {
                    plan.copies.push((source.clone(), destination));
                }
            }
            for id in &deleted {
                let stale = dir.join(format!("local_{id}.json"));
                if stale.exists() {
                    plan.removals.push(stale);
                }
            }
            for (name, (mtime, source)) in &newest {
                let destination = dir.join(name);
                if *source != destination
                    && (!destination.exists() || modified(&destination) < *mtime)
                {
                    plan.copies.push((source.clone(), destination));
                }
            }
        }
    }
    plan
}

/// How many distinct sessions exist across all profiles, not counting deleted ones. This is
/// how many every account would show once the lists are shared.
pub fn live_count(profiles: &[PathBuf]) -> usize {
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut deleted: BTreeSet<String> = BTreeSet::new();
    for profile in profiles {
        let root = profile.join(SESSIONS_DIR);
        for account in subdirs(&root) {
            for org in subdirs(&root.join(&account)) {
                let Ok(entries) = fs::read_dir(root.join(&account).join(&org)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Ok(name) = entry.file_name().into_string() else {
                        continue;
                    };
                    if let Some(id) = name.strip_prefix(DELETED_PREFIX) {
                        deleted.insert(id.to_string());
                    } else if let Some(id) = session_id(&name) {
                        live.insert(id.to_string());
                    }
                }
            }
        }
    }
    live.difference(&deleted).count()
}

/// Carries out a plan. Best effort: one locked file must not stop the rest, and a
/// file that fails is simply picked up by the next sync.
pub fn apply(plan: &Plan) -> Report {
    let mut report = Report::default();
    for (source, destination) in &plan.copies {
        // Copy beside the target and rename, so Desktop never reads half a file.
        let staging = destination.with_extension("sync-tmp");
        let result = destination
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::copy(source, &staging))
            .and_then(|_| fs::rename(&staging, destination));
        match result {
            Ok(()) => report.copied += 1,
            Err(error) => {
                let _ = fs::remove_file(&staging);
                log_warn!("could not copy session {}: {error}", destination.display());
                report.failed += 1;
            }
        }
    }
    for path in &plan.removals {
        match fs::remove_file(path) {
            Ok(()) => report.removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                log_warn!("could not remove deleted session {}: {error}", path.display());
                report.failed += 1;
            }
        }
    }
    report
}

/// Copies every profile's session list into `backups_root` and keeps the newest
/// `keep` such snapshots. Returns `None` when there was nothing to copy.
pub fn backup(profiles: &[PathBuf], backups_root: &Path, keep: usize) -> io::Result<Option<PathBuf>> {
    let sources: Vec<(usize, &PathBuf)> = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.join(SESSIONS_DIR).is_dir())
        .collect();
    if sources.is_empty() {
        return Ok(None);
    }

    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let target = backups_root.join(format!("{BACKUP_PREFIX}{seconds}"));
    for (index, profile) in sources {
        let label = profile
            .file_name()
            .map_or_else(|| "profile".into(), |name| name.to_string_lossy());
        copy_dir_recursive(
            &profile.join(SESSIONS_DIR),
            &target.join(format!("{index}_{label}")).join(SESSIONS_DIR),
        )?;
    }
    log_info!("backed up Code session lists to {}", target.display());

    prune(backups_root, keep);
    Ok(Some(target))
}

/// Removes the oldest automatic snapshots; backups made by hand are never touched.
fn prune(backups_root: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(backups_root) else {
        return;
    };
    // Ordered by the timestamp in the name, numerically, so the newest is always kept.
    let mut snapshots: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let seconds = name.strip_prefix(BACKUP_PREFIX)?.parse().ok()?;
            Some((seconds, entry.path()))
        })
        .collect();
    snapshots.sort();
    let excess = snapshots.len().saturating_sub(keep);
    for (_, old) in snapshots.into_iter().take(excess) {
        if let Err(error) = fs::remove_dir_all(&old) {
            log_warn!("could not prune old backup {}: {error}", old.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply, backup, live_count, plan, SESSIONS_DIR};
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cst-sess-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(profile: &Path, account: &str, org: &str, file: &str, content: &str) -> PathBuf {
        let dir = profile.join(SESSIONS_DIR).join(account).join(org);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file);
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

    fn read(profile: &Path, account: &str, org: &str, file: &str) -> Option<String> {
        fs::read_to_string(profile.join(SESSIONS_DIR).join(account).join(org).join(file)).ok()
    }

    #[test]
    fn every_session_reaches_every_account_folder_in_every_profile() {
        let base = scratch("union");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "acctA", "org1", "local_a.json", "from-one");
        put(&two, "acctB", "org2", "local_b.json", "from-two");

        let first = plan(&[one.clone(), two.clone()], &[]);
        assert!(!first.is_empty());
        let report = apply(&first);
        assert_eq!(report.failed, 0);

        for profile in [&one, &two] {
            for (account, org) in [("acctA", "org1"), ("acctB", "org2")] {
                assert_eq!(read(profile, account, org, "local_a.json").as_deref(), Some("from-one"));
                assert_eq!(read(profile, account, org, "local_b.json").as_deref(), Some("from-two"));
            }
        }
        // Copies keep their timestamps, so a second pass has nothing left to do.
        assert!(plan(&[one, two], &[]).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_account_adds_to_a_full_one_and_never_takes_from_it() {
        let base = scratch("empty");
        let (full, empty) = (base.join("full"), base.join("empty"));
        for id in 0..5 {
            put(&full, "acctA", "org1", &format!("local_{id}.json"), "x");
        }
        put(&empty, "acctB", "org1", "keep.txt", "unrelated");

        let profiles = [full.clone(), empty.clone()];
        assert_eq!(live_count(&profiles), 5);
        let first = plan(&profiles, &[]);
        assert!(first.removals.is_empty());
        apply(&first);

        for id in 0..5 {
            let name = format!("local_{id}.json");
            assert!(read(&full, "acctA", "org1", &name).is_some());
            assert!(read(&empty, "acctA", "org1", &name).is_some());
            assert!(read(&empty, "acctB", "org1", &name).is_some());
        }
        assert_eq!(live_count(&profiles), 5);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn different_sessions_in_two_accounts_add_up_and_shared_ones_count_once() {
        let base = scratch("count");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "acctA", "org1", "local_a.json", "1");
        put(&one, "acctA", "org1", "local_shared.json", "1");
        put(&two, "acctB", "org1", "local_shared.json", "2");
        put(&two, "acctB", "org1", "local_b.json", "2");
        put(&two, "acctB", "org1", "local_gone.json", "2");
        put(&one, "acctA", "org1", "deleted_gone", "1");

        assert_eq!(live_count(&[one, two]), 3);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_known_account_with_no_folder_yet_gets_the_shared_list() {
        let base = scratch("known");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "acctA", "org1", "local_a.json", "from-one");
        fs::create_dir_all(&two).unwrap();

        apply(&plan(
            &[one.clone(), two.clone()],
            &[("acctB".to_string(), "org2".to_string())],
        ));

        for profile in [&one, &two] {
            assert_eq!(read(profile, "acctB", "org2", "local_a.json").as_deref(), Some("from-one"));
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_newest_copy_of_a_session_wins() {
        let base = scratch("newest");
        let (one, two) = (base.join("one"), base.join("two"));
        age(&put(&one, "acct", "org", "local_s.json", "old"), 600);
        age(&put(&two, "acct", "org", "local_s.json", "new"), 5);

        apply(&plan(&[one.clone(), two.clone()], &[]));

        assert_eq!(read(&one, "acct", "org", "local_s.json").as_deref(), Some("new"));
        assert_eq!(read(&two, "acct", "org", "local_s.json").as_deref(), Some("new"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_deleted_session_disappears_everywhere_and_is_not_resurrected() {
        let base = scratch("deleted");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "acct", "org", "deleted_x", "1791496299030");
        put(&two, "acct", "org", "local_x.json", "still-here");
        put(&two, "acct", "org", "local_keep.json", "keep");

        apply(&plan(&[one.clone(), two.clone()], &[]));

        assert_eq!(read(&two, "acct", "org", "local_x.json"), None);
        assert_eq!(read(&one, "acct", "org", "local_x.json"), None);
        assert!(read(&two, "acct", "org", "deleted_x").is_some());
        assert_eq!(read(&one, "acct", "org", "local_keep.json").as_deref(), Some("keep"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn files_that_are_not_sessions_are_left_alone() {
        let base = scratch("others");
        let (one, two) = (base.join("one"), base.join("two"));
        put(&one, "acct", "org", "scheduled-tasks.json", "tasks");
        put(&two, "acct", "org", "local_s.json", "s");

        apply(&plan(&[one.clone(), two.clone()], &[]));

        assert_eq!(read(&two, "acct", "org", "scheduled-tasks.json"), None);
        assert_eq!(read(&one, "acct", "org", "scheduled-tasks.json").as_deref(), Some("tasks"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn backups_keep_the_newest_snapshots_and_never_touch_manual_ones() {
        let base = scratch("backup");
        let profile = base.join("profile");
        put(&profile, "acct", "org", "local_s.json", "s");
        let backups = base.join("backups");
        for old in ["auto-sessions-1", "auto-sessions-2", "auto-sessions-3", "manual-copy"] {
            fs::create_dir_all(backups.join(old)).unwrap();
        }

        let made = backup(&[profile], &backups, 2).unwrap().unwrap();

        assert_eq!(
            fs::read_to_string(made.join("0_profile").join(SESSIONS_DIR).join("acct/org/local_s.json")).unwrap(),
            "s"
        );
        assert!(!backups.join("auto-sessions-1").exists());
        assert!(!backups.join("auto-sessions-2").exists());
        assert!(backups.join("auto-sessions-3").exists());
        assert!(backups.join("manual-copy").exists());
        assert!(made.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn nothing_to_back_up_creates_nothing() {
        let base = scratch("nobackup");
        let made = backup(&[base.join("empty-profile")], &base.join("backups"), 5).unwrap();
        assert_eq!(made, None);
        assert!(!base.join("backups").exists());
        let _ = fs::remove_dir_all(&base);
    }
}
