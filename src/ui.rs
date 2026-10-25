//! GPUI desktop dashboard for ClaudeSyncTray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, rgb, rgba, size, AnyElement, App, Application, Bounds, ClipboardItem,
    Context, Div, ElementId, FocusHandle, FontWeight, KeyDownEvent, PathPromptOptions,
    SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowOptions,
};

use crate::claude::{self, LaunchTarget};
use crate::config::{Config, LaunchConfig, ProfileConfig};
use crate::profiles::{LiveState, ProfileContents, ProfileManager, SwitchOutcome};
use crate::tray::{TrayAction, TrayController};
use crate::dither::{self, Glyph, Sky, SkyFrame};
use crate::{icon, logging, paths, process, startup};

mod data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Profiles,
    Settings,
    Data,
    Logs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditingField {
    RenameProfile(usize),
    NewProfileName,
    LiveOverride,
    LaunchExe,
    LaunchCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
struct StatusBanner {
    kind: BannerKind,
    text: String,
    shown_at: Instant,
}

impl StatusBanner {
    /// Confirmations fade on their own; warnings and errors stay until dismissed.
    fn expired(&self) -> bool {
        matches!(self.kind, BannerKind::Info | BannerKind::Success)
            && self.shown_at.elapsed() > Duration::from_secs(6)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkHealth {
    Healthy,
    NotLinked,
    Broken,
}

/// Disk-derived facts the UI shows, cached so animation frames don't touch the
/// file system.
struct Snapshot {
    taken: Instant,
    /// Reading every session file is slower than the rest, so it is refreshed less often.
    contents_taken: Instant,
    profiles: Vec<ProfileFacts>,
    link: LinkHealth,
}

#[derive(Debug, Clone, Default)]
struct ProfileFacts {
    detail: String,
    cli_auth: bool,
    desktop_account: Option<String>,
    contents: ProfileContents,
}

const CONTENTS_REFRESH: Duration = Duration::from_secs(10);

/// Animation / polling cadence (25 fps; plenty for a dither, cheap on CPU).
const FRAME_INTERVAL: Duration = Duration::from_millis(40);

/// How long the switch dissolve takes.
const DISSOLVE: Duration = Duration::from_millis(1400);

/// Animation state for the dithered sky on the Accounts page.
struct SkyState {
    /// Account the sky currently shows.
    shown: Option<usize>,
    /// Sky being dissolved away, and when the dissolve started.
    dissolve: Option<(Option<Sky>, Instant)>,
    clock: f32,
    last_tick: Instant,
    /// Lets the sky replay its reveal each time the dashboard is opened.
    was_visible: bool,
}

pub struct DashboardView {
    cfg: Config,
    manager: ProfileManager,
    tray: Option<TrayController>,
    focus_handle: FocusHandle,
    allow_close: Arc<AtomicBool>,
    tab: ActiveTab,
    busy_message: Option<String>,
    banner: Option<StatusBanner>,
    editing: Option<EditingField>,
    edit_buffer: String,
    confirm_delete: Option<usize>,
    expanded: Option<usize>,
    claude_pids: usize,
    autostart_enabled: bool,
    log_lines: Vec<String>,
    snapshot: Option<Snapshot>,
    sky: SkyState,
    data: data::DataState,
}

impl DashboardView {
    fn new(
        cfg: Config,
        manager: ProfileManager,
        tray: Option<TrayController>,
        allow_close: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let claude_pids = process::find_claude_desktop().len();
        let autostart_enabled = startup::is_enabled();
        let log_lines = logging::recent_lines(120);

        Self {
            cfg,
            manager,
            tray,
            focus_handle,
            allow_close,
            tab: ActiveTab::Profiles,
            busy_message: None,
            banner: None,
            editing: None,
            edit_buffer: String::new(),
            confirm_delete: None,
            expanded: None,
            claude_pids,
            autostart_enabled,
            log_lines,
            snapshot: None,
            sky: SkyState {
                shown: None,
                dissolve: None,
                clock: 0.0,
                last_tick: Instant::now(),
                was_visible: false,
            },
            data: data::DataState::default(),
        }
    }

    fn set_banner(&mut self, kind: BannerKind, text: impl Into<String>) {
        self.banner = Some(StatusBanner {
            kind,
            text: text.into(),
            shown_at: Instant::now(),
        });
    }

    fn link_health(&self) -> LinkHealth {
        match self.manager.live_state() {
            Ok(LiveState::Junction(target))
                if paths::same_path(&target, &self.manager.active_dir()) && target.exists() =>
            {
                LinkHealth::Healthy
            }
            Ok(LiveState::RealDir) | Ok(LiveState::Missing) => LinkHealth::NotLinked,
            _ => LinkHealth::Broken,
        }
    }

    fn ensure_snapshot(&mut self) {
        let fresh = self.snapshot.as_ref().is_some_and(|snap| {
            snap.taken.elapsed() < Duration::from_secs(2)
                && snap.profiles.len() == self.manager.profiles().len()
        });
        if fresh {
            return;
        }
        let reusable = self.snapshot.as_ref().filter(|snap| {
            snap.contents_taken.elapsed() < CONTENTS_REFRESH
                && snap.profiles.len() == self.manager.profiles().len()
        });
        let contents_taken = reusable.map_or_else(Instant::now, |snap| snap.contents_taken);
        // `~/.claude.json` belongs to whichever account is active, so only that profile may use it.
        let live_login = crate::profiles::live_cli_account();
        let active = self.manager.active();
        let profiles = self
            .manager
            .profiles()
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let live = live_login.as_ref().filter(|_| index == active);
                ProfileFacts {
                desktop_account: profile.desktop_account_id(),
                contents: reusable
                    .map(|snap| snap.profiles[index].contents.clone())
                    .unwrap_or_else(|| profile.contents(live)),
                detail: match profile.account_email(live) {
                    Some(email) => email,
                    None if profile.desktop_account_id().is_some() => format!(
                        "Signed in · account {}",
                        profile.desktop_account_short().unwrap_or_default()
                    ),
                    None if profile.has_desktop_auth() => "Signed in to Claude Desktop".into(),
                    None if profile.has_session_data() => "Has saved session data".into(),
                    None => "Not signed in yet".into(),
                },
                cli_auth: profile.has_cli_auth(),
                }
            })
            .collect();
        self.snapshot = Some(Snapshot {
            taken: Instant::now(),
            contents_taken,
            profiles,
            link: self.link_health(),
        });
    }

    fn facts(&self, index: usize) -> ProfileFacts {
        self.snapshot
            .as_ref()
            .and_then(|snap| snap.profiles.get(index).cloned())
            .unwrap_or_default()
    }

    fn cached_link(&self) -> LinkHealth {
        self.snapshot
            .as_ref()
            .map_or(LinkHealth::Healthy, |snap| snap.link)
    }

    /// Starts a dissolve whenever the active account changes (including the
    /// very first frame, which dissolves in from an empty sky).
    fn sync_sky(&mut self) {
        let active = self.manager.active();
        if self.sky.shown != Some(active) {
            let from = self
                .sky
                .shown
                .map(|index| Sky::for_account(index, account_hex(index)));
            self.sky.shown = Some(active);
            self.sky.dissolve = Some((from, Instant::now()));
        }
        if self
            .sky
            .dissolve
            .is_some_and(|(_, started)| started.elapsed() >= DISSOLVE)
        {
            self.sky.dissolve = None;
        }
    }

    fn sky_frame(&self) -> SkyFrame {
        let active = self.manager.active();
        let (from, progress) = match self.sky.dissolve {
            Some((from, started)) => (
                from,
                started.elapsed().as_secs_f32() / DISSOLVE.as_secs_f32(),
            ),
            None => (None, 1.0),
        };
        SkyFrame {
            sky: Sky::for_account(active, account_hex(active)),
            from: if progress < 1.0 { from } else { None },
            progress: progress.min(1.0),
            time: self.sky.clock,
        }
    }

    /// Whether anything on screen is moving. The logo and the idle sky only move
    /// while the window has focus, so a dashboard left open in the background costs
    /// nothing.
    fn wants_frames(&self, window: &Window) -> bool {
        if !process::window_visible(window) {
            return false;
        }
        self.busy_message.is_some() || self.sky.dissolve.is_some() || window.is_window_active()
    }

    fn advance_clock(&mut self) {
        let now = Instant::now();
        let dt = (now - self.sky.last_tick).as_secs_f32().min(0.1);
        self.sky.last_tick = now;
        // Clouds race while a switch is running.
        self.sky.clock += dt * if self.busy_message.is_some() { 5.0 } else { 1.0 };
    }

    fn sync_all(&mut self) {
        self.snapshot = None;
        self.manager.refresh_active_from_junction();
        self.cfg.active = Some(self.manager.active());
        self.cfg.save();
        self.autostart_enabled = startup::is_enabled();
        self.claude_pids = process::find_claude_desktop().len();
        if let Some(tray) = &self.tray {
            tray.sync(&self.manager);
        }
        if self.tab == ActiveTab::Logs {
            self.log_lines = logging::recent_lines(120);
        }
    }

    fn reload_manager(&mut self) {
        if let Err(error) = self.manager.reload_from_config(&self.cfg) {
            log_error!("could not reload profile manager: {error}");
            self.set_banner(BannerKind::Error, format!("Failed to reload profiles: {error}"));
        }
        self.sync_all();
    }

    fn handle_tray_action(
        &mut self,
        action: TrayAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            TrayAction::OpenDashboard => {
                process::show_window(window);
                cx.notify();
            }
            TrayAction::ToggleDashboard => {
                process::toggle_window(window);
                cx.notify();
            }
            TrayAction::SwitchAccount(index) => {
                self.trigger_switch(index, true, window, cx);
            }
            TrayAction::RestartClaude => {
                self.trigger_restart_claude(window, cx);
            }
            TrayAction::RepairLink => {
                self.trigger_repair_link(window, cx);
            }
            TrayAction::ToggleAutostart => {
                self.toggle_autostart(cx);
            }
            TrayAction::OpenCodeFolder => {
                self.open_code_folder();
                cx.notify();
            }
            TrayAction::OpenProfilesFolder => {
                process::open_path(&paths::profiles_root());
            }
            TrayAction::OpenLogFile => {
                process::open_path(&paths::log_file());
            }
            TrayAction::Quit => {
                self.quit_app(cx);
            }
        }
    }

    fn poll_external_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = process::take_command() else {
            return;
        };
        let text = text.trim().to_string();
        log_info!("received command: {text}");
        if let Some(index) = text
            .strip_prefix("switch ")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            self.trigger_switch(index, true, window, cx);
        } else if text == "repair" {
            self.trigger_repair_link(window, cx);
        } else if text == "show" {
            process::show_window(window);
            cx.notify();
        } else {
            log_warn!("ignoring unknown command: {text}");
        }
    }

    fn trigger_switch(
        &mut self,
        index: usize,
        launch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy_message.is_some() {
            return;
        }
        let Some(profile) = self.manager.profiles().get(index) else {
            return;
        };
        let profile_name = profile.name.clone();
        self.confirm_delete = None;
        self.busy_message = Some(if launch {
            format!("Switching to '{profile_name}' and relaunching Claude Desktop…")
        } else {
            format!("Switching junction to '{profile_name}'…")
        });
        cx.notify();

        let mut manager_clone = self.manager.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let outcome = manager_clone.switch_to(index, launch);
                    (manager_clone, outcome)
                })
                .await;

            let _ = view.update_in(cx, |this, _window, cx| {
                this.busy_message = None;
                let (updated_manager, outcome) = result;
                this.manager = updated_manager;
                this.apply_switch_outcome(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_switch_outcome(&mut self, outcome: Result<SwitchOutcome, String>) {
        match outcome {
            Ok(outcome) => {
                log_info!(
                    "switched to '{}' (closed {} graceful, {} forced; launched={})",
                    outcome.profile,
                    outcome.killed.graceful,
                    outcome.killed.forced,
                    outcome.launched
                );
                self.sync_all();
                if let Some(error) = outcome.launch_error {
                    self.set_banner(
                        BannerKind::Warning,
                        format!(
                            "Switched to '{}' but Claude could not be launched: {error}",
                            outcome.profile
                        ),
                    );
                } else if outcome.launched {
                    self.set_banner(
                        BannerKind::Success,
                        format!("Active profile is now '{}' (Claude relaunched).", outcome.profile),
                    );
                } else {
                    self.set_banner(
                        BannerKind::Success,
                        format!("Switched junction to '{}' (without launching Claude).", outcome.profile),
                    );
                }
            }
            Err(error) => {
                log_error!("account switch failed: {error}");
                self.sync_all();
                self.set_banner(BannerKind::Error, format!("Account switch failed: {error}"));
            }
        }
    }

    fn trigger_restart_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_message.is_some() {
            return;
        }
        self.busy_message = Some("Restarting Claude Desktop…".into());
        cx.notify();

        let target = self.manager.launch_target().clone();
        let manager = self.manager.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let report = process::terminate_claude(Duration::from_secs(6));
                    log_info!(
                        "manual restart (closed {} graceful, {} forced)",
                        report.graceful,
                        report.forced
                    );
                    std::thread::sleep(Duration::from_millis(250));
                    // Signing in to another account inside Desktop is not a switch, so this is
                    // the point where the shared Code list can reach that account.
                    if manager.share_sessions {
                        if let Err(error) = manager.sync_sessions(true) {
                            log_warn!("could not share Code sessions on restart: {error}");
                        }
                    }
                    claude::launch(&target).map(|()| report).map_err(|e| e.to_string())
                })
                .await;

            let _ = view.update_in(cx, |this, _window, cx| {
                this.busy_message = None;
                this.sync_all();
                match result {
                    Ok(_) => this.set_banner(BannerKind::Success, "Claude Desktop restarted."),
                    Err(error) => this.set_banner(
                        BannerKind::Error,
                        format!("Could not start Claude Desktop: {error}"),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_stop_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_message.is_some() {
            return;
        }
        self.busy_message = Some("Closing Claude Desktop…".into());
        cx.notify();

        cx.spawn_in(window, async move |view, cx| {
            let report = cx
                .background_spawn(async move { process::terminate_claude(Duration::from_secs(6)) })
                .await;

            let _ = view.update_in(cx, |this, _window, cx| {
                this.busy_message = None;
                this.sync_all();
                this.set_banner(
                    BannerKind::Info,
                    format!(
                        "Closed Claude Desktop ({} graceful, {} forced).",
                        report.graceful, report.forced
                    ),
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_repair_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_message.is_some() {
            return;
        }
        self.busy_message = Some("Verifying and repairing profile junction…".into());
        cx.notify();

        let manager_clone = self.manager.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx.background_spawn(async move { manager_clone.repair() }).await;

            let _ = view.update_in(cx, |this, _window, cx| {
                this.busy_message = None;
                this.sync_all();
                match result {
                    Ok(message) => this.set_banner(BannerKind::Success, message),
                    Err(error) => this.set_banner(
                        BannerKind::Error,
                        format!("Could not repair profile link: {error}"),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_side_by_side(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.manager.launch_side_by_side(index) {
            Ok(msg) => {
                self.sync_all();
                self.set_banner(BannerKind::Success, msg);
            }
            Err(err) => {
                self.set_banner(
                    BannerKind::Error,
                    format!("Side-by-side launch failed: {err}"),
                );
            }
        }
        cx.notify();
    }

    fn toggle_share_mcp_config(&mut self, cx: &mut Context<Self>) {
        self.cfg.share_mcp_config = !self.cfg.share_mcp_config;
        self.cfg.save();
        self.reload_manager();
        self.set_banner(
            BannerKind::Success,
            if self.cfg.share_mcp_config {
                "Enabled shared MCP & desktop config across all profiles."
            } else {
                "Disabled shared MCP config."
            },
        );
        cx.notify();
    }

    fn sync_mcp_now(&mut self, cx: &mut Context<Self>) {
        match self.manager.sync_mcp_config() {
            Ok(count) => {
                self.sync_all();
                self.set_banner(
                    BannerKind::Success,
                    format!("Synced claude_desktop_config.json across {count} profile(s)."),
                );
            }
            Err(err) => {
                self.set_banner(
                    BannerKind::Error,
                    format!("Failed to sync MCP config: {err}"),
                );
            }
        }
        cx.notify();
    }

    fn toggle_share_sessions(&mut self, cx: &mut Context<Self>) {
        self.cfg.share_sessions = !self.cfg.share_sessions;
        self.cfg.save();
        self.reload_manager();
        self.set_banner(
            BannerKind::Success,
            if self.cfg.share_sessions {
                "Code sessions will be shared across all accounts on every switch."
            } else {
                "Code sessions are no longer shared across accounts."
            },
        );
        cx.notify();
    }

    fn sync_sessions_now(&mut self, cx: &mut Context<Self>) {
        if !process::find_claude_desktop().is_empty() {
            self.set_banner(
                BannerKind::Warning,
                "Close Claude Desktop first, then sync Code sessions.",
            );
            cx.notify();
            return;
        }
        match self.manager.sync_sessions(true) {
            Ok(report) if report.failed > 0 => self.set_banner(
                BannerKind::Warning,
                format!(
                    "Shared Code sessions, but {} file(s) could not be written; see the log.",
                    report.failed
                ),
            ),
            Ok(report) if report.copied + report.removed == 0 => self.set_banner(
                BannerKind::Success,
                "Code sessions are already the same in every account.",
            ),
            Ok(report) => self.set_banner(
                BannerKind::Success,
                format!(
                    "Shared Code sessions: {} copied, {} removed. A backup was saved first.",
                    report.copied, report.removed
                ),
            ),
            Err(err) => self.set_banner(
                BannerKind::Error,
                format!("Could not share Code sessions: {err}"),
            ),
        }
        cx.notify();
    }

    fn toggle_sync_claude_code(&mut self, cx: &mut Context<Self>) {
        self.cfg.sync_claude_code = !self.cfg.sync_claude_code;
        self.cfg.save();
        self.reload_manager();
        self.set_banner(
            BannerKind::Success,
            if self.cfg.sync_claude_code {
                "Enabled automatic Claude Code CLI credential switching."
            } else {
                "Disabled automatic Claude Code CLI credential switching."
            },
        );
        cx.notify();
    }

    fn snapshot_cli_auth_now(&mut self, cx: &mut Context<Self>) {
        let active = self.manager.active();
        let active_name = self.manager.active_profile().name.clone();
        match self.manager.save_cli_auth_for(active) {
            Ok(true) => {
                self.sync_all();
                self.set_banner(
                    BannerKind::Success,
                    format!("Saved current Claude Code CLI credentials to '{active_name}'."),
                );
            }
            Ok(false) => {
                self.set_banner(
                    BannerKind::Warning,
                    "No Claude Code CLI credentials found in ~/.claude/.credentials.json or ~/.claude.json.",
                );
            }
            Err(err) => {
                self.set_banner(
                    BannerKind::Error,
                    format!("Could not snapshot Claude Code CLI credentials: {err}"),
                );
            }
        }
        cx.notify();
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let desired = !startup::is_enabled();
        match startup::set_enabled(desired) {
            Ok(()) => {
                self.cfg.start_with_windows = desired;
                self.cfg.save();
                self.sync_all();
                self.set_banner(
                    BannerKind::Success,
                    if desired {
                        "Enabled 'Start with Windows'."
                    } else {
                        "Disabled 'Start with Windows'."
                    },
                );
            }
            Err(error) => {
                self.set_banner(
                    BannerKind::Error,
                    format!("Could not update Windows startup entry: {error}"),
                );
            }
        }
        cx.notify();
    }

    fn open_code_folder(&mut self) {
        match paths::claude_code_projects() {
            Some(path) => process::open_path(&path),
            None => self.set_banner(
                BannerKind::Error,
                "Could not resolve your home directory from the environment.",
            ),
        }
    }

    fn begin_edit(&mut self, field: EditingField, initial: String, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Some(field);
        self.edit_buffer = initial;
        self.confirm_delete = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.edit_buffer.clear();
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.editing.take() else {
            return;
        };
        let value = self.edit_buffer.trim().to_string();
        self.edit_buffer.clear();

        match field {
            EditingField::RenameProfile(index) => {
                if value.is_empty() {
                    self.set_banner(BannerKind::Warning, "Profile name cannot be empty.");
                    cx.notify();
                    return;
                }
                if let Some(profile) = self.cfg.profiles.get_mut(index) {
                    let old_name = profile.name.clone();
                    profile.name = value.clone();
                    self.cfg.save();
                    self.reload_manager();
                    log_info!("renamed profile {index} from '{old_name}' to '{value}'");
                    self.set_banner(BannerKind::Success, format!("Renamed profile to '{value}'."));
                }
            }
            EditingField::NewProfileName => {
                let name = if value.is_empty() {
                    format!("Account {}", self.cfg.profiles.len() + 1)
                } else {
                    value
                };
                self.create_profile_with_name(name);
            }
            EditingField::LiveOverride => {
                self.cfg.live_override = if value.is_empty() { None } else { Some(value) };
                self.cfg.save();
                self.reload_manager();
                self.set_banner(BannerKind::Success, "Updated live data directory override.");
            }
            EditingField::LaunchExe => {
                let mut launch = self.cfg.launch.clone().unwrap_or_default();
                launch.exe = if value.is_empty() { None } else { Some(value) };
                self.cfg.launch = if launch.exe.is_none() && launch.command.is_none() {
                    None
                } else {
                    Some(launch)
                };
                self.cfg.save();
                self.reload_manager();
                self.set_banner(BannerKind::Success, "Updated Claude executable path.");
            }
            EditingField::LaunchCommand => {
                let mut launch = self.cfg.launch.clone().unwrap_or_default();
                launch.command = if value.is_empty() { None } else { Some(value) };
                self.cfg.launch = if launch.exe.is_none() && launch.command.is_none() {
                    None
                } else {
                    Some(launch)
                };
                self.cfg.save();
                self.reload_manager();
                self.set_banner(BannerKind::Success, "Updated custom launch command.");
            }
        }
        cx.notify();
    }

    fn create_profile_with_name(&mut self, name: String) {
        let mut slug_num = self.cfg.profiles.len() + 1;
        let dir = loop {
            let candidate = format!(r"profiles\account_{slug_num}");
            let exists = self
                .cfg
                .profiles
                .iter()
                .any(|p| p.dir.eq_ignore_ascii_case(&candidate));
            if !exists {
                break candidate;
            }
            slug_num += 1;
        };
        self.cfg.profiles.push(ProfileConfig {
            name: name.clone(),
            dir: dir.clone(),
        });
        self.cfg.save();
        self.reload_manager();
        log_info!("added profile '{name}' ({dir})");
        self.set_banner(BannerKind::Success, format!("Added profile '{name}'."));
    }

    fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.cfg.profiles.len() <= 1 {
            self.set_banner(BannerKind::Warning, "At least one profile must remain.");
            cx.notify();
            return;
        }
        if index == self.manager.active() {
            self.set_banner(
                BannerKind::Warning,
                "Switch to another profile before removing the active one.",
            );
            cx.notify();
            return;
        }
        let removed = self.cfg.profiles.remove(index);
        if self.manager.active() > index {
            self.cfg.active = Some(self.manager.active() - 1);
        }
        self.confirm_delete = None;
        self.expanded = None;
        self.cfg.save();
        self.reload_manager();
        log_info!(
            "removed profile '{}' from config (data folder kept at {})",
            removed.name,
            removed.dir
        );
        self.set_banner(
            BannerKind::Info,
            format!(
                "Removed '{}' from profile list (folder '{}' preserved on disk).",
                removed.name, removed.dir
            ),
        );
        cx.notify();
    }

    fn browse_for_claude_exe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select claude.exe".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(chosen) = paths.pop() {
                    let _ = view.update_in(cx, |this, _window, cx| {
                        let mut launch: LaunchConfig = this.cfg.launch.clone().unwrap_or_default();
                        launch.exe = Some(chosen.to_string_lossy().to_string());
                        this.cfg.launch = Some(launch);
                        this.cfg.save();
                        this.reload_manager();
                        this.set_banner(
                            BannerKind::Success,
                            format!("Set Claude executable to {}", chosen.display()),
                        );
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_none() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        match key {
            "escape" => {
                self.cancel_edit(cx);
            }
            "enter" => {
                self.commit_edit(cx);
            }
            "backspace" => {
                self.edit_buffer.pop();
                cx.notify();
            }
            "v" if modifiers.control => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let clean = text.replace(['\r', '\n'], " ");
                    self.edit_buffer.push_str(&clean);
                    cx.notify();
                }
            }
            "c" if modifiers.control => {
                cx.write_to_clipboard(ClipboardItem::new_string(self.edit_buffer.clone()));
            }
            "space" => {
                self.edit_buffer.push(' ');
                cx.notify();
            }
            _ => {
                if !modifiers.control && !modifiers.alt && !modifiers.platform {
                    if let Some(ch) = &event.keystroke.key_char {
                        if !ch.chars().any(char::is_control) {
                            self.edit_buffer.push_str(ch);
                            cx.notify();
                            return;
                        }
                    }
                    if key.chars().count() == 1 {
                        let ch = if modifiers.shift {
                            key.to_uppercase()
                        } else {
                            key.to_string()
                        };
                        self.edit_buffer.push_str(&ch);
                        cx.notify();
                    }
                }
            }
        }
    }

    fn quit_app(&mut self, cx: &mut Context<Self>) {
        log_info!("exiting application at user request");
        self.allow_close.store(true, Ordering::SeqCst);
        cx.quit();
    }

    fn render_titlebar(&self, window: &Window) -> impl IntoElement {
        let max_glyph = if window.is_maximized() {
            Glyph::Restore
        } else {
            Glyph::Maximize
        };

        div()
            .id("titlebar")
            .flex_shrink_0()
            .h(px(38.0))
            .flex()
            .flex_row()
            .items_center()
            .child(
                // Only this part drags: GPUI reports the first control area under
                // the cursor, so a Drag area wrapping the caption buttons would
                // shadow them.
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .pl_4()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .window_control_area(WindowControlArea::Drag)
                    .child(dither::logo(self.sky.clock, 1.0).size(px(22.0)))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child("ClaudeSyncTray"),
                    )
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                    ),
            )
            .child(caption_button("caption-min", Glyph::Minimize, WindowControlArea::Min, false))
            .child(caption_button("caption-max", max_glyph, WindowControlArea::Max, false))
            .child(caption_button("caption-close", Glyph::Close, WindowControlArea::Close, true))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy_message.is_some();
        let running = self.claude_pids > 0;
        let active_idx = self.manager.active();
        let active_name = self.manager.active_profile().name.clone();
        let profile_count = self.manager.profiles().len();

        div()
            .w(px(204.0))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .px_3()
            .pt_4()
            .pb_3()
            .child(self.nav_item(
                "nav-accounts",
                "01",
                "Accounts",
                ActiveTab::Profiles,
                Some(format!("{profile_count:02}")),
                cx,
            ))
            .child(self.nav_item("nav-settings", "02", "Settings", ActiveTab::Settings, None, cx))
            .child(self.nav_item("nav-data", "03", "Data", ActiveTab::Data, None, cx))
            .child(self.nav_item("nav-activity", "04", "Activity", ActiveTab::Logs, None, cx))
            .child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(caption("CLAUDE DESKTOP"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(status_dot(if running { SUCCESS } else { TEXT_FAINT }))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(TEXT))
                                    .child(if running { "Running" } else { "Not running" }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(status_dot(account_hex(active_idx)))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(active_name),
                            ),
                    )
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .flex_row()
                            .gap_1p5()
                            .child(
                                button(
                                    "btn-restart-claude",
                                    if running { "Restart" } else { "Launch" },
                                    ButtonKind::Secondary,
                                    !busy,
                                )
                                .flex_1()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.trigger_restart_claude(window, cx);
                                })),
                            )
                            .when(running, |row| {
                                row.child(
                                    button("btn-stop-claude", "Stop", ButtonKind::Danger, !busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.trigger_stop_claude(window, cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .my_3()
                    .child(dither::dotted_rule(BORDER_STRONG).w_full().h(px(1.0))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(
                        button("btn-hide-tray", "Hide to tray", ButtonKind::Ghost, true).on_click(
                            cx.listener(|_, _, window, _| {
                                process::hide_window(window);
                            }),
                        ),
                    )
                    .child(
                        button("btn-quit-app", "Quit", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.quit_app(cx);
                            }),
                        ),
                    ),
            )
    }

    fn nav_item(
        &self,
        id: &'static str,
        number: &'static str,
        label: &'static str,
        tab: ActiveTab,
        trailing: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.tab == tab;
        div()
            .id(id)
            .h(px(34.0))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .rounded_sm()
            .text_sm()
            .when(selected, |s| {
                s.bg(rgb(SURFACE))
                    .text_color(rgb(TEXT))
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!selected, |s| {
                s.text_color(rgb(TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.tab = tab;
                this.confirm_delete = None;
                this.expanded = None;
                this.editing = None;
                this.edit_buffer.clear();
                if tab == ActiveTab::Logs {
                    this.log_lines = logging::recent_lines(120);
                }
                if tab == ActiveTab::Data {
                    this.open_data(window, cx);
                }
                cx.notify();
            }))
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(if selected { ACCENT } else { TEXT_FAINT }))
                    .child(number),
            )
            .child(div().flex_1().child(label))
            .when_some(trailing, |s, text| {
                s.child(
                    div()
                        .font_family(MONO)
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child(text),
                )
            })
    }

    /// The active account, drawn over its live dithered sky.
    fn render_hero(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.manager.active();
        let profile = self.manager.active_profile();
        let name = profile.name.clone();
        let dir = profile.dir.clone();
        let facts = self.facts(index);
        let is_renaming = self.editing == Some(EditingField::RenameProfile(index));
        let (link_label, link_tone) = match self.cached_link() {
            LinkHealth::Healthy => ("LINKED", SUCCESS),
            LinkHealth::NotLinked => ("NOT LINKED", WARNING),
            LinkHealth::Broken => ("LINK BROKEN", DANGER),
        };

        let title = if is_renaming {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .child(text_input(&self.edit_buffer, "Account name").w(px(240.0)))
                .child(
                    button("hero-save-rename", "Save", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))),
                )
                .child(
                    button("hero-cancel-rename", "Cancel", ButtonKind::Ghost, true)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                )
                .into_any_element()
        } else {
            // GPUI's ellipsis truncation collapses large text to "…", so clip and
            // shorten by hand instead.
            let shown: String = if name.chars().count() > 26 {
                name.chars().take(25).chain(std::iter::once('…')).collect()
            } else {
                name.to_string()
            };
            div()
                .w_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_3xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(shown)
                .into_any_element()
        };

        div()
            .id("hero")
            .flex_none()
            .relative()
            .h(px(220.0))
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(SUNKEN))
            .border_1()
            .border_color(rgb(BORDER))
            .child(
                dither::sky(self.sky_frame())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(
                div()
                    .relative()
                    .size_full()
                    .p_5()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(caption(format!("ACTIVE ACCOUNT · {:02}", index + 1)))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1p5()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(rgba((BG << 8) | 0xd9))
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .child(status_dot(link_tone))
                                    .child(
                                        div()
                                            .font_family(MONO)
                                            .text_xs()
                                            .text_color(rgb(link_tone))
                                            .child(link_label),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(title)
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .max_w(px(320.0))
                                            .truncate()
                                            .text_sm()
                                            .text_color(rgb(TEXT_MUTED))
                                            .child(facts.detail),
                                    )
                                    .when(facts.cli_auth, |row| row.child(tag("CLAUDE CODE", TEXT_MUTED))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1p5()
                            .child(
                                button("hero-rename", "Rename", ButtonKind::Secondary, !is_renaming)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.begin_edit(
                                            EditingField::RenameProfile(index),
                                            name.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                button("hero-side-by-side", "Open side by side", ButtonKind::Secondary, true)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.trigger_side_by_side(index, cx);
                                    })),
                            )
                            .child(
                                button("hero-open-folder", "Open folder", ButtonKind::Secondary, true)
                                    .on_click(cx.listener(move |_, _, _, _| {
                                        process::open_path(&dir);
                                    })),
                            ),
                    ),
            )
    }

    fn render_link_callout(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active_name = self.manager.active_profile().name.clone();
        let (tone, title, body, action) = match self.cached_link() {
            LinkHealth::Healthy => return None,
            LinkHealth::NotLinked => (
                WARNING,
                "Claude's data folder isn't linked to an account yet",
                format!(
                    "Linking moves its current contents into '{active_name}'. \
                     Claude Desktop will be closed if it's open."
                ),
                "Link now",
            ),
            LinkHealth::Broken => (
                DANGER,
                "The account link is broken",
                format!(
                    "Claude's data folder doesn't point at '{active_name}'. \
                     Repairing closes Claude Desktop if it's open."
                ),
                "Repair",
            ),
        };
        let busy = self.busy_message.is_some();

        Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .rounded_md()
                .overflow_hidden()
                .bg(rgba((tone << 8) | 0x0d))
                .border_1()
                .border_color(rgba((tone << 8) | 0x40))
                .child(dither::hatch(tone).w(px(10.0)).flex_shrink_0())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(TEXT))
                                        .child(title),
                                )
                                .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(body)),
                        )
                        .child(
                            button("btn-repair-link", action, ButtonKind::Secondary, !busy).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.trigger_repair_link(window, cx);
                                }),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_accounts_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_idx = self.manager.active();
        let profiles = self.manager.profiles().to_vec();
        let total = profiles.len();
        let busy = self.busy_message.is_some();
        let is_adding = self.editing == Some(EditingField::NewProfileName);

        let others: Vec<_> = profiles
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index != active_idx)
            .collect();
        let other_count = others.len();

        let rows = others.into_iter().enumerate().map(|(position, (index, profile))| {
            let is_expanded = self.expanded == Some(index);
            let is_renaming = self.editing == Some(EditingField::RenameProfile(index));
            let is_confirming = self.confirm_delete == Some(index);
            let can_remove = total > 1;
            let facts = self.facts(index);
            let facts_for_contents = facts.clone();
            let current_name = profile.name.clone();
            let dir_for_open = profile.dir.clone();

            let name_line = if is_renaming {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .child(text_input(&self.edit_buffer, "Account name").w(px(220.0)))
                    .child(
                        button(("save-rename", index), "Save", ButtonKind::Primary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))),
                    )
                    .child(
                        button(("cancel-rename", index), "Cancel", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                    )
            } else {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(TEXT))
                            .child(profile.name.clone()),
                    )
                    .when(facts.cli_auth, |row| row.child(tag("CLAUDE CODE", TEXT_FAINT)))
            };

            let summary = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .child(account_tile(index))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(name_line)
                        .child(
                            div()
                                .w_full()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(TEXT_MUTED))
                                .child(facts.detail),
                        ),
                )
                .child(
                    button(("switch", index), "Switch", ButtonKind::Secondary, !busy).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.trigger_switch(index, true, window, cx);
                        }),
                    ),
                )
                .child(
                    button(("more", index), "···", ButtonKind::Ghost, true)
                        .w(px(30.0))
                        .px_0()
                        .when(is_expanded, |b| b.bg(rgb(SURFACE_HOVER)).text_color(rgb(TEXT)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.expanded = if this.expanded == Some(index) {
                                None
                            } else {
                                Some(index)
                            };
                            this.confirm_delete = None;
                            cx.notify();
                        })),
                );

            let actions = if is_confirming {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(TEXT))
                            .child(format!(
                                "Remove '{current_name}' from the list? Its folder stays on disk."
                            )),
                    )
                    .child(
                        button(("confirm-del", index), "Remove", ButtonKind::Destructive, true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_profile(index, cx);
                            })),
                    )
                    .child(
                        button(("cancel-del", index), "Cancel", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.confirm_delete = None;
                                cx.notify();
                            }),
                        ),
                    )
            } else {
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(
                        button(
                            ("switch-only", index),
                            "Switch without relaunching",
                            ButtonKind::Ghost,
                            !busy,
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.trigger_switch(index, false, window, cx);
                        })),
                    )
                    .child(
                        button(("side-by-side", index), "Side by side", ButtonKind::Ghost, true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.trigger_side_by_side(index, cx);
                            })),
                    )
                    .child(
                        button(("rename", index), "Rename", ButtonKind::Ghost, true).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.begin_edit(
                                    EditingField::RenameProfile(index),
                                    current_name.clone(),
                                    window,
                                    cx,
                                );
                            }),
                        ),
                    )
                    .child(
                        button(("open-folder", index), "Open folder", ButtonKind::Ghost, true)
                            .on_click(cx.listener(move |_, _, _, _| {
                                process::open_path(&dir_for_open);
                            })),
                    )
                    .when(can_remove, |row| {
                        row.child(div().flex_1()).child(
                            button(("remove", index), "Remove", ButtonKind::Danger, true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.confirm_delete = Some(index);
                                    cx.notify();
                                }),
                            ),
                        )
                    })
            };

            div()
                .flex()
                .flex_col()
                .when(position > 0, |row| row.border_t_1().border_color(rgb(BORDER)))
                .child(summary)
                .when(is_expanded, |row| {
                    row.child(
                        div().flex().flex_row().pl(px(62.0)).pr_4().pb_3().child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .font_family(MONO)
                                        .text_xs()
                                        .text_color(rgb(TEXT_FAINT))
                                        .child(profile.dir.display().to_string()),
                                )
                                .child(contents_view(&facts_for_contents))
                                .child(actions),
                        ),
                    )
                })
        });
        let rows: Vec<_> = rows.collect();

        let new_row = is_adding.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .when(other_count > 0, |row| row.border_t_1().border_color(rgb(BORDER)))
                .child(account_tile(total))
                .child(text_input(&self.edit_buffer, "Account name").flex_1())
                .child(
                    button("btn-save-new-profile", "Add", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))),
                )
                .child(
                    button("btn-cancel-new-profile", "Cancel", ButtonKind::Ghost, true)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                )
        });

        let list = if rows.is_empty() && new_row.is_none() {
            panel()
                .px_4()
                .py_5()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child("No other accounts yet. Add one to keep a second Claude sign-in on hand.")
        } else {
            panel().children(rows).children(new_row)
        };

        page("accounts-scroll")
            .child(self.render_hero(cx))
            .children(self.render_link_callout(cx))
            .child(
                section("Data in this account")
                    .child(panel().px_4().py_3().child(contents_view(&self.facts(active_idx)))),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .h(px(28.0))
                            .px_1()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(caption("OTHER ACCOUNTS"))
                            .child(caption(format!("{other_count:02}")))
                            .child(div().flex_1())
                            .when(!is_adding, |header| {
                                header.child(
                                    button("btn-add-profile", "+ New account", ButtonKind::Secondary, true)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let default_name =
                                                format!("Account {}", this.cfg.profiles.len() + 1);
                                            this.begin_edit(
                                                EditingField::NewProfileName,
                                                default_name,
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                    .child(list),
            )
            .when(
                matches!(
                    self.editing,
                    Some(EditingField::NewProfileName | EditingField::RenameProfile(_))
                ),
                |page| page.child(edit_hint()),
            )
    }

    fn render_settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let autostart = self.autostart_enabled;
        let share_mcp = self.cfg.share_mcp_config;
        let sync_cli = self.cfg.sync_claude_code;
        let share_sessions = self.cfg.share_sessions;
        let launch = self.cfg.launch.clone().unwrap_or_default();
        let live_override = self.cfg.live_override.clone().unwrap_or_default();
        let busy = self.busy_message.is_some();

        let resolved_target = match self.manager.launch_target() {
            LaunchTarget::Exe(p) => p.display().to_string(),
            LaunchTarget::AppsFolder(aumid) => format!("Store app  {aumid}"),
            LaunchTarget::Shell(cmd) => format!("Command  {cmd}"),
        };
        let (health_label, health_tone) = match self.cached_link() {
            LinkHealth::Healthy => ("LINKED", SUCCESS),
            LinkHealth::NotLinked => ("NOT LINKED", WARNING),
            LinkHealth::Broken => ("BROKEN", DANGER),
        };

        page("settings-scroll")
            .child(page_header(
                "Settings",
                "Changes are saved to config.json immediately.",
            ))
            .child(
                section("General").child(
                    panel().child(
                        setting_row(
                            true,
                            "Start with Windows",
                            "Start ClaudeSyncTray hidden in the tray when you sign in.",
                        )
                        .child(toggle("btn-toggle-autostart", autostart).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.toggle_autostart(cx);
                            },
                        ))),
                    ),
                ),
            )
            .child(
                section("Shared between accounts").child(
                    panel()
                        .child(
                            setting_row(
                                true,
                                "MCP servers and desktop config",
                                "Copy the newest claude_desktop_config.json to every account on switch.",
                            )
                            .child(
                                button("btn-sync-mcp-now", "Sync now", ButtonKind::Secondary, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.sync_mcp_now(cx))),
                            )
                            .child(toggle("btn-toggle-share-mcp", share_mcp).on_click(
                                cx.listener(|this, _, _, cx| this.toggle_share_mcp_config(cx)),
                            )),
                        )
                        .child(
                            setting_row(
                                false,
                                "Claude Code CLI sign-in",
                                "Swap ~/.claude credentials along with the desktop account.",
                            )
                            .child(
                                button("btn-snapshot-cli-now", "Save current", ButtonKind::Secondary, true)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.snapshot_cli_auth_now(cx)),
                                    ),
                            )
                            .child(toggle("btn-toggle-sync-cli", sync_cli).on_click(cx.listener(
                                |this, _, _, cx| this.toggle_sync_claude_code(cx),
                            ))),
                        )
                        .child(
                            setting_row(
                                false,
                                "Code sessions",
                                "Show the same Code session list under every account. Chats on claude.ai stay per account.",
                            )
                            .child(
                                button("btn-sync-sessions-now", "Sync now", ButtonKind::Secondary, true)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.sync_sessions_now(cx)),
                                    ),
                            )
                            .child(toggle("btn-toggle-share-sessions", share_sessions).on_click(
                                cx.listener(|this, _, _, cx| this.toggle_share_sessions(cx)),
                            )),
                        ),
                ),
            )
            .child(
                section("Claude Desktop").child(
                    panel()
                        .child(
                            setting_row(true, "Launches with", resolved_target).child(
                                button("btn-browse-exe", "Browse…", ButtonKind::Secondary, true)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.browse_for_claude_exe(window, cx);
                                    })),
                            ),
                        )
                        .child(self.render_edit_row(
                            "edit-launch-exe",
                            "Executable path",
                            "Overrides auto-detection of claude.exe.",
                            launch.exe.unwrap_or_default(),
                            "Auto-detect",
                            EditingField::LaunchExe,
                            cx,
                        ))
                        .child(self.render_edit_row(
                            "edit-launch-cmd",
                            "Launch command",
                            "Runs instead of the executable, e.g. explorer.exe shell:AppsFolder\\<AUMID>.",
                            launch.command.unwrap_or_default(),
                            "None",
                            EditingField::LaunchCommand,
                            cx,
                        ))
                        .child(self.render_edit_row(
                            "edit-live-override",
                            "Data folder",
                            "The folder Claude Desktop reads; it becomes a link to the active account.",
                            live_override,
                            "Auto-detect",
                            EditingField::LiveOverride,
                            cx,
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_4()
                                .px_4()
                                .py_3()
                                .border_t_1()
                                .border_color(rgb(BORDER))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(TEXT))
                                                        .child("Account link"),
                                                )
                                                .child(tag(health_label, health_tone)),
                                        )
                                        .child(mono_line(self.manager.live.display().to_string()))
                                        .child(mono_line(format!(
                                            "→ {}",
                                            self.manager.active_dir().display()
                                        ))),
                                )
                                .child(
                                    button("btn-verify-link", "Repair", ButtonKind::Secondary, !busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.trigger_repair_link(window, cx);
                                        })),
                                ),
                        ),
                ),
            )
            .child(
                section("Files").child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            button("shortcut-code", "Claude Code projects", ButtonKind::Secondary, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_code_folder();
                                    cx.notify();
                                })),
                        )
                        .child(
                            button("shortcut-profiles", "Profiles folder", ButtonKind::Secondary, true)
                                .on_click(cx.listener(|_, _, _, _| {
                                    process::open_path(&paths::profiles_root());
                                })),
                        )
                        .child(
                            button("shortcut-config", "config.json", ButtonKind::Secondary, true)
                                .on_click(cx.listener(|_, _, _, _| {
                                    process::open_path(&paths::config_file());
                                })),
                        )
                        .child(
                            button("shortcut-log", "Log file", ButtonKind::Secondary, true).on_click(
                                cx.listener(|_, _, _, _| {
                                    process::open_path(&paths::log_file());
                                }),
                            ),
                        ),
                ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_edit_row(
        &self,
        id: &'static str,
        title: &'static str,
        description: &'static str,
        value: String,
        placeholder: &'static str,
        field: EditingField,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_editing = self.editing.as_ref() == Some(&field);
        let initial = value.clone();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().text_sm().text_color(rgb(TEXT)).child(title))
                            .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(description)),
                    )
                    .child(if is_editing {
                        div()
                            .flex()
                            .flex_row()
                            .gap_1p5()
                            .child(
                                button((id, 1usize), "Save", ButtonKind::Primary, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.commit_edit(cx))),
                            )
                            .child(
                                button((id, 2usize), "Cancel", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                            )
                            .into_any_element()
                    } else {
                        button((id, 0usize), "Edit", ButtonKind::Secondary, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_edit(field.clone(), initial.clone(), window, cx);
                            }))
                            .into_any_element()
                    }),
            )
            .child(if is_editing {
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(text_input(&self.edit_buffer, "Leave empty to auto-detect"))
                    .child(edit_hint())
                    .into_any_element()
            } else if value.is_empty() {
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(placeholder)
                    .into_any_element()
            } else {
                mono_line(value).into_any_element()
            })
    }

    fn render_activity_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.log_lines.clone();
        let is_empty = lines.is_empty();

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .px_6()
            .py_5()
            .overflow_hidden()
            .child(
                page_header("Activity", "Newest entries first.").child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1p5()
                        .child(
                            button("btn-refresh-logs", "Refresh", ButtonKind::Secondary, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.log_lines = logging::recent_lines(120);
                                    cx.notify();
                                })),
                        )
                        .child(
                            button("btn-open-log-external", "Open file", ButtonKind::Ghost, true)
                                .on_click(cx.listener(|_, _, _, _| {
                                    process::open_path(&paths::log_file());
                                })),
                        )
                        .child(
                            button("btn-clear-logs", "Clear", ButtonKind::Danger, true).on_click(
                                cx.listener(|this, _, _, cx| {
                                    logging::clear();
                                    this.log_lines = logging::recent_lines(120);
                                    cx.notify();
                                }),
                            ),
                        ),
                ),
            )
            .child(
                div()
                    .id("log-lines-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(SUNKEN))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .font_family(MONO)
                    .text_xs()
                    .when(is_empty, |log| {
                        log.child(
                            div()
                                .px_4()
                                .py_2()
                                .text_color(rgb(TEXT_FAINT))
                                .child("Nothing logged yet."),
                        )
                    })
                    // Newest first, so the latest switch is visible without scrolling.
                    .children(lines.into_iter().rev().map(|line| {
                        let (time, level, message) = split_log_line(&line);
                        let level_color = match level.as_str() {
                            "ERROR" => DANGER,
                            "WARN" => WARNING,
                            _ => TEXT_FAINT,
                        };
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .px_4()
                            .py_0p5()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(time),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .w(px(40.0))
                                    .text_color(rgb(level_color))
                                    .child(level),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(rgb(if level_color == TEXT_FAINT {
                                        TEXT_MUTED
                                    } else {
                                        level_color
                                    }))
                                    .child(message),
                            )
                    })),
            )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(busy) = &self.busy_message {
            return Some(
                status_bar(BG)
                    .child(
                        dither::scanner(ACCENT, self.sky.clock)
                            .flex_shrink_0()
                            .w(px(64.0))
                            .h(px(8.0)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(TEXT))
                            .child(busy.clone()),
                    )
                    .into_any_element(),
            );
        }

        let banner = self.banner.as_ref()?;
        let (tone, bg) = match banner.kind {
            BannerKind::Info => (TEXT_MUTED, BG),
            BannerKind::Success => (SUCCESS, BG),
            BannerKind::Warning => (WARNING, 0x1c170c),
            BannerKind::Error => (DANGER, 0x1f1010),
        };
        Some(
            status_bar(bg)
                .child(status_dot(tone))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(TEXT))
                        .child(banner.text.clone()),
                )
                .child(
                    button("btn-dismiss-banner", "Dismiss", ButtonKind::Ghost, true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.banner = None;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        )
    }
}

