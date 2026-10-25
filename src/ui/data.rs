//! The Data tab: what each account folder holds, and moving data between accounts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{prelude::*, relative};

use super::*;
use crate::transfer::{
    self, Action, Entry, Kind, Mode, Plan, PlanItem, Report, ShareOutcome, ShareResult,
};

/// Rows shown before "Show all"; the long tail is mostly browser cache.
const INVENTORY_ROWS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DataSource {
    label: String,
    path: PathBuf,
    /// Index of the profile this is, or `None` for a backup folder.
    profile: Option<usize>,
}

struct LastCopy {
    from: String,
    to: String,
    report: Report,
}

pub(super) struct DataState {
    /// The account whose folders are listed and which receives a copy.
    viewing: usize,
    source: Option<PathBuf>,
    customize: bool,
    show_all: bool,
    mode: Mode,
    /// Folders ticked or unticked against the mode's default.
    overrides: HashMap<String, bool>,
    scans: HashMap<PathBuf, Vec<Entry>>,
    scanning: HashSet<PathBuf>,
    backups: Vec<DataSource>,
    last_copy: Option<LastCopy>,
    last_share: Vec<ShareResult>,
}

impl Default for DataState {
    fn default() -> Self {
        Self {
            viewing: 0,
            source: None,
            customize: false,
            show_all: false,
            mode: Mode::Minimal,
            overrides: HashMap::new(),
            scans: HashMap::new(),
            scanning: HashSet::new(),
            backups: Vec::new(),
            last_copy: None,
            last_share: Vec::new(),
        }
    }
}

fn list_backups() -> Vec<DataSource> {
    let Ok(entries) = std::fs::read_dir(paths::backups_root()) else {
        return Vec::new();
    };
    let mut found: Vec<DataSource> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().into_string().ok()?;
            (path.is_dir() && transfer::looks_like_claude_dir(&path)).then(|| DataSource {
                label: format!("Backup · {name}"),
                path,
                profile: None,
            })
        })
        .collect();
    found.sort_by(|a, b| b.label.cmp(&a.label));
    found
}

fn kind_tone(kind: Kind) -> u32 {
    match kind {
        Kind::Login => WARNING,
        Kind::Settings => SUCCESS,
        Kind::History => ACCENT,
        Kind::Runtime | Kind::Cache | Kind::Other => TEXT_MUTED,
    }
}

fn action_tone(action: Action) -> u32 {
    match action {
        Action::Copy => SUCCESS,
        Action::Merge => ACCENT,
        Action::Keep | Action::Skip => TEXT_FAINT,
    }
}

fn why(item: &PlanItem) -> String {
    match item.action {
        Action::Copy => format!("new here · {}", plural(item.files as usize, "file", "files")),
        Action::Merge if item.kind == Kind::History => "adds what is missing, newest wins".into(),
        Action::Merge => "adds what is missing".into(),
        Action::Keep if item.target_is_link => "shared folder, left alone".into(),
        Action::Keep => "already here, left as is".into(),
        Action::Skip if item.is_link => "a link, never copied".into(),
        Action::Skip => "not selected".into(),
    }
}

fn check_box(id: impl Into<ElementId>, on: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .size(px(14.0))
        .rounded(px(2.0))
        .border_1()
        .border_color(rgb(if on { ACCENT } else { BORDER_STRONG }))
        .bg(rgb(if on { ACCENT } else { SUNKEN }))
        .when(enabled, |s| s.cursor_pointer())
        .when(!enabled, |s| s.opacity(0.4))
}

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(26.0))
        .px_2p5()
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if selected { ACCENT } else { BORDER_STRONG }))
        .bg(rgb(if selected { SURFACE_HOVER } else { SURFACE }))
        .text_xs()
        .text_color(rgb(if selected { TEXT } else { TEXT_MUTED }))
        .cursor_pointer()
        .child(label.into())
}

