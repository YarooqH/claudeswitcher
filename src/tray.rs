//! System tray icon, context menu, and headless CLI switch helper.

use std::sync::mpsc::{self, Receiver};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;
use crate::profiles::ProfileManager;
use crate::{icon, startup};

#[derive(Debug, Clone)]
pub enum TrayAction {
    OpenDashboard,
    ToggleDashboard,
    SwitchAccount(usize),
    RestartClaude,
    RepairLink,
    ToggleAutostart,
    OpenCodeFolder,
    OpenProfilesFolder,
    OpenLogFile,
    Quit,
}

pub struct TrayController {
    tray: TrayIcon,
}

impl TrayController {
    pub fn new(manager: &ProfileManager) -> Result<(Self, Receiver<TrayAction>), String> {
        let (tx, rx) = mpsc::channel();

        let menu_tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = parse_menu_id(event.id.as_ref()) {
                let _ = menu_tx.send(action);
            }
        }));

        let tray_tx = tx;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => {
                let _ = tray_tx.send(TrayAction::ToggleDashboard);
            }
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                let _ = tray_tx.send(TrayAction::OpenDashboard);
            }
            _ => {}
        }));

        let menu = build_menu(manager);
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip(manager))
            .with_icon(icon::for_profile(manager.active()))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;

        Ok((Self { tray }, rx))
    }

    /// Rebuilds the tray menu, tooltip, and icon to reflect the latest profile state.
    pub fn sync(&self, manager: &ProfileManager) {
        let menu = build_menu(manager);
        self.tray.set_menu(Some(Box::new(menu)));
        if let Err(error) = self.tray.set_tooltip(Some(tooltip(manager))) {
            log_warn!("could not update tooltip: {error}");
        }
        if let Err(error) = self.tray.set_icon(Some(icon::for_profile(manager.active()))) {
            log_warn!("could not update tray icon: {error}");
        }
    }
}

fn parse_menu_id(id: &str) -> Option<TrayAction> {
    match id {
        "open_dashboard" => Some(TrayAction::OpenDashboard),
        "quit" => Some(TrayAction::Quit),
        "autostart" => Some(TrayAction::ToggleAutostart),
        "restart" => Some(TrayAction::RestartClaude),
        "repair" => Some(TrayAction::RepairLink),
        "open_code" => Some(TrayAction::OpenCodeFolder),
        "open_profiles" => Some(TrayAction::OpenProfilesFolder),
        "open_log" => Some(TrayAction::OpenLogFile),
        "status" => None,
        other => other
            .strip_prefix("account:")
            .and_then(|value| value.parse::<usize>().ok())
            .map(TrayAction::SwitchAccount),
    }
}

fn build_menu(manager: &ProfileManager) -> Menu {
    let menu = Menu::new();
    let append = |item: &dyn tray_icon::menu::IsMenuItem| {
        if let Err(error) = menu.append(item) {
            log_warn!("could not append menu item: {error}");
        }
    };

    let open_dashboard = MenuItem::with_id("open_dashboard", "Open Dashboard", true, None);
    let status_item = MenuItem::with_id(
        "status",
        format!("Active: {}", manager.active_profile().name),
        false,
        None,
    );
    let separator_top = PredefinedMenuItem::separator();

    let switch_menu = Submenu::with_id("switch", "Switch Account", true);
    for (index, profile) in manager.profiles().iter().enumerate() {
        let item = CheckMenuItem::with_id(
            format!("account:{index}"),
            &profile.name,
            true,
            index == manager.active(),
            None,
        );
        if let Err(error) = switch_menu.append(&item) {
            log_warn!("could not append account item: {error}");
        }
    }

    let separator_two = PredefinedMenuItem::separator();
    let restart_item = MenuItem::with_id("restart", "Restart Claude Desktop", true, None);
    let repair_item = MenuItem::with_id("repair", "Repair Profile Link", true, None);
    let separator_three = PredefinedMenuItem::separator();
    let autostart_item = CheckMenuItem::with_id(
        "autostart",
        "Start with Windows",
        true,
        startup::is_enabled(),
        None,
    );
    let separator_four = PredefinedMenuItem::separator();
    let open_code = MenuItem::with_id("open_code", "Open Claude Code Projects", true, None);
    let open_profiles = MenuItem::with_id("open_profiles", "Open Profiles Folder", true, None);
    let open_log = MenuItem::with_id("open_log", "Open Log File", true, None);
    let separator_five = PredefinedMenuItem::separator();
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);

    append(&open_dashboard);
    append(&status_item);
    append(&separator_top);
    append(&switch_menu);
    append(&separator_two);
    append(&restart_item);
    append(&repair_item);
    append(&separator_three);
    append(&autostart_item);
    append(&separator_four);
    append(&open_code);
    append(&open_profiles);
    append(&open_log);
    append(&separator_five);
    append(&quit_item);

    menu
}

fn tooltip(manager: &ProfileManager) -> String {
    format!("ClaudeSyncTray — Active: {}", manager.active_profile().name)
}

/// Used by `main` for the one-shot `--switch N` path (no tray needed).
pub fn headless_switch(index: usize) -> i32 {
    let cfg = Config::load_or_default();
    let mut manager = match ProfileManager::new(&cfg) {
        Ok(manager) => manager,
        Err(error) => {
            log_error!("could not initialise profiles: {error}");
            return 1;
        }
    };
    match manager.switch_to(index, true) {
        Ok(outcome) => {
            log_info!(
                "headless switch to '{}' complete (launched={})",
                outcome.profile,
                outcome.launched
            );
            if let Some(error) = outcome.launch_error {
                log_error!("Claude relaunch failed: {error}");
                return 1;
            }
            0
        }
        Err(error) => {
            log_error!("headless switch failed: {error}");
            1
        }
    }
}