impl Render for DashboardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_snapshot();
        self.sync_sky();

        div()
            .id("dashboard-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.render_titlebar(window))
            .child(
                dither::dotted_rule(BORDER_STRONG)
                    .flex_shrink_0()
                    .w_full()
                    .h(px(1.0)),
            )
            .child(
                // Focus lives below the titlebar: a focusable ancestor marks every
                // mouse-down as handled, which stops Windows from starting a
                // window drag or pressing the caption buttons.
                div()
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::handle_key_down))
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(self.render_sidebar(cx))
                    .child(
                        dither::dotted_rule(BORDER_STRONG)
                            .flex_shrink_0()
                            .w(px(1.0))
                            .h_full(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(match self.tab {
                                ActiveTab::Profiles => {
                                    self.render_accounts_page(cx).into_any_element()
                                }
                                ActiveTab::Settings => {
                                    self.render_settings_page(cx).into_any_element()
                                }
                                ActiveTab::Data => self.render_data_page(cx).into_any_element(),
                                ActiveTab::Logs => self.render_activity_page(cx).into_any_element(),
                            })
                            .children(self.render_status_bar(cx)),
                    ),
            )
    }
}

// ---- Visual language ---------------------------------------------------------
//
// Ink-black surfaces and warm paper-white type; colour comes from the
// per-account tray palette, rendered as ordered dither. Small radii and mono
// captions keep it feeling like a 1-bit print.