fn size_bar(bytes: u64, max: u64, tone: u32) -> Div {
    let fraction = if max == 0 || bytes == 0 {
        0.0
    } else {
        (bytes as f32 / max as f32).clamp(0.02, 1.0)
    };
    div()
        .flex_1()
        .min_w_0()
        .h(px(4.0))
        .rounded_sm()
        .bg(rgb(BORDER))
        .child(div().h_full().rounded_sm().w(relative(fraction)).bg(rgb(tone)))
}

fn name_cell(name: &str, is_dir: bool) -> Div {
    let mut cell = div()
        .w(px(210.0))
        .flex_none()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .truncate()
                .font_family(MONO)
                .text_xs()
                .text_color(rgb(TEXT))
                .child(if is_dir { format!("{name}\\") } else { name.to_string() }),
        );
    if let Some(note) = transfer::describe(name) {
        cell = cell.child(div().truncate().text_xs().text_color(rgb(TEXT_FAINT)).child(note));
    }
    cell
}

fn size_cell(text: String) -> Div {
    div()
        .w(px(72.0))
        .flex_none()
        .flex()
        .justify_end()
        .font_family(MONO)
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(text)
}

fn row_frame(first: bool) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_4()
        .py_1p5()
        .when(!first, |row| row.border_t_1().border_color(rgb(BORDER)))
}

fn share_text(outcome: &ShareOutcome) -> String {
    match outcome {
        ShareOutcome::Moved => "moved into the shared folder".into(),
        ShareOutcome::Linked => "linked to the shared folder".into(),
        ShareOutcome::AlreadyShared => "already shared".into(),
        ShareOutcome::KeptOwnCopy => "kept its own copy, nothing deleted".into(),
        ShareOutcome::Restored => "link removed".into(),
        ShareOutcome::Failed(error) => format!("failed: {error}"),
    }
}