const BG: u32 = 0x0d0d0d;
const SURFACE: u32 = 0x151515;
const SURFACE_HOVER: u32 = 0x1f1f1f;
const SUNKEN: u32 = 0x090909;
const BORDER: u32 = 0x242424;
const BORDER_STRONG: u32 = 0x363636;
const TEXT: u32 = 0xeeeae2;
const TEXT_MUTED: u32 = 0xaaa69d;
const TEXT_FAINT: u32 = 0x77746d;
const ACCENT: u32 = 0xd97757;
const SUCCESS: u32 = 0x6fdc8c;
const WARNING: u32 = 0xf2b84b;
const DANGER: u32 = 0xf06a5f;
const MONO: &str = "Cascadia Mono";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
    Destructive,
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
) -> Stateful<Div> {
    let transparent = rgba(0x00000000);
    let (bg, border, text, hover_bg, hover_text) = match kind {
        ButtonKind::Primary => (rgb(TEXT), rgb(TEXT), rgb(0x111111), rgb(0xffffff), rgb(0x111111)),
        ButtonKind::Secondary => (
            rgb(SURFACE_HOVER),
            rgb(BORDER_STRONG),
            rgb(TEXT),
            rgb(0x2a2a2a),
            rgb(TEXT),
        ),
        ButtonKind::Ghost => (transparent, transparent, rgb(TEXT_MUTED), rgb(SURFACE_HOVER), rgb(TEXT)),
        ButtonKind::Danger => (transparent, transparent, rgb(DANGER), rgba((DANGER << 8) | 0x22), rgb(DANGER)),
        ButtonKind::Destructive => (rgb(0xc23b32), rgb(0xc23b32), rgb(0xffffff), rgb(0xd4473d), rgb(0xffffff)),
    };
    let base = div()
        .id(id)
        .flex_shrink_0()
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text)
        .whitespace_nowrap()
        .child(label.into());
    if enabled {
        base.cursor_pointer()
            .hover(move |s| s.bg(hover_bg).text_color(hover_text))
    } else {
        base.opacity(0.4)
    }
}

/// Minimise / maximise / close, hit-tested by Windows itself so snap layouts work.
fn caption_button(
    id: &'static str,
    glyph: Glyph,
    area: WindowControlArea,
    is_close: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(46.0))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .hover(move |s| s.bg(rgb(if is_close { 0xc42b1c } else { SURFACE_HOVER })))
        .child(dither::glyph(glyph, TEXT_MUTED).size(px(10.0)))
}

fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .w(px(34.0))
        .h(px(18.0))
        .p(px(3.0))
        .flex()
        .items_center()
        .when(on, |s| s.justify_end())
        .rounded_sm()
        .bg(rgb(if on { ACCENT } else { BORDER_STRONG }))
        .cursor_pointer()
        .child(
            div()
                .size(px(12.0))
                .rounded(px(1.0))
                .bg(rgb(if on { 0xffffff } else { TEXT_MUTED })),
        )
}

/// Single-line text field drawn from `DashboardView::edit_buffer`; typing is
/// handled by the root key handler.
fn text_input(value: &str, placeholder: &'static str) -> Div {
    div()
        .h(px(30.0))
        .px_2p5()
        .flex()
        .flex_row()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .rounded_sm()
        .bg(rgb(SUNKEN))
        .border_1()
        .border_color(rgb(ACCENT))
        .text_sm()
        .text_color(rgb(TEXT))
        .when(!value.is_empty(), |s| s.child(value.to_string()))
        .child(div().flex_shrink_0().w(px(7.0)).h(px(15.0)).bg(rgba((ACCENT << 8) | 0xb0)))
        .when(value.is_empty(), |s| {
            s.child(div().pl_1().text_color(rgb(TEXT_FAINT)).child(placeholder))
        })
}