impl DashboardView {
    pub(super) fn open_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data.viewing = self.manager.active();
        self.data.source = None;
        self.data.backups = list_backups();
        self.refresh_data(window, cx);
    }

    fn refresh_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data.scans.clear();
        self.ensure_scans(window, cx);
    }

    /// Measures every folder the page needs that has not been measured yet, off the UI thread.
    fn ensure_scans(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut wanted: Vec<PathBuf> = self
            .manager
            .profiles()
            .iter()
            .map(|profile| profile.dir.clone())
            .collect();
        wanted.push(paths::shared_root());
        let sources = self.data_sources();
        if let Some(source) = self.resolve_source(&sources) {
            wanted.push(source.path.clone());
        }
        for dir in wanted {
            if self.data.scans.contains_key(&dir) || !self.data.scanning.insert(dir.clone()) {
                continue;
            }
            cx.spawn_in(window, async move |view, cx| {
                let measured = dir.clone();
                let entries = cx
                    .background_spawn(async move { transfer::scan(&measured) })
                    .await;
                let _ = view.update_in(cx, |this, _window, cx| {
                    this.data.scanning.remove(&dir);
                    this.data.scans.insert(dir, entries);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn data_sources(&self) -> Vec<DataSource> {
        let viewing = self.data.viewing;
        let mut sources: Vec<DataSource> = self
            .manager
            .profiles()
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != viewing)
            .map(|(index, profile)| DataSource {
                label: profile.name.clone(),
                path: profile.dir.clone(),
                profile: Some(index),
            })
            .collect();
        sources.extend(self.data.backups.iter().cloned());
        sources
    }

    fn resolve_source<'a>(&self, sources: &'a [DataSource]) -> Option<&'a DataSource> {
        self.data
            .source
            .as_ref()
            .and_then(|path| sources.iter().find(|source| &source.path == path))
            .or_else(|| sources.first())
    }

    fn data_plan(&self, source: &Path, target: &Path, quick: bool) -> Option<Plan> {
        let from = self.data.scans.get(source)?;
        let to = self.data.scans.get(target)?;
        Some(if quick {
            transfer::plan(from, to, Mode::Minimal, &HashMap::new())
        } else {
            transfer::plan(from, to, self.data.mode, &self.data.overrides)
        })
    }

    fn trigger_copy(&mut self, quick: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_message.is_some() {
            return;
        }
        let sources = self.data_sources();
        let Some(source) = self.resolve_source(&sources).cloned() else {
            self.set_banner(BannerKind::Warning, "There is no other account or backup to copy from.");
            cx.notify();
            return;
        };
        let viewing = self.data.viewing.min(self.manager.profiles().len().saturating_sub(1));
        let target = self.manager.profiles()[viewing].clone();
        let active = self.manager.active();
        if (viewing == active || source.profile == Some(active))
            && !process::find_claude_desktop().is_empty()
        {
            self.set_banner(
                BannerKind::Warning,
                "Close Claude Desktop first: the active account's files are in use.",
            );
            cx.notify();
            return;
        }
        let Some(plan) = self.data_plan(&source.path, &target.dir, quick) else {
            self.set_banner(BannerKind::Info, "Still measuring the folders; try again in a moment.");
            cx.notify();
            return;
        };
        if !plan.writes_anything() {
            self.set_banner(BannerKind::Info, "Nothing to copy: everything selected is already here.");
            cx.notify();
            return;
        }

        self.busy_message = Some(format!("Copying '{}' into '{}'…", source.label, target.name));
        cx.notify();

        let from_path = source.path.clone();
        let to_path = target.dir.clone();
        let (from_label, to_label) = (source.label.clone(), target.name.clone());
        cx.spawn_in(window, async move |view, cx| {
            let report = cx
                .background_spawn(async move {
                    transfer::execute(&plan, &from_path, &to_path, &|_| {})
                })
                .await;

            let _ = view.update_in(cx, |this, window, cx| {
                this.busy_message = None;
                let failed = report.failed();
                let message = if failed > 0 {
                    format!(
                        "Copied {} but {failed} file(s) could not be written; details below.",
                        transfer::format_bytes(report.bytes())
                    )
                } else {
                    format!(
                        "Copied {}: {} added, {} updated.",
                        transfer::format_bytes(report.bytes()),
                        report.added(),
                        report.updated()
                    )
                };
                this.set_banner(
                    if failed > 0 { BannerKind::Warning } else { BannerKind::Success },
                    message,
                );
                this.data.last_copy = Some(LastCopy { from: from_label, to: to_label, report });
                this.refresh_data(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_share(&mut self, share: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_message.is_some() {
            return;
        }
        if !process::find_claude_desktop().is_empty() {
            self.set_banner(BannerKind::Warning, "Close Claude Desktop first, then change sandbox sharing.");
            cx.notify();
            return;
        }
        self.busy_message = Some(if share {
            "Moving the Linux sandbox into the shared folder…".into()
        } else {
            "Giving the Linux sandbox back to an account…".into()
        });
        cx.notify();

        let profiles: Vec<(String, PathBuf)> = self
            .manager
            .profiles()
            .iter()
            .map(|profile| (profile.name.clone(), profile.dir.clone()))
            .collect();
        cx.spawn_in(window, async move |view, cx| {
            let results = cx
                .background_spawn(async move {
                    let shared = paths::shared_root();
                    if share {
                        transfer::share_runtime(&profiles, &shared)
                    } else {
                        transfer::unshare_runtime(&profiles, &shared)
                    }
                })
                .await;

            let _ = view.update_in(cx, |this, window, cx| {
                this.busy_message = None;
                let failed = results
                    .iter()
                    .filter(|result| matches!(result.outcome, ShareOutcome::Failed(_)))
                    .count();
                if failed > 0 {
                    this.set_banner(
                        BannerKind::Warning,
                        format!("{failed} folder(s) could not be changed; details below."),
                    );
                } else {
                    this.set_banner(
                        BannerKind::Success,
                        if share { "The Linux sandbox is now shared." } else { "Sandbox sharing is off." },
                    );
                }
                this.data.last_share = results;
                this.refresh_data(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_data_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.manager.profiles().len();
        let viewing = self.data.viewing.min(count.saturating_sub(1));
        let profile = &self.manager.profiles()[viewing];
        let (name, dir) = (profile.name.clone(), profile.dir.clone());

        let chips = (0..count).map(|index| {
            let label = self.manager.profiles()[index].name.clone();
            chip(("data-account", index), label, index == viewing).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.data.viewing = index;
                    this.data.source = None;
                    this.data.overrides.clear();
                    this.ensure_scans(window, cx);
                    cx.notify();
                },
            ))
        });

        page("data-scroll")
            .child(page_header(
                "Storage",
                "Where each account's disk space goes, and how to copy data between accounts.",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .child(caption("ACCOUNT").pr_2())
                    .children(chips),
            )
            .child(self.inventory_section(&name, &dir, cx))
            .child(self.copy_section(&name, &dir, cx))
            .child(self.sandbox_section(cx))
            .when_some(self.data.last_copy.as_ref(), |page, last| {
                page.child(last_copy_section(last))
            })
            .when(!self.data.last_share.is_empty(), |page| {
                page.child(last_share_section(&self.data.last_share))
            })
    }

    fn inventory_section(&self, name: &str, dir: &Path, cx: &mut Context<Self>) -> Div {
        let mut body = panel();
        let Some(entries) = self.data.scans.get(dir) else {
            return section("Inside the folder").child(
                body.child(
                    div()
                        .px_4()
                        .py_3()
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child("Measuring…"),
                ),
            );
        };
        let total: u64 = entries.iter().map(|entry| entry.bytes).sum();
        let max = entries.iter().map(|entry| entry.bytes).max().unwrap_or(0);
        let shown = if self.data.show_all { entries.len() } else { INVENTORY_ROWS.min(entries.len()) };

        body = body.child(
            row_frame(true)
                .bg(rgb(SUNKEN))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO)
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(dir.display().to_string()),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .child(format!(
                            "{} · {}",
                            plural(entries.len(), "entry", "entries"),
                            transfer::format_bytes(total)
                        )),
                ),
        );
        if entries.is_empty() {
            body = body.child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(format!("'{name}' has no data yet.")),
            );
        }
        for entry in entries.iter().take(shown) {
            let kind = transfer::classify(&entry.name);
            let tone = kind_tone(kind);
            let status = if entry.is_link {
                div()
                    .w(px(72.0))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .child(tag("LINK", SUCCESS))
            } else {
                size_cell(transfer::format_bytes(entry.bytes))
            };
            body = body.child(
                row_frame(false)
                    .child(name_cell(&entry.name, entry.is_dir))
                    .child(div().w(px(76.0)).flex_none().child(tag(kind.label(), tone)))
                    .child(size_bar(entry.bytes, max, tone))
                    .child(status),
            );
        }
        let hidden = entries.len() - shown;
        if hidden > 0 || self.data.show_all && entries.len() > INVENTORY_ROWS {
            let rest: u64 = entries.iter().skip(INVENTORY_ROWS).map(|entry| entry.bytes).sum();
            body = body.child(
                row_frame(false).child(
                    button(
                        "data-show-all",
                        if self.data.show_all {
                            "Show fewer".to_string()
                        } else {
                            format!("Show all · {hidden} more, {}", transfer::format_bytes(rest))
                        },
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.data.show_all = !this.data.show_all;
                        cx.notify();
                    })),
                ),
            );
        }
        section("Inside the folder").child(body)
    }

    fn copy_section(&self, name: &str, dir: &Path, cx: &mut Context<Self>) -> Div {
        let busy = self.busy_message.is_some();
        let sources = self.data_sources();
        let Some(source) = self.resolve_source(&sources).cloned() else {
            return section("Copy data in").child(
                panel().child(
                    div()
                        .px_4()
                        .py_3()
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child("There is no other account or backup to copy from."),
                ),
            );
        };

        let chips = sources.iter().enumerate().map(|(index, candidate)| {
            let path = candidate.path.clone();
            chip(
                ("data-source", index),
                candidate.label.clone(),
                candidate.path == source.path,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.data.source = Some(path.clone());
                this.data.overrides.clear();
                this.ensure_scans(window, cx);
                cx.notify();
            }))
        });

        let quick = self.data_plan(&source.path, dir, true);
        let quick_text = match &quick {
            Some(plan) => {
                let summary = plan.summary();
                if plan.writes_anything() {
                    format!(
                        "Settings and Code history only: {} across {}. Leaves out the Linux sandbox, caches and sign-in data.",
                        transfer::format_bytes(summary.bytes),
                        plural(summary.copy_items + summary.merge_items, "folder", "folders"),
                    )
                } else {
                    "Nothing to bring over: settings and history are already here.".to_string()
                }
            }
            None => "Measuring…".to_string(),
        };

        let mut body = panel()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .px_4()
                    .py_3()
                    .child(caption("FROM").pr_2())
                    .children(chips),
            )
            .child(
                setting_row(false, format!("Minimal copy into {name}"), quick_text).child(
                    button(
                        "data-copy-minimal",
                        "Copy minimal",
                        ButtonKind::Primary,
                        !busy && quick.as_ref().is_some_and(Plan::writes_anything),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.trigger_copy(true, window, cx);
                    })),
                ),
            )
            .child(
                setting_row(
                    false,
                    "Choose exactly what to copy",
                    "See every folder, what is new, what would be merged and what stays put.",
                )
                .child(
                    button(
                        "data-customize",
                        if self.data.customize { "Hide" } else { "Customize" },
                        ButtonKind::Secondary,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.data.customize = !this.data.customize;
                        cx.notify();
                    })),
                ),
            );

        if self.data.customize {
            body = body.child(self.customize_view(&source, dir, cx));
        }
        section("Copy data in").child(body)
    }

    fn customize_view(&self, source: &DataSource, dir: &Path, cx: &mut Context<Self>) -> Div {
        let busy = self.busy_message.is_some();
        let Some(plan) = self.data_plan(&source.path, dir, false) else {
            return div()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(rgb(BORDER))
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child("Measuring…");
        };
        let summary = plan.summary();

        let mut view = div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                row_frame(true)
                    .bg(rgb(SUNKEN))
                    .child(
                        chip(("data-mode", 0usize), "Minimal", self.data.mode == Mode::Minimal).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.data.mode = Mode::Minimal;
                                this.data.overrides.clear();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        chip(("data-mode", 1usize), "Everything", self.data.mode == Mode::Full).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.data.mode = Mode::Full;
                                this.data.overrides.clear();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child(format!(
                                "COPY {} · MERGE {} · KEEP {} · SKIP {}  ·  {} to read, {} left behind",
                                summary.copy_items,
                                summary.merge_items,
                                summary.keep_items,
                                summary.skip_items,
                                transfer::format_bytes(summary.bytes),
                                transfer::format_bytes(summary.skipped_bytes),
                            )),
                    ),
            );

        let max = plan.items.iter().map(|item| item.bytes).max().unwrap_or(0);
        for (index, item) in plan.items.iter().enumerate() {
            let tone = kind_tone(item.kind);
            let selected = item.selected && !item.is_link;
            let name = item.name.clone();
            let checkbox = check_box(("data-check", index), selected, !item.is_link)
                .when(!item.is_link, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.data.overrides.insert(name.clone(), !selected);
                        cx.notify();
                    }))
                });
            view = view.child(
                row_frame(false)
                    .child(checkbox)
                    .child(name_cell(&item.name, item.is_dir))
                    .child(div().w(px(76.0)).flex_none().child(tag(item.kind.label(), tone)))
                    .child(size_bar(item.bytes, max, tone))
                    .child(size_cell(transfer::format_bytes(item.bytes)))
                    .child(
                        div()
                            .w(px(190.0))
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(52.0)).flex_none().child(tag(
                                item.action.label(),
                                action_tone(item.action),
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(why(item)),
                            ),
                    ),
            );
        }

        view.child(
            row_frame(false)
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child("Nothing already in the account is deleted or replaced, except an older session by a newer one."),
                )
                .child(
                    button(
                        "data-copy-selected",
                        "Copy selected",
                        ButtonKind::Primary,
                        !busy && plan.writes_anything(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.trigger_copy(false, window, cx);
                    })),
                ),
        )
    }

    fn sandbox_section(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy_message.is_some();
        let shared_root = paths::shared_root();
        let (mut linked, mut own_copies, mut own_bytes) = (0usize, 0usize, 0u64);
        let mut measured = true;
        for profile in self.manager.profiles() {
            let Some(entries) = self.data.scans.get(&profile.dir) else {
                measured = false;
                continue;
            };
            for entry in entries
                .iter()
                .filter(|entry| transfer::SHARED_RUNTIME.contains(&entry.name.as_str()))
            {
                if entry.is_link {
                    linked += 1;
                } else {
                    own_copies += 1;
                    own_bytes += entry.bytes;
                }
            }
        }
        let shared_bytes: u64 = self
            .data
            .scans
            .get(&shared_root)
            .map(|entries| entries.iter().map(|entry| entry.bytes).sum())
            .unwrap_or(0);

        let description = if !measured {
            "Measuring…".to_string()
        } else if linked > 0 {
            format!(
                "{} use the shared copy ({}). {} keep their own ({}).",
                plural(linked, "folder", "folders"),
                transfer::format_bytes(shared_bytes),
                plural(own_copies, "folder", "folders"),
                transfer::format_bytes(own_bytes),
            )
        } else if own_copies > 0 {
            format!(
                "Every account keeps its own copy: {} on disk. Sharing stores one and links the rest.",
                transfer::format_bytes(own_bytes)
            )
        } else {
            "No account has a sandbox yet.".to_string()
        };

        section("Linux sandbox").child(
            panel()
                .child(
                    setting_row(true, "Share one sandbox between accounts", description)
                        .child(
                            button("data-share", "Share", ButtonKind::Secondary, !busy && measured)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.trigger_share(true, window, cx);
                                })),
                        )
                        .when(linked > 0, |row| {
                            row.child(
                                button("data-unshare", "Undo", ButtonKind::Ghost, !busy).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.trigger_share(false, window, cx);
                                    }),
                                ),
                            )
                        }),
                )
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child("Needs Claude closed. Don't run two accounts side by side while it is shared."),
                ),
        )
    }
}