fn edit_hint() -> Div {
    div()
        .px_1()
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(TEXT_FAINT))
        .child("ENTER to save · ESC to cancel")
}

fn page(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .px_6()
        .py_5()
        .flex()
        .flex_col()
        .gap_5()
}

fn page_header(title: &'static str, subtitle: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_start()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT_MUTED))
                        .child(subtitle.into()),
                ),
        )
}

fn time_ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let seconds = now.saturating_sub(ms) / 1000;
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a profile folder holds: the signed-in Desktop account, the saved CLI login, MCP
/// servers, and the Code sessions stored per account folder.
fn contents_view(facts: &ProfileFacts) -> Div {
    let contents = &facts.contents;
    let row = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_row()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .w(px(96.0))
                    .flex_none()
                    .font_family(MONO)
                    .text_color(rgb(TEXT_FAINT))
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(TEXT_MUTED))
                    .child(value),
            )
    };

    let mut view = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(row("DESKTOP", facts.detail.clone()))
        .child(row(
            "CODE CLI",
            contents
                .cli_email
                .clone()
                .map_or("No saved login".into(), |email| format!("Saved login · {email}")),
        ))
        .child(row(
            "MCP SERVERS",
            if contents.mcp_servers == 0 {
                "None".into()
            } else {
                plural(contents.mcp_servers, "server", "servers")
            },
        ));

    if contents.accounts.is_empty() {
        view = view.child(row("CODE SESSIONS", "None stored".into()));
    }
    for (position, account) in contents.accounts.iter().enumerate() {
        let who = account
            .email
            .clone()
            .unwrap_or_else(|| format!("account {}", account.account_uuid.chars().take(8).collect::<String>()));
        let mut summary = format!("{who} · {}", plural(account.sessions, "session", "sessions"));
        if account.archived > 0 {
            summary.push_str(&format!(" ({} archived)", account.archived));
        }
        if let Some(ms) = account.last_activity_ms {
            summary.push_str(&format!(" · last active {}", time_ago(ms)));
        }
        if facts.desktop_account.as_deref() == Some(account.account_uuid.as_str()) {
            summary.push_str(" · signed in now");
        }
        view = view.child(row(if position == 0 { "CODE SESSIONS" } else { "" }, summary));
        for title in &account.recent {
            view = view.child(row("", format!("  · {title}")));
        }
    }
    view
}

/// Small mono label used for section headings and metadata.
fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(TEXT_FAINT))
        .child(text.into())
}

fn section(title: &'static str) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .child(caption(title.to_uppercase()).px_1())
}

fn panel() -> Div {
    // flex_none: otherwise the page column shrinks the panel (overflow_hidden
    // lets it go below content size) instead of letting the page scroll.
    div()
        .flex_none()
        .flex()
        .flex_col()
        .rounded_md()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .overflow_hidden()
}

/// A settings row; callers append the controls on the right.
fn setting_row(
    first: bool,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_4()
        .py_3()
        .when(!first, |row| row.border_t_1().border_color(rgb(BORDER)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().text_color(rgb(TEXT)).child(title.into()))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(description.into()),
                ),
        )
}

fn mono_line(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .truncate()
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(text.into())
}

/// Outlined mono tag, e.g. `CLAUDE CODE`.
fn tag(label: &'static str, tone: u32) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded_sm()
        .border_1()
        .border_color(rgba((tone << 8) | 0x59))
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(tone))
        .child(label)
}

/// A square "pixel" status marker.
fn status_dot(color: u32) -> Div {
    div().flex_shrink_0().size(px(6.0)).bg(rgb(color))
}

fn status_bar(bg: u32) -> Div {
    div()
        .flex_shrink_0()
        .h(px(38.0))
        .px_4()
        .flex()
        .flex_row()
        .items_center()
        .gap_2p5()
        .bg(rgb(bg))
        .border_t_1()
        .border_color(rgb(BORDER))
        .text_xs()
}