fn last_copy_section(last: &LastCopy) -> Div {
    let report = &last.report;
    let mut body = panel().child(
        row_frame(true)
            .bg(rgb(SUNKEN))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(format!("{} → {}", last.from, last.to)),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .child(format!(
                        "+{} added · ~{} updated · {}",
                        report.added(),
                        report.updated(),
                        transfer::format_bytes(report.bytes())
                    )),
            ),
    );
    for outcome in &report.outcomes {
        let summary = if outcome.failed > 0 {
            format!(
                "{} failed{}",
                outcome.failed,
                outcome.error.as_ref().map_or(String::new(), |error| format!(" · {error}"))
            )
        } else {
            format!(
                "+{} added · ~{} updated · {} already there",
                outcome.added, outcome.updated, outcome.kept
            )
        };
        body = body.child(
            row_frame(false)
                .child(name_cell(&outcome.name, true))
                .child(div().w(px(76.0)).flex_none().child(tag(outcome.kind.label(), kind_tone(outcome.kind))))
                .child(div().w(px(52.0)).flex_none().child(tag(
                    outcome.action.label(),
                    action_tone(outcome.action),
                )))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(if outcome.failed > 0 { DANGER } else { TEXT_MUTED }))
                        .child(summary),
                )
                .child(size_cell(transfer::format_bytes(outcome.bytes))),
        );
    }
    section("Last copy").child(body)
}

fn last_share_section(results: &[ShareResult]) -> Div {
    let mut body = panel();
    for (index, result) in results.iter().enumerate() {
        let failed = matches!(result.outcome, ShareOutcome::Failed(_));
        body = body.child(
            row_frame(index == 0)
                .child(name_cell(&result.name, true))
                .child(
                    div()
                        .w(px(110.0))
                        .flex_none()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(result.profile.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(if failed { DANGER } else { TEXT_MUTED }))
                        .child(share_text(&result.outcome)),
                ),
        );
    }
    section("Last sandbox change").child(body)
}