fn account_hex(index: usize) -> u32 {
    let [r, g, b] = icon::accent(index);
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// The account's dithered emblem, in the same colour as its tray icon.
fn account_tile(index: usize) -> Div {
    let hex = account_hex(index);
    div()
        .flex_shrink_0()
        .size(px(32.0))
        .p(px(4.0))
        .rounded_sm()
        .bg(rgba((hex << 8) | 0x1a))
        .child(dither::tile(index, hex).size(px(24.0)))
}

/// Splits `2026-10-18 22:19:07 [INFO] message` into `("10-18 22:19:07", "INFO", "message")`.
fn split_log_line(line: &str) -> (String, String, String) {
    let parsed = line.get(19..).and_then(|rest| {
        let rest = rest.strip_prefix(" [")?;
        let (level, message) = rest.split_once("] ")?;
        Some((line.get(5..19)?.to_string(), level.to_string(), message.to_string()))
    });
    parsed.unwrap_or_else(|| (String::new(), String::new(), line.to_string()))
}

pub fn run(
    cfg: Config,
    manager: ProfileManager,
    start_hidden: bool,
    _guard: process::InstanceGuard,
) {
    Application::new().run(move |cx: &mut App| {
        let (tray, tray_rx): (Option<TrayController>, Option<Receiver<TrayAction>>) =
            match TrayController::new(&manager) {
                Ok((tray, rx)) => (Some(tray), Some(rx)),
                Err(error) => {
                    log_error!("could not create the system tray icon: {error}");
                    (None, None)
                }
            };

        let bounds = Bounds::centered(None, size(px(880.0), px(600.0)), cx);
        let allow_close = Arc::new(AtomicBool::new(false));
        let allow_close_for_hook = allow_close.clone();

        let window_result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("ClaudeSyncTray")),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_min_size: Some(size(px(680.0), px(480.0))),
                show: !start_hidden,
                focus: !start_hidden,
                ..Default::default()
            },
            move |window, cx| {
                window.on_window_should_close(cx, move |window, _cx| {
                    if allow_close_for_hook.load(Ordering::SeqCst) {
                        true
                    } else {
                        process::hide_window(window);
                        false
                    }
                });

                cx.new(|cx| {
                    let view = DashboardView::new(
                        cfg,
                        manager,
                        tray,
                        allow_close,
                        window,
                        cx,
                    );

                    // One loop drives tray actions, periodic checks and animation frames.
                    cx.spawn_in(window, async move |weak_view, cx| {
                        let mut tick: u32 = 0;
                        loop {
                            cx.background_executor().timer(FRAME_INTERVAL).await;
                            tick = tick.wrapping_add(1);

                            let mut actions = Vec::new();
                            if let Some(rx) = &tray_rx {
                                while let Ok(action) = rx.try_recv() {
                                    actions.push(action);
                                }
                            }
                            let check_periodic = tick % 16 == 0;

                            let should_continue = weak_view
                                .update_in(cx, |this, window, cx| {
                                    for action in actions {
                                        this.handle_tray_action(action, window, cx);
                                    }
                                    if check_periodic {
                                        this.poll_external_command(window, cx);
                                        let new_pids = process::find_claude_desktop().len();
                                        if new_pids != this.claude_pids {
                                            this.claude_pids = new_pids;
                                            cx.notify();
                                        }
                                        if this.banner.as_ref().is_some_and(StatusBanner::expired) {
                                            this.banner = None;
                                            cx.notify();
                                        }
                                    }
                                    let visible = process::window_visible(window);
                                    if visible && !this.sky.was_visible {
                                        this.sky.dissolve = Some((None, Instant::now()));
                                    }
                                    this.sky.was_visible = visible;
                                    if this.wants_frames(window) {
                                        this.advance_clock();
                                        cx.notify();
                                    } else {
                                        this.sky.last_tick = Instant::now();
                                    }
                                })
                                .is_ok();

                            if !should_continue {
                                break;
                            }
                        }
                    })
                    .detach();

                    view
                })
            },
        );

        if let Err(error) = window_result {
            log_error!("could not open GPUI window: {error}");
            process::message_box(
                "ClaudeSyncTray",
                &format!("Could not create the GPUI window:\n{error}"),
            );
            std::process::exit(1);
        }
    });
}
