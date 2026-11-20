mod controls;
mod help;
#[cfg(target_os = "macos")]
mod home;
mod icons;
mod instance;
#[cfg(target_os = "macos")]
mod keep;
#[cfg(target_os = "macos")]
mod monitor;
mod old_files;
#[cfg(target_os = "macos")]
mod projects;
mod scan;
#[cfg(target_os = "macos")]
mod sdk;
#[cfg(target_os = "macos")]
mod shell;
#[cfg(target_os = "macos")]
mod stage_recovery;

use super::message::Message;
use super::{app_storage, bytes, inventory, profile_for, resolve, stale, Profile};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use scan::{display, usage, Baseline, Confirmation, Scan, THREADS};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

const INK: Color32 = Color32::from_rgb(29, 31, 36);
const ACCENT: Color32 = Color32::from_rgb(0, 102, 222);

#[cfg(target_os = "macos")]
const MUTED: Color32 = Color32::from_rgb(95, 100, 110);
#[cfg(target_os = "macos")]
const LINE: Color32 = Color32::from_rgb(226, 228, 233);

pub fn run(profile: Option<String>) -> Result<()> {
    let Some(_instance) = instance::acquire()? else {
        eprintln!("still はすでに起動しています / still is already running");
        return Ok(());
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 800.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "still",
        options,
        Box::new(move |cc| {
            configure_ui(&cc.egui_ctx);
            let mut app = App {
                japanese_reasons: true,
                ..App::default()
            };
            app.load_names();
            if let Some(name) = profile {
                app.open_profile(&cc.egui_ctx, name);
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(err) = &result {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("still を起動できませんでした")
            .set_description(err.to_string())
            .show();
    }
    result.map_err(|err| anyhow::anyhow!("{err}"))
}

fn configure_ui(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut visuals = egui::Visuals::light();
    let paper = Color32::WHITE;
    let line = Color32::from_rgb(226, 228, 233);
    visuals.panel_fill = paper;
    visuals.window_fill = paper;
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.weak_text_color = Some(Color32::from_rgb(95, 100, 110));
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.inactive.weak_bg_fill = paper;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(240, 245, 253);
    visuals.widgets.hovered.bg_stroke =
        egui::Stroke::new(1.0_f32, Color32::from_rgb(160, 185, 222));
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(220, 235, 254);
    visuals.widgets.active.fg_stroke.color = INK;
    visuals.widgets.hovered.fg_stroke.color = INK;
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(7);
    }
    visuals.widgets.noninteractive.fg_stroke.color = INK;
    visuals.widgets.inactive.fg_stroke.color = INK;
    visuals.selection.bg_fill = Color32::from_rgb(220, 234, 255);
    visuals.selection.stroke.color = ACCENT;
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        for (kind, size) in [
            (egui::TextStyle::Heading, 22.0),
            (egui::TextStyle::Body, 15.0),
            (egui::TextStyle::Button, 15.0),
            (egui::TextStyle::Small, 12.0),
        ] {
            style
                .text_styles
                .insert(kind, egui::FontId::proportional(size));
        }
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
        style.spacing.window_margin = egui::Margin::same(22);
        style.spacing.interact_size.y = controls::HEIGHT;
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.icon_spacing = 8.0;
        style.spacing.button_padding = egui::vec2(controls::PADDING_X, 8.0);
        style.animation_time = if reduce_motion() { 0.0 } else { 0.12 };
    });
    #[cfg(target_os = "macos")]
    let font = PathBuf::from("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc");
    #[cfg(target_os = "windows")]
    let font = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into()))
        .join("Fonts")
        .join("YuGothM.ttc");
    if let Ok(data) = std::fs::read(font) {
        let mut fonts = egui::FontDefinitions::default();
        let japanese = egui::FontData::from_owned(data);
        #[cfg(target_os = "macos")]
        let mut japanese = japanese;
        #[cfg(target_os = "macos")]
        {
            // Hiragino's UI face uses 1083 units/em, matching SF's cap/x-height.
            // The default TTC face is for documents and has a larger character face.
            japanese.index = 1;
            japanese.tweak.y_offset_factor = 0.24;
            if let Ok(data) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
                let mut latin = egui::FontData::from_owned(data);
                // Match visible Latin ascenders to the Japanese UI face; advances
                // and line heights are measured after this optical adjustment.
                latin.tweak.scale = 1.1;
                latin.tweak.coords = egui::epaint::text::VariationCoords::new([("opsz", 17.0)]);
                fonts.font_data.insert("system-ui".into(), latin.into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "system-ui".into());
            }
        }
        fonts
            .font_data
            .insert("system-japanese".into(), japanese.into());
        #[cfg(target_os = "macos")]
        {
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(1, "system-japanese".into());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("system-japanese".into());
        }
        #[cfg(not(target_os = "macos"))]
        for family in fonts.families.values_mut() {
            family.push("system-japanese".into());
        }
        ctx.set_fonts(fonts);
    }
}

fn reduce_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::{process::Command, time::Duration};
        app_storage::command_output(
            Command::new("/usr/bin/defaults").args([
                "read",
                "com.apple.universalaccess",
                "reduceMotion",
            ]),
            Duration::from_secs(1),
        )
        .is_ok_and(|value| value.trim() == "1")
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(target_os = "macos")]
fn open_in_finder(path: &Path) -> Result<()> {
    anyhow::ensure!(path.is_absolute(), "Expected an absolute path");
    let metadata = std::fs::symlink_metadata(path)?;
    let mut command = std::process::Command::new("/usr/bin/open");
    if !metadata.is_dir() {
        command.arg("-R");
    }
    anyhow::ensure!(
        command.arg(path).status()?.success(),
        "Finder could not open {}",
        display(path)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn folder_breadcrumb(ui: &mut egui::Ui, root: &Path, path: &Path) -> Option<PathBuf> {
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        let mut ancestors: Vec<_> = path
            .ancestors()
            .take_while(|part| part.starts_with(root))
            .collect();
        ancestors.reverse();
        for (index, part) in ancestors.into_iter().enumerate() {
            if index > 0 {
                ui.label("/");
            }
            let name = part
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| display(part));
            if ui.link(name).on_hover_text(display(part)).clicked() {
                chosen = Some(part.to_owned());
            }
        }
    });
    chosen
}

fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::popup(&ctx.global_style())
        .inner_margin(24)
        .corner_radius(12)
}

fn text<'a>(japanese: bool, ja: &'a str, en: &'a str) -> &'a str {
    if japanese {
        ja
    } else {
        en
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum View {
    #[default]
    Folders,
    Candidates,
    Search,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Sort {
    #[default]
    Size,
    Largest,
    Change,
}

impl Sort {
    fn label(self, japanese: bool) -> &'static str {
        match self {
            Self::Size => text(japanese, "フォルダ容量", "Folder size"),
            Self::Largest => text(japanese, "最大ファイル", "Largest file"),
            Self::Change => text(japanese, "前回からの増加", "Weekly change"),
        }
    }
}

struct TableRow {
    name: String,
    path: Option<PathBuf>,
    folder: bool,
    size: String,
    change: String,
    order: Option<i128>,
    detail: String,
    detail_ja: Option<String>,
    blocked_reason: Option<Message>,
    removable: bool,
}

enum Outcome {
    Scan(Result<Scan>),
    #[cfg(target_os = "macos")]
    StageReview(Result<super::filesystem::recovery::ReviewedStage>),
    #[cfg(target_os = "macos")]
    StageRemoval(Result<()>),
    Search(Result<inventory::Inventory>),
    Stale(Result<stale::Report>),
    AppStorage(Result<app_storage::Report>),
    #[cfg(target_os = "macos")]
    SdkScan(Result<super::sdk_cleanup::Report>),
    #[cfg(target_os = "macos")]
    SdkRemoval(Result<super::sdk_cleanup::Receipt>),
    #[cfg(target_os = "macos")]
    DockerReview(Result<super::docker::storage::Review>),
    #[cfg(target_os = "macos")]
    DockerRemoval(super::docker::storage::Receipt),
    #[cfg(target_os = "macos")]
    DockerCacheReview(Result<super::docker::storage::CacheCleanup>),
    #[cfg(target_os = "macos")]
    DockerCacheRemoval(super::docker::storage::CacheCleanupResult),
    Review(Result<Confirmation>),
    Inspection(Vec<String>),
    Removal(scan::CleanupReceipt),
}

struct Job {
    result: Receiver<Outcome>,
    label: &'static str,
    removing: bool,
    progress: Option<Arc<inventory::ScanProgress>>,
}

struct StaleFiles {
    sort: crate::cli::StaleSort,
    root: Option<PathBuf>,
    days: u16,
    mib: u64,
    query: String,
    report: Option<stale::Report>,
    error: String,
}

impl Default for StaleFiles {
    fn default() -> Self {
        Self {
            sort: crate::cli::StaleSort::Size,
            root: None,
            days: 90,
            mib: 100,
            query: String::new(),
            report: None,
            error: String::new(),
        }
    }
}

#[derive(Default)]
struct App {
    #[cfg(target_os = "macos")]
    screen: shell::Screen,
    #[cfg(target_os = "macos")]
    app_storage_selected: bool,
    #[cfg(target_os = "macos")]
    projects: projects::Projects,
    #[cfg(target_os = "macos")]
    home: home::Home,
    profile: Profile,
    names: Vec<String>,
    profile_name: Option<String>,
    docker_input: String,
    kube_context: String,
    kube_namespace: String,
    index: usize,
    baseline: HashMap<PathBuf, Baseline>,
    scan: Option<Scan>,
    location: PathBuf,
    view: View,
    japanese_reasons: bool,
    sort: Sort,
    query: String,
    matches: Vec<inventory::Row>,
    omitted_matches: usize,
    rows: Vec<TableRow>,
    focused: Option<usize>,
    marked: BTreeSet<PathBuf>,
    candidate_filter: String,
    inspection: Option<Vec<String>>,
    next_presence_check: Option<std::time::Instant>,
    confirmation: Option<Confirmation>,
    #[cfg(target_os = "macos")]
    stages: stage_recovery::Panel,
    job: Option<Job>,
    save_as: Option<String>,
    status: String,
    cleanup_receipt: Option<scan::CleanupReceipt>,
    stale_files: StaleFiles,
    show_stale_files: bool,
    show_help: bool,
    #[cfg(target_os = "macos")]
    monitor: monitor::Panel,
    show_app_storage: bool,
    xcode_runtimes: bool,
    library_inventory: bool,
    #[cfg(target_os = "macos")]
    browse_folders: bool,
    read_only_browse: bool,
    app_storage: Option<Result<app_storage::Report>>,
    #[cfg(target_os = "macos")]
    sdk: sdk::Panel,
}

impl App {
    fn spawn(
        &mut self,
        ctx: &egui::Context,
        label: &'static str,
        removing: bool,
        work: impl FnOnce() -> Outcome + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let (tx, result) = mpsc::channel();
        let ctx = ctx.clone();
        self.status.clear();
        self.job = Some(Job {
            result,
            label,
            removing,
            progress: None,
        });
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                // A worker can finish after logic has polled this frame. Keep
                // delivery responsive even if a wake-up coalesces with it.
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.status = text(
                    self.japanese_reasons,
                    "処理が中断されました。再スキャンしてから続けてください。",
                    "The operation stopped unexpectedly. Refresh before continuing.",
                )
                .into();
                return;
            }
        };
        self.job = None;
        match result {
            Outcome::Scan(Ok(result)) => {
                if self.sort == Sort::Change && result.previous.is_none() {
                    self.sort = Sort::Size;
                }
                self.baseline
                    .insert(result.root.clone(), result.previous.clone());
                #[cfg(target_os = "macos")]
                {
                    self.projects.refresh(&result);
                    if let Some(path) = self.home.requested_project.take() {
                        self.projects.open_project(path);
                    } else if self.browse_folders {
                        self.projects.active = false;
                    }
                }
                #[cfg(target_os = "macos")]
                if !self.read_only_browse {
                    if let Err(err) = self.home.remember(
                        &result.root,
                        &self.profile,
                        self.profile_name.clone(),
                        self.projects.growth(),
                    ) {
                        self.status = format!(
                            "{} · {err:#}",
                            text(
                                self.japanese_reasons,
                                "前回の場所を保存できませんでした",
                                "Could not save the previous workspace"
                            )
                        );
                    }
                }
                self.location = result.root.clone();
                self.scan = Some(result);
                self.rebuild();
            }
            Outcome::Search(Ok(result)) => {
                self.matches = result.matches;
                self.omitted_matches = result.omitted_matches;
                self.view = View::Search;
                self.rebuild();
            }
            Outcome::Stale(Ok(result)) => self.stale_files.report = Some(result),
            Outcome::Stale(Err(err)) => {
                self.stale_files.error = format!(
                    "{} · {err:#}",
                    text(
                        self.japanese_reasons,
                        "古いファイルの検索に失敗しました",
                        "Could not search for old files"
                    )
                )
            }
            Outcome::AppStorage(result) => self.app_storage = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkScan(result) => self.sdk.report = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkRemoval(result) => self.sdk.receipt = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::DockerReview(result) => self.monitor.set_review(result),
            #[cfg(target_os = "macos")]
            Outcome::DockerRemoval(receipt) => self.monitor.set_receipt(receipt),
            #[cfg(target_os = "macos")]
            Outcome::DockerCacheReview(review) => self.monitor.set_cache_review(review),
            #[cfg(target_os = "macos")]
            Outcome::DockerCacheRemoval(receipt) => self.monitor.set_cache_receipt(receipt),
            Outcome::Review(Ok(mut confirmation)) => {
                if let Some(scan) = self.scan.as_ref() {
                    confirmation.match_scan_sizes(scan);
                    self.confirmation = Some(confirmation);
                }
            }
            #[cfg(target_os = "macos")]
            Outcome::StageReview(result) => self.stages.review = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::StageRemoval(result) => match result {
                Ok(()) => {
                    self.refresh(ctx, false);
                    self.status = text(
                        self.japanese_reasons,
                        "ステージを片付けました。",
                        "Removed the reviewed stage.",
                    )
                    .into();
                }
                Err(error) => self.status = format!("{error:#}"),
            },
            Outcome::Inspection(report) => self.inspection = Some(report),
            Outcome::Removal(results) => {
                #[cfg(target_os = "macos")]
                {
                    self.home.last_measurement = results.before.zip(results.after);
                }
                self.refresh(ctx, false);
                self.cleanup_receipt = Some(results);
            }
            Outcome::Review(Err(err)) => {
                self.status = Message::from_error(&err)
                    .text(self.japanese_reasons)
                    .to_owned();
            }
            Outcome::Scan(Err(err)) | Outcome::Search(Err(err)) => {
                #[cfg(target_os = "macos")]
                {
                    self.home.requested_project = None;
                }
                self.status = format!(
                    "{} · {err:#}",
                    text(
                        self.japanese_reasons,
                        "調査を完了できませんでした",
                        "Could not complete the scan"
                    )
                );
            }
        }
    }

    fn load_names(&mut self) {
        match Profile::names() {
            Ok(names) => self.names = names,
            Err(err) => {
                self.status = format!(
                    "{} · {err:#}",
                    text(
                        self.japanese_reasons,
                        "プロファイル一覧を取得できませんでした",
                        "Could not list profiles"
                    )
                )
            }
        }
    }

    fn open_profile(&mut self, ctx: &egui::Context, name: String) {
        if self.job.is_some() {
            return;
        }
        match Profile::load(&name).and_then(|p| resolve(p, true)) {
            Ok(scope) => {
                self.read_only_browse = false;
                self.profile = profile_for(&scope.roots, &scope.policy, &scope.sources);
                self.kube_context = self
                    .profile
                    .kube
                    .as_ref()
                    .map_or_else(String::new, |source| source.context.clone());
                self.kube_namespace = self
                    .profile
                    .kube
                    .as_ref()
                    .map_or_else(String::new, |source| source.namespace.clone());
                self.docker_input = self.profile.docker_context.clone().unwrap_or_default();
                self.profile_name = Some(name);
                self.index = 0;
                self.baseline.clear();
                self.cleanup_receipt = None;
                self.refresh(ctx, true);
            }
            Err(err) => self.status = display_error(&err, self.japanese_reasons),
        }
    }

    fn add_workspace(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.job.is_some() {
            return;
        }
        match inventory::safe_workspace_dir(&path) {
            Ok(path) => {
                self.read_only_browse = false;
                if let Some(index) = self.profile.roots.iter().position(|root| *root == path) {
                    self.index = index;
                } else {
                    self.profile.roots.push(path.clone());
                    self.profile.cleanup_roots.push(path);
                    self.index = self.profile.roots.len() - 1;
                    self.settings_changed();
                }
                self.cleanup_receipt = None;
                self.refresh(ctx, true);
            }
            Err(err) => self.status = display_error(&err, self.japanese_reasons),
        }
    }

    fn settings_changed(&mut self) {
        self.profile_name = None;
        self.baseline.clear();
    }

    fn refresh(&mut self, ctx: &egui::Context, record: bool) {
        if self.job.is_some() {
            return;
        }
        #[cfg(target_os = "macos")]
        self.navigate(shell::Screen::Workspace);
        self.cleanup_receipt = None;
        self.marked.clear();
        self.matches.clear();
        if self.view == View::Search {
            self.view = View::Folders;
        }
        self.query.clear();
        self.focused = None;
        self.scan = None;
        self.rows.clear();
        let settings = self.profile.clone();
        let index = self.index;
        let name = self.profile_name.clone();
        let read_only = self.read_only_browse;
        let previous = settings
            .roots
            .get(index)
            .and_then(|path| self.baseline.get(path))
            .cloned();
        let progress = Arc::new(inventory::ScanProgress::default());
        let worker_progress = Arc::clone(&progress);
        self.spawn(
            ctx,
            text(
                self.japanese_reasons,
                "フォルダの容量と候補を調査中…",
                "Scanning workspace…",
            ),
            false,
            move || {
                Outcome::Scan(Scan::run(
                    settings,
                    index,
                    name.as_deref(),
                    previous,
                    record,
                    read_only,
                    &worker_progress,
                ))
            },
        );
        if let Some(job) = &mut self.job {
            job.progress = Some(progress);
        }
    }

    fn search(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.scan else { return };
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let root = scan.root.clone();
        let read_only = self.read_only_browse;
        self.marked.clear();
        self.spawn(
            ctx,
            text(self.japanese_reasons, "パスを検索中…", "Searching paths…"),
            false,
            move || {
                Outcome::Search(
                    (if read_only {
                        inventory::safe_dir(&root)
                    } else {
                        inventory::workspace_dir(&root)
                    })
                    .and_then(|root| inventory::scan(&root, THREADS, Some(&query))),
                )
            },
        );
    }

    fn rebuild(&mut self) {
        self.focused = None;
        self.rows.clear();
        let Some(scan) = &self.scan else { return };
        let from_row = |row: &inventory::Row| {
            let delta = if self.view == View::Search && row.is_dir {
                scan.inventory
                    .directories
                    .binary_search_by(|dir| dir.path.cmp(&row.path))
                    .ok()
                    .and_then(|i| scan.delta(&scan.inventory.directories[i]))
            } else {
                scan.delta(row)
            };
            let shown = if self.view == View::Folders {
                row.path.file_name().map(Path::new).unwrap_or(&row.path)
            } else {
                row.path.strip_prefix(&scan.root).unwrap_or(&row.path)
            };
            let size = match self.sort {
                Sort::Largest => row.usage.largest_file_bytes,
                _ => row.usage.size(),
            };
            let mut detail = format!(
                "{} · {} files · largest file {}",
                usage(&row.usage),
                row.usage.files,
                bytes(row.usage.largest_file_bytes)
            );
            if row.usage.skipped != 0 {
                detail.push_str(&format!(
                    " · {} unread or skipped entries",
                    row.usage.skipped
                ));
            }
            let mut detail_ja = format!(
                "{} · {} ファイル · 最大ファイル {}",
                display_size(&usage(&row.usage), true),
                row.usage.files,
                bytes(row.usage.largest_file_bytes)
            );
            if row.usage.skipped != 0 {
                detail_ja.push_str(&format!(" · 読み取り不可／対象外 {} 件", row.usage.skipped));
            }
            TableRow {
                name: display(shown),
                path: Some(row.path.clone()),
                folder: row.is_dir,
                size: bytes(size),
                change: delta.map_or_else(|| "—".into(), |delta| delta.label()),
                order: if self.sort == Sort::Change {
                    delta.map(|d| d.signed_bytes())
                } else {
                    Some(i128::from(size))
                },
                detail,
                detail_ja: Some(detail_ja),
                blocked_reason: None,
                removable: false,
            }
        };
        match self.view {
            View::Folders => {
                self.rows.extend(
                    scan.inventory
                        .directories
                        .iter()
                        .filter(|row| row.path.parent() == Some(self.location.as_path()))
                        .map(from_row),
                );
                // The inventory keeps directory totals, not every individual file.
                // List files only for the folder currently open to avoid retaining
                // millions of file rows after a whole-volume scan.
                if let Ok(entries) = std::fs::read_dir(&self.location) {
                    for entry in entries.flatten() {
                        if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
                            if meta.is_file() && !inventory::is_link(&meta) {
                                let row = inventory::Row {
                                    path: entry.path(),
                                    is_dir: false,
                                    usage: inventory::Usage::file(&meta),
                                };
                                self.rows.push(from_row(&row));
                            }
                        }
                    }
                }
            }
            View::Search => self.rows.extend(self.matches.iter().map(from_row)),
            View::Candidates => {
                for candidate in &scan.files {
                    let row = &candidate.row;
                    let (english, japanese) = candidate.producer.explanation();
                    self.rows.push(TableRow {
                        name: display(row.path.strip_prefix(&scan.root).unwrap_or(&row.path)),
                        path: Some(row.path.clone()),
                        folder: true,
                        size: usage(&row.usage),
                        change: String::new(),
                        order: Some(i128::from(row.usage.size())),
                        detail: english.into(),
                        detail_ja: Some(japanese.into()),
                        blocked_reason: candidate.blocked_reason.clone(),
                        removable: candidate.blocked_reason.is_none(),
                    });
                }
                for item in &scan.external {
                    let command = match item.kind {
                        "Git" => "still worktrees",
                        "Kubernetes" => "still kube",
                        _ => "still docker",
                    };
                    self.rows.push(TableRow {
                        name: format!("{} · {}", item.kind, item.label),
                        path: None,
                        folder: false,
                        size: item.estimated_bytes.map_or_else(|| "—".into(), bytes),
                        change: String::new(),
                        order: item.estimated_bytes.map(i128::from),
                        detail: format!(
                            "{} · {} · {}",
                            item.reason,
                            item.estimate_basis,
                            if item.eligible {
                                format!("Review with `{command}`")
                            } else {
                                "Protected".into()
                            }
                        ),
                        detail_ja: Some(if item.eligible {
                            format!(
                                "{} · ターミナルで確認するコマンド: `{command}`",
                                item.reason.text(true)
                            )
                        } else {
                            String::new()
                        }),
                        blocked_reason: (!item.eligible).then(|| item.reason.clone()),
                        removable: false,
                    });
                }
            }
        }
        if self.view == View::Candidates && !self.candidate_filter.is_empty() {
            let needle = self.candidate_filter.to_lowercase();
            self.rows
                .retain(|row| row.name.to_lowercase().contains(&needle));
        }
        self.rows
            .sort_by(|a, b| b.order.cmp(&a.order).then(a.name.cmp(&b.name)));
    }

    #[cfg(not(target_os = "macos"))]
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("still").size(24.0).strong().color(INK));
            #[cfg(target_os = "macos")]
            if self.scan.is_some()
                && ui
                    .add_enabled(
                        self.job.is_none(),
                        egui::Button::new(text(self.japanese_reasons, "ホーム", "Home")),
                    )
                    .clicked()
            {
                self.scan = None;
                self.marked.clear();
                self.cleanup_receipt = None;
                self.home.update_capacity();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(
                    ui,
                    icons::Icon::Help,
                    text(self.japanese_reasons, "ヘルプ", "Help"),
                )
                .clicked()
                {
                    self.show_help = true;
                    #[cfg(target_os = "macos")]
                    {
                        self.monitor.close();
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    if icons::button(ui, icons::Icon::Docker, "Docker").clicked() {
                        self.monitor.open_tab(monitor::Tab::Docker);
                        if self.monitor.context.is_empty() {
                            self.monitor.context =
                                self.profile.docker_context.clone().unwrap_or_default();
                        }
                        self.show_help = false;
                    }
                    if icons::button(
                        ui,
                        icons::Icon::Disk,
                        text(self.japanese_reasons, "ディスク", "Disk"),
                    )
                    .clicked()
                    {
                        self.monitor.open_tab(monitor::Tab::Disk);
                        self.show_help = false;
                    }
                }
                egui::ComboBox::from_id_salt("language")
                    .width(88.0)
                    .selected_text(if self.japanese_reasons {
                        "日本語"
                    } else {
                        "English"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.japanese_reasons, true, "日本語");
                        ui.selectable_value(&mut self.japanese_reasons, false, "English");
                    });
                ui.add_enabled_ui(self.job.is_none(), |ui| {
                    ui.menu_button(
                        text(self.japanese_reasons, "その他の操作", "More actions"),
                        |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                #[cfg(target_os = "macos")]
                                if self.scan.is_some()
                                    && self.projects.active
                                    && ui
                                        .button(text(
                                            self.japanese_reasons,
                                            "フォルダ探索・一括操作",
                                            "Folders and batch actions",
                                        ))
                                        .clicked()
                                {
                                    self.marked.clear();
                                    self.projects.active = false;
                                    self.rebuild();
                                    ui.close();
                                }
                                self.toolbar_actions(ui, &ctx);
                            });
                        },
                    );
                });
            });
        });
        if self.profile.roots.is_empty()
            || self.scan.is_none()
            || (cfg!(target_os = "macos") && self.profile.roots.len() == 1)
        {
            return;
        }
        ui.add_enabled_ui(self.job.is_none(), |ui| {
            ui.horizontal(|ui| {
                let mut selected = self.index;
                egui::ComboBox::from_id_salt("workspace")
                    .selected_text(display(&self.profile.roots[self.index]))
                    .show_ui(ui, |ui| {
                        for (i, root) in self.profile.roots.iter().enumerate() {
                            ui.selectable_value(&mut selected, i, display(root));
                        }
                    });
                if selected != self.index {
                    self.switch_workspace(&ctx, selected);
                }
            })
        });
    }

    fn switch_workspace(&mut self, ctx: &egui::Context, index: usize) {
        if self.job.is_some() || index == self.index {
            return;
        }
        self.index = index;
        self.cleanup_receipt = None;
        self.refresh(ctx, true);
    }

    fn toolbar_actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        self.stage_menu(ui);
        if ui
            .button(text(
                self.japanese_reasons,
                "古いファイルを探す…",
                "Find old files…",
            ))
            .on_hover_text(if self.japanese_reasons {
                "ホーム内の古い大容量ファイルを探します。"
            } else {
                "Find large, older files in your home folder."
            })
            .clicked()
        {
            ui.close();
            self.show_stale_files = true;
        }
        #[cfg(target_os = "macos")]
        if ui
            .button(text(
                self.japanese_reasons,
                "アプリと SDK の容量…",
                "App storage…",
            ))
            .on_hover_text(if self.japanese_reasons {
                "アプリのデータ・キャッシュ・SDK の版ごとの容量を確認します。"
            } else {
                "Inspect app data, caches, and installed SDK versions."
            })
            .clicked()
        {
            ui.close();
            self.show_app_storage = true;
        }
        if ui
            .button(text(
                self.japanese_reasons,
                "調査フォルダを追加…",
                "Add workspace…",
            ))
            .on_hover_text(if self.japanese_reasons {
                "調査する開発フォルダを選び、容量と掃除候補を表示します。"
            } else {
                "Choose a development folder to scan by size and review cleanup candidates."
            })
            .clicked()
        {
            ui.close();
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.add_workspace(ctx, path);
            }
        }
        ui.menu_button(
            self.profile_name
                .as_deref()
                .unwrap_or(text(
                    self.japanese_reasons,
                    "プロファイルを開く",
                    "Open profile",
                ))
                .to_owned(),
            |ui| {
                if self.names.is_empty() {
                    ui.label(text(
                        self.japanese_reasons,
                        "保存済みプロファイルはありません",
                        "No saved profiles",
                    ));
                }
                if let Some(name) = self
                    .names
                    .iter()
                    .find(|name| ui.button(name.as_str()).clicked())
                    .cloned()
                {
                    ui.close();
                    self.open_profile(ctx, name);
                }
            },
        )
        .response
        .on_hover_text(if self.japanese_reasons {
            "保存した週次プロファイルを開いて、同じ範囲をスキャンします。"
        } else {
            "Open a saved weekly profile and scan its folders."
        });
        let scanned = self.scan.is_some();
        if ui
            .add_enabled(
                scanned,
                egui::Button::new(text(self.japanese_reasons, "別名で保存…", "Save as…")),
            )
            .on_disabled_hover_text(if self.japanese_reasons {
                "先に調査するフォルダを追加してください。"
            } else {
                "Add a workspace first."
            })
            .on_hover_text(if self.japanese_reasons {
                "現在のフォルダと候補の設定を週次プロファイルとして保存します。"
            } else {
                "Save these folders and candidate sources as a weekly profile."
            })
            .clicked()
        {
            ui.close();
            self.save_as = Some(String::new());
        }
        if ui
            .add_enabled(
                !self.profile.roots.is_empty(),
                egui::Button::new(text(self.japanese_reasons, "再スキャン", "Refresh")),
            )
            .clicked()
        {
            ui.close();
            self.cleanup_receipt = None;
            self.refresh(ctx, true);
        }
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.collapsing(
            text(self.japanese_reasons, "候補の調査元", "Candidate sources"),
            |ui| {
                let mut changed = false;
                ui.horizontal(|ui| {
                    if ui
                        .button(text(
                            self.japanese_reasons,
                            "Git リポジトリを追加…",
                            "Add Git repository…",
                        ))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            match inventory::safe_workspace_dir(&path) {
                                Ok(path) if !self.profile.git_repos.contains(&path) => {
                                    self.profile.git_repos.push(path);
                                    changed = true;
                                }
                                Err(err) => {
                                    self.status = display_error(&err, self.japanese_reasons)
                                }
                                _ => {}
                            }
                        }
                    }
                    if !self.profile.git_repos.is_empty()
                        && ui
                            .button(text(
                                self.japanese_reasons,
                                "Git の指定を解除",
                                "Clear Git repositories",
                            ))
                            .clicked()
                    {
                        self.profile.git_repos.clear();
                        changed = true;
                    }
                });
                for repo in &self.profile.git_repos {
                    ui.label(display(repo));
                }
                ui.horizontal(|ui| {
                    ui.label(text(
                        self.japanese_reasons,
                        "ローカル Docker 接続先",
                        "Local Docker context",
                    ));
                    ui.add(controls::field(ui, &mut self.docker_input).hint_text(text(
                        self.japanese_reasons,
                        "例 colima",
                        "e.g. colima",
                    )));
                    if ui
                        .button(text(self.japanese_reasons, "接続先を設定", "Set context"))
                        .clicked()
                    {
                        let context = self.docker_input.trim();
                        let context = (!context.is_empty()).then(|| context.to_owned());
                        if context != self.profile.docker_context {
                            changed = true;
                            self.profile.docker_context = context;
                            if self.profile.docker_context.is_none() {
                                self.profile.docker_cache = false;
                            }
                        }
                    }
                });
                if self.profile.docker_context.is_some() {
                    changed |= ui
                        .checkbox(
                            &mut self.profile.docker_cache,
                            text(
                                self.japanese_reasons,
                                "BuildKit キャッシュを含める",
                                "Include BuildKit cache",
                            ),
                        )
                        .changed();
                }
                ui.separator();
                ui.label(text(
                    self.japanese_reasons,
                    "ローカル Kubernetes / K3s",
                    "Local Kubernetes / K3s",
                ));
                ui.horizontal(|ui| {
                    ui.add(
                        controls::field(ui, &mut self.kube_context)
                            .desired_width(140.0)
                            .hint_text(text(self.japanese_reasons, "接続先", "Context")),
                    );
                    ui.add(
                        controls::field(ui, &mut self.kube_namespace)
                            .desired_width(140.0)
                            .hint_text(text(self.japanese_reasons, "名前空間", "Namespace")),
                    );
                    let ready = !self.kube_context.trim().is_empty()
                        && !self.kube_namespace.trim().is_empty();
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(text(
                                self.japanese_reasons,
                                "kubeconfig を選ぶ…",
                                "Choose kubeconfig…",
                            )),
                        )
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            match super::kube::Source::new(
                                path,
                                self.kube_context.trim().into(),
                                self.kube_namespace.trim().into(),
                            ) {
                                Ok(source) => {
                                    changed |= self.profile.kube.as_ref() != Some(&source);
                                    self.profile.kube = Some(source);
                                }
                                Err(err) => {
                                    self.status = display_error(&err, self.japanese_reasons)
                                }
                            }
                        }
                    }
                    if self.profile.kube.is_some()
                        && ui
                            .button(text(
                                self.japanese_reasons,
                                "Kubernetes の指定を解除",
                                "Clear Kubernetes",
                            ))
                            .clicked()
                    {
                        self.profile.kube = None;
                        changed = true;
                    }
                });
                if let Some(source) = &self.profile.kube {
                    ui.label(format!(
                        "{} / {} · {}",
                        source.context,
                        source.namespace,
                        display(&source.kubeconfig)
                    ));
                }
                ui.separator();
                ui.label(text(
                    self.japanese_reasons,
                    "再生成できる専用出力フォルダ",
                    "Dedicated script outputs (only folders you can regenerate)",
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(text(
                            self.japanese_reasons,
                            "出力フォルダを追加…",
                            "Add output folder…",
                        ))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            let result = inventory::safe_workspace_dir(&path).and_then(|path| {
                                let mut draft = self.profile.clone();
                                if !draft.outputs.contains(&path) {
                                    draft.outputs.push(path);
                                }
                                resolve(draft, true)
                            });
                            match result {
                                Ok(scope) => {
                                    changed |= self.profile.outputs != scope.policy.outputs;
                                    self.profile.outputs = scope.policy.outputs;
                                }
                                Err(err) => {
                                    self.status = display_error(&err, self.japanese_reasons)
                                }
                            }
                        }
                    }
                    if !self.profile.outputs.is_empty()
                        && ui
                            .button(text(
                                self.japanese_reasons,
                                "出力先の指定を解除",
                                "Clear outputs",
                            ))
                            .clicked()
                    {
                        self.profile.outputs.clear();
                        changed = true;
                    }
                });
                for output in &self.profile.outputs {
                    ui.label(display(output));
                }
                if changed {
                    self.settings_changed();
                    self.refresh(&ctx, false);
                }
            },
        );
    }

    fn workspace(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.scan.is_none() {
            if self.job.is_none() {
                #[cfg(target_os = "macos")]
                self.show_home(ui);
                #[cfg(not(target_os = "macos"))]
                ui.vertical_centered(|ui| {
                    ui.heading(text(
                        self.japanese_reasons,
                        "調査する開発フォルダを選んでください",
                        "Choose a development workspace",
                    ));
                    ui.label(text(
                        self.japanese_reasons,
                        "フォルダを追加するか、保存した週次プロファイルを開きます。",
                        "Add a folder or open a saved weekly profile.",
                    ));
                });
            }
            return;
        }
        #[cfg(target_os = "macos")]
        {
            if self.projects.active {
                if ui
                    .button(text(
                        self.japanese_reasons,
                        "フォルダを容量順に見る",
                        "Browse folders by size",
                    ))
                    .clicked()
                {
                    self.projects.active = false;
                    self.marked.clear();
                    self.browse_folders = true;
                    self.location = self.scan.as_ref().unwrap().root.clone();
                    self.view = View::Folders;
                    self.rebuild();
                    return;
                }
                self.project_screen(ui);
                return;
            } else if ui.link("‹ プロジェクト一覧").clicked() {
                self.marked.clear();
                self.browse_folders = false;
                self.projects = projects::Projects::from_scan(self.scan.as_ref().unwrap());
                return;
            }
        }
        self.settings_panel(ui);
        if self.scan.is_none() {
            return;
        }
        ui.horizontal(|ui| {
            let mut selected = self.view;
            ui.selectable_value(
                &mut selected,
                View::Folders,
                text(self.japanese_reasons, "フォルダ", "Folders"),
            );
            ui.selectable_value(
                &mut selected,
                View::Candidates,
                text(self.japanese_reasons, "掃除候補", "Cleanup candidates"),
            );
            if self.view == View::Search {
                ui.selectable_value(
                    &mut selected,
                    View::Search,
                    text(self.japanese_reasons, "検索結果", "Search results"),
                );
            }
            if selected != self.view {
                self.view = selected;
                self.rebuild();
            }
            ui.separator();
            if self.view == View::Candidates {
                if ui
                    .add(
                        controls::field(ui, &mut self.candidate_filter).hint_text(text(
                            self.japanese_reasons,
                            "候補名で絞り込む",
                            "Filter candidates",
                        )),
                    )
                    .changed()
                {
                    self.marked.clear();
                    self.rebuild();
                }
            } else {
                let input = ui.add(controls::field(ui, &mut self.query).hint_text(text(
                    self.japanese_reasons,
                    "フォルダ・ファイル名を検索",
                    "Search paths",
                )));
                if ui
                    .button(text(self.japanese_reasons, "検索", "Search"))
                    .clicked()
                    || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.search(&ctx);
                }
            }
        });
        #[cfg(target_os = "macos")]
        if self.view == View::Folders {
            if let Some(path) =
                folder_breadcrumb(ui, &self.scan.as_ref().unwrap().root, &self.location)
            {
                self.location = path;
                self.rebuild();
            }
        }
        ui.horizontal_wrapped(|ui| {
            if self.view == View::Folders {
                if let Some(parent) = self
                    .location
                    .parent()
                    .filter(|p| p.starts_with(&self.scan.as_ref().unwrap().root))
                {
                    if ui.button(text(self.japanese_reasons, "上のフォルダ", "Up")).clicked() {
                        self.location = parent.to_owned();
                        self.rebuild();
                    }
                }
                #[cfg(not(target_os = "macos"))]
                ui.label(display(&self.location));
            } else if self.view == View::Candidates {
                ui.label(if self.japanese_reasons {
                    format!("候補 {} 件 · 選択 {} 件", self.rows.len(), self.marked.len())
                } else {
                    format!("{} candidates · {} selected", self.rows.len(), self.marked.len())
                });
                if ui.button(text(self.japanese_reasons, "削除できる候補を選択", "Select all eligible"))
                    .on_hover_text(text(self.japanese_reasons, "絞り込み中の削除可能な候補を選択します。親フォルダを選ぶと、その中の候補も対象になります。", "Select eligible folders matching this filter. An enclosing folder includes its nested candidates."))
                    .clicked() {
                    let paths: BTreeSet<_> = self.rows.iter().filter(|row| row.removable)
                        .filter_map(|row| row.path.clone()).collect();
                    self.marked = paths.iter().filter(|path|
                        !path.ancestors().skip(1).any(|ancestor| paths.contains(ancestor)))
                        .cloned().collect();
                    let nested = paths.len() - self.marked.len();
                    self.status = if nested > 0 {
                        if self.japanese_reasons {
                            format!("選択した親フォルダに、配下の候補 {nested} 件を含みます")
                        } else {
                            format!("{nested} nested candidates included in their selected folders")
                        }
                    } else {
                        String::new()
                    };
                }
                if ui.add_enabled(!self.marked.is_empty(), egui::Button::new(text(self.japanese_reasons, "選択を解除", "Clear selection"))).clicked() {
                    self.marked.clear();
                }
            } else if self.omitted_matches > 0 {
                ui.label(if self.japanese_reasons {
                    format!("ほかに {} 件あります。検索条件を絞り込んでください。", self.omitted_matches)
                } else {
                    format!("{} more matches. Narrow the search to see them.", self.omitted_matches)
                });
            }
            if self.view != View::Candidates {
                let mut sort = self.sort;
                egui::ComboBox::from_id_salt("sort")
                    .selected_text(sort.label(self.japanese_reasons))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut sort, Sort::Size, Sort::Size.label(self.japanese_reasons));
                        ui.selectable_value(&mut sort, Sort::Largest, Sort::Largest.label(self.japanese_reasons));
                        if self
                            .scan
                            .as_ref()
                            .is_some_and(|scan| scan.previous.is_some())
                        {
                            ui.selectable_value(&mut sort, Sort::Change, Sort::Change.label(self.japanese_reasons));
                        }
                    });
                if sort != self.sort {
                    self.sort = sort;
                    self.rebuild();
                }
            }
        });
        if let Some(scan) = &self.scan {
            if let Some(previous) = &scan.previous {
                ui.label(format!(
                    "{} {}",
                    text(self.japanese_reasons, "比較元", "Compared with"),
                    previous
                        .completed_at()
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                ));
            } else if scan.recorded {
                ui.label(text(
                    self.japanese_reasons,
                    "今回のスキャンを週次比較の基準として保存しました",
                    "First scan saved as the weekly baseline",
                ));
            }
        }
        self.table(ui);
        if self.view == View::Candidates {
            ui.horizontal_wrapped(|ui| self.candidate_actions(ui, &ctx));
        }
    }

    fn candidate_actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let focused = self
            .focused
            .and_then(|index| self.rows.get(index))
            .and_then(|row| row.path.as_ref());
        let inspect_count = if self.marked.is_empty() {
            usize::from(focused.is_some())
        } else {
            self.marked.len()
        };
        let inspect = egui::Button::new(if self.japanese_reasons {
            format!("選択した {inspect_count} 件を調べる…")
        } else {
            format!("Scan {inspect_count} selected…")
        });
        if ui.add_enabled(inspect_count > 0, inspect)
            .on_hover_text(text(self.japanese_reasons, "追跡ファイル・保護対象・ビルド出力を読み取りだけで確認します。", "Inspect selected folders for tracked files, protected content and build output. Read only."))
            .clicked()
        {
            let paths: Vec<_> = if self.marked.is_empty() {
                focused.cloned().into_iter().collect()
            } else {
                self.marked.iter().cloned().collect()
            };
            let policy = self.scan.as_ref().unwrap().policy.clone();
            let japanese = self.japanese_reasons;
            self.spawn(ctx, text(self.japanese_reasons, "選んだフォルダを確認中…", "Inspecting selected folders…"), false, move || {
                let mut report = Vec::with_capacity(paths.len());
                for path in paths {
                    let detail = match policy.prepare(&path) {
                        Ok(item) => {
                            let (en, ja) = item.producer.explanation();
                            let reason = if japanese { ja } else { en };
                            if japanese {
                                format!("{} ファイル · 論理容量 {} · {reason}", item.files(), bytes(item.logical_bytes()))
                            } else {
                                format!("{} files · {} logical · {reason}", item.files(), bytes(item.logical_bytes()))
                            }
                        }
                        Err(err) => Message::from_error(&err).text(japanese).to_owned(),
                    };
                    report.push(format!("{} · {detail}", display(&path)));
                }
                Outcome::Inspection(report)
            });
        }
        let review = egui::Button::new(if self.japanese_reasons {
            format!("選択した {} 件の削除内容を確認…", self.marked.len())
        } else {
            format!("Review {} selected…", self.marked.len())
        });
        if ui.add_enabled(!self.marked.is_empty(), review).clicked() {
            let paths = self.marked.iter().cloned().collect();
            let policy = self.scan.as_ref().unwrap().policy.clone();
            self.spawn(
                ctx,
                text(
                    self.japanese_reasons,
                    "内容を再照合中… 大きなフォルダは時間がかかります",
                    "Checking contents again… Large folders may take time",
                ),
                false,
                move || Outcome::Review(Confirmation::prepare(policy, paths)),
            );
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.label(match self.view {
                View::Folders => text(
                    self.japanese_reasons,
                    "この場所にサブフォルダはありません",
                    "No subfolders here.",
                ),
                View::Candidates => text(
                    self.japanese_reasons,
                    "この場所に掃除候補は見つかりませんでした",
                    "No cleanup candidates in this workspace.",
                ),
                View::Search => text(
                    self.japanese_reasons,
                    "一致するパスはありません",
                    "No paths matched.",
                ),
            });
            return;
        }
        let candidates = self.view == View::Candidates;
        let show_delta = !candidates && self.scan.as_ref().is_some_and(|s| s.previous.is_some());
        ui.horizontal(|ui| {
            ui.add_space(if candidates { 32.0 } else { 0.0 });
            ui.add_sized(
                [136.0, 23.0],
                egui::Label::new(
                    RichText::new(text(self.japanese_reasons, "容量", "Size")).strong(),
                ),
            );
            if show_delta {
                ui.add_sized(
                    [90.0, 23.0],
                    egui::Label::new(
                        RichText::new(text(self.japanese_reasons, "前回との差", "Change")).strong(),
                    ),
                );
            }
            if candidates {
                ui.add_sized(
                    [220.0, 23.0],
                    egui::Label::new(
                        RichText::new(if self.japanese_reasons {
                            "状態・理由"
                        } else {
                            "Status"
                        })
                        .strong(),
                    ),
                );
            }
            ui.label(RichText::new(text(self.japanese_reasons, "パス", "Path")).strong());
        });
        ui.separator();
        let mut enter = None;
        let mut missing = BTreeSet::new();
        let check_active = candidates
            && self.job.is_none()
            && self.confirmation.is_none()
            && self.inspection.is_none()
            && !self.show_app_storage
            && !self.show_stale_files
            && ui.ctx().input(|input| input.focused);
        let now = std::time::Instant::now();
        let check_visible = if check_active {
            let interval = std::time::Duration::from_secs(10);
            let deadline = self.next_presence_check.get_or_insert(now);
            let due = *deadline <= now;
            if due {
                *deadline = now + interval;
            }
            ui.ctx()
                .request_repaint_after(deadline.saturating_duration_since(now));
            due
        } else {
            false
        };
        egui::ScrollArea::vertical()
            .id_salt("workspace-rows")
            .max_height((ui.available_height() - 132.0).max(110.0))
            .show_rows(ui, 26.0, self.rows.len(), |ui, range| {
                for index in range {
                    let row = &self.rows[index];
                    if check_visible {
                        if let Some(path) = &row.path {
                            if std::fs::symlink_metadata(path)
                                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
                            {
                                missing.insert(path.clone());
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if candidates {
                            if let Some(path) = row.path.as_ref().filter(|_| row.removable) {
                                let mut checked = self.marked.contains(path);
                                if ui
                                    .checkbox(&mut checked, "")
                                    .on_hover_text(text(
                                        self.japanese_reasons,
                                        "削除内容の確認に含める",
                                        "Include in cleanup review",
                                    ))
                                    .changed()
                                {
                                    if checked {
                                        self.marked.insert(path.clone());
                                    } else {
                                        self.marked.remove(path);
                                    }
                                    self.focused = Some(index);
                                }
                            } else {
                                ui.add_space(23.0);
                            }
                        }
                        ui.add_sized(
                            [136.0, 23.0],
                            egui::Label::new(display_size(&row.size, self.japanese_reasons))
                                .truncate(),
                        );
                        if show_delta {
                            ui.add_sized([90.0, 23.0], egui::Label::new(&row.change).truncate());
                        }
                        if candidates {
                            let status = match (&row.blocked_reason, self.japanese_reasons) {
                                (Some(reason), true) => format!("保留: {}", reason.text(true)),
                                (Some(reason), false) => format!("Blocked: {reason}"),
                                (None, true) if row.removable => "確認へ進めます".into(),
                                (None, false) if row.removable => "Ready to review".into(),
                                (None, true) => "ターミナルで確認".into(),
                                (None, false) => "Review in terminal".into(),
                            };
                            let mut label = RichText::new(&status);
                            if row.blocked_reason.is_some() {
                                label = label.color(Color32::from_rgb(134, 66, 35));
                            }
                            ui.add_sized([220.0, 23.0], egui::Label::new(label).truncate())
                                .on_hover_text(status);
                        }
                        if row.folder {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 18.0), egui::Sense::hover());
                            icons::paint(
                                ui,
                                rect,
                                icons::Icon::Folder,
                                ui.visuals().weak_text_color(),
                            );
                        }
                        let response = ui.selectable_label(self.focused == Some(index), &row.name);
                        if response.clicked() {
                            self.focused = Some(index);
                        }
                        if !candidates && row.folder && response.clicked() {
                            enter = row.path.clone();
                        }
                    });
                }
            });
        if !missing.is_empty() {
            if let Some(scan) = &mut self.scan {
                scan.files.retain(|file| !missing.contains(&file.row.path));
            }
            self.marked.retain(|path| !missing.contains(path));
            self.rebuild();
            ui.ctx().request_repaint();
        }
        if let Some(row) = self.focused.and_then(|i| self.rows.get(i)) {
            ui.separator();
            if let Some(path) = &row.path {
                ui.label(display(path));
                #[cfg(target_os = "macos")]
                if ui
                    .button(if self.japanese_reasons {
                        "Finder で開く"
                    } else {
                        "Open in Finder"
                    })
                    .clicked()
                {
                    if let Err(error) = open_in_finder(path) {
                        self.status = display_error(&error, self.japanese_reasons);
                    }
                }
            } else {
                ui.label(&row.name);
            }
            let detail = if self.japanese_reasons {
                row.detail_ja.as_ref().unwrap_or(&row.detail)
            } else {
                &row.detail
            };
            if !detail.is_empty() {
                ui.label(detail);
            }
            if let Some(reason) = &row.blocked_reason {
                let label = if self.japanese_reasons {
                    "保留理由"
                } else {
                    "Blocked"
                };
                ui.label(format!("{label}: {}", reason.text(self.japanese_reasons)));
            }
            if !candidates
                && row.folder
                && ui
                    .button(text(self.japanese_reasons, "フォルダを開く", "Open folder"))
                    .clicked()
            {
                enter = row.path.clone();
            }
        }
        if let Some(path) = enter {
            self.location = path;
            self.view = View::Folders;
            self.rebuild();
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        self.stage_dialog(ctx);
        if self.show_help {
            self.help_dialog(ctx);
        }
        #[cfg(not(target_os = "macos"))]
        if self.show_stale_files {
            self.stale_dialog(ctx, None);
        }
        #[cfg(not(target_os = "macos"))]
        if self.show_app_storage {
            self.app_storage_dialog(ctx, None);
        }
        #[cfg(target_os = "macos")]
        self.sdk_dialog(ctx);
        #[cfg(target_os = "macos")]
        self.docker_confirmation(ctx);
        if let Some(name) = &mut self.save_as {
            let mut close = false;
            let mut save = false;
            let dialog = egui::Modal::new(egui::Id::new("save-profile"))
                .frame(modal_frame(ctx))
                .show(ctx, |ui| {
                    ui.heading(text(
                        self.japanese_reasons,
                        "週次プロファイルを保存",
                        "Save weekly profile",
                    ));
                    ui.label(text(
                        self.japanese_reasons,
                        "名前には半角英小文字・数字・ハイフン・アンダースコアを使えます。",
                        "Use lowercase letters, digits, hyphens or underscores.",
                    ));
                    ui.add(controls::field(ui, name));
                    ui.horizontal(|ui| {
                        close = ui
                            .button(text(self.japanese_reasons, "キャンセル", "Cancel"))
                            .clicked();
                        save = ui
                            .button(text(
                                self.japanese_reasons,
                                "プロファイルを保存",
                                "Save profile",
                            ))
                            .clicked();
                    });
                });
            if close || dialog.should_close() {
                self.save_as = None;
            } else if save {
                let name = name.trim().to_owned();
                match self.profile.create(&name) {
                    Ok(()) => {
                        self.profile_name = Some(name);
                        self.baseline.clear();
                        self.save_as = None;
                        self.load_names();
                        self.refresh(ctx, true);
                    }
                    Err(err) => {
                        self.status = display_error(&err, self.japanese_reasons);
                        self.save_as = None;
                    }
                }
            }
        }
        if let Some(report) = &self.inspection {
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("inspection"))
                .frame(modal_frame(ctx))
                .show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 80.0).clamp(560.0, 940.0));
                    ui.heading(text(
                        self.japanese_reasons,
                        "選んだフォルダの確認結果",
                        "Selected folder scan",
                    ));
                    egui::ScrollArea::vertical().max_height(320.0).show_rows(
                        ui,
                        28.0,
                        report.len(),
                        |ui, range| {
                            for line in &report[range] {
                                ui.add(egui::Label::new(line).truncate())
                                    .on_hover_text(line);
                            }
                        },
                    );
                    #[cfg(target_os = "macos")]
                    {
                        close = controls::button(
                            ui,
                            text(self.japanese_reasons, "閉じる", "Close"),
                            false,
                            true,
                        )
                        .clicked();
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        close = ui
                            .button(text(self.japanese_reasons, "閉じる", "Close"))
                            .clicked();
                    }
                });
            if close || dialog.should_close() {
                self.inspection = None;
            }
        }
        if let Some(confirmation) = &self.confirmation {
            let mut delete = false;
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("delete-review"))
                .frame(modal_frame(ctx))
                .show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 120.0).clamp(460.0, 650.0));
                    ui.heading(if self.japanese_reasons {
                        format!(
                            "{} 件のフォルダを完全に削除しますか？",
                            confirmation.items().len()
                        )
                    } else {
                        format!("Delete {} folders permanently?", confirmation.items().len())
                    });
                    ui.label(text(
                        self.japanese_reasons,
                        "ゴミ箱を経由せず削除します。再生成の方法と対象を確認してください。",
                        "Deletion bypasses Trash. Check these paths and how to regenerate them.",
                    ));
                    ui.small(text(self.japanese_reasons,
                        "一覧はディスク使用量（取得できない場合は論理容量）、以下はファイルの論理容量です。実際の空き容量の変化は削除後に測定します。",
                        "The list uses allocated space when available. Sizes below are logical file sizes. The change in free space is measured after deletion."));
                    ui.add_space(14.0);
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for (item, size) in confirmation.summaries() {
                                ui.label(
                                    RichText::new(
                                        item.path.file_name().unwrap_or_default().to_string_lossy(),
                                    )
                                    .strong(),
                                );
                                ui.add(
                                    egui::Label::new(RichText::new(display(&item.path)).small())
                                        .wrap(),
                                );
                                let (english, japanese) = item.producer.explanation();
                                ui.label(if self.japanese_reasons {
                                    format!(
                                        "{} ファイル · 論理容量 {} · {japanese}",
                                        size.files,
                                        bytes(size.logical_bytes)
                                    )
                                } else {
                                    format!(
                                        "{} files · {} logical · {english}",
                                        size.files,
                                        bytes(size.logical_bytes)
                                    )
                                });
                                if let Some(allocated) = size.scanned_allocated {
                                    ui.small(format!("{} · {}", text(self.japanese_reasons, "スキャン時のディスク使用量", "Allocated at scan"), bytes(allocated)));
                                }
                                ui.add_space(8.0);
                                ui.separator();
                            }
                        });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        #[cfg(target_os = "macos")]
                        {
                            close = controls::button(
                                ui,
                                text(self.japanese_reasons, "キャンセル", "Cancel"),
                                false,
                                true,
                            )
                            .clicked();
                            delete = controls::destructive_button(
                                ui,
                                text(
                                    self.japanese_reasons,
                                    "確認した対象を削除",
                                    "Delete selected",
                                ),
                                true,
                            )
                            .clicked();
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            close = ui
                                .button(text(self.japanese_reasons, "キャンセル", "Cancel"))
                                .clicked();
                            delete = ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(text(
                                            self.japanese_reasons,
                                            "確認した対象を削除",
                                            "Delete selected",
                                        ))
                                        .color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(180, 48, 42))
                                    .min_size(egui::vec2(0.0, controls::HEIGHT)),
                                )
                                .clicked();
                        }
                    });
                });
            if close || dialog.should_close() {
                self.confirmation = None;
            } else if delete {
                let Some(root) = self.scan.as_ref().map(|scan| scan.root.clone()) else {
                    self.confirmation = None;
                    return;
                };
                let confirmation = self.confirmation.take().unwrap();
                self.spawn(
                    ctx,
                    text(
                        self.japanese_reasons,
                        "確認した生成物を削除中…",
                        "Deleting confirmed outputs…",
                    ),
                    true,
                    move || Outcome::Removal(confirmation.remove(&root)),
                );
            }
        }
    }

    fn app_storage_dialog(&mut self, ctx: &egui::Context, page: Option<&mut egui::Ui>) {
        let busy = self.job.is_some();
        let mut start = false;
        let mut close = false;
        let as_page = page.is_some();
        #[cfg(target_os = "macos")]
        let mut review_sdk = false;
        let render = |ui: &mut egui::Ui| {
            if !as_page {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 920.0));
            }
            ui.heading(text(
                self.japanese_reasons,
                "アプリと SDK の容量",
                "App storage",
            ));
            ui.label(text(
                self.japanese_reasons,
                "SDK、ブラウザキャッシュ、アプリのデータを容量順に確認します。",
                "Review SDKs, browser caches, and protected app data by size.",
            ));
            ui.label(text(
                self.japanese_reasons,
                "ホーム内の既知の場所から容量を調べます。この画面は確認専用です。",
                "The scan reads metadata from known folders in your home. This view is read-only.",
            ));
            ui.add_enabled_ui(!busy, |ui| {
                ui.checkbox(
                    &mut self.xcode_runtimes,
                    text(
                        self.japanese_reasons,
                        "Xcode のインストール済みランタイムも確認する",
                        "Ask Xcode for installed simulator runtimes",
                    ),
                );
                ui.checkbox(
                    &mut self.library_inventory,
                    text(
                        self.japanese_reasons,
                        "Library の主な保存先を確認する（時間がかかる場合があります）",
                        "Inventory major Library folders (may take time)",
                    ),
                );
                start = ui
                    .button(text(self.japanese_reasons, "調べる", "Scan"))
                    .clicked();
                #[cfg(target_os = "macos")]
                {
                    review_sdk = ui
                        .button(text(
                            self.japanese_reasons,
                            "Xcode と NDK を版ごとに確認…",
                            "Review Xcode & NDK versions…",
                        ))
                        .clicked();
                }
            });
            if busy && !as_page {
                ui.spinner();
            }
            match &self.app_storage {
                Some(Ok(report)) => {
                    ui.label(if self.japanese_reasons {
                        format!("{} 件", report.rows.len())
                    } else {
                        format!("{} items", report.rows.len())
                    });
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for row in &report.rows {
                                let size = row.size_bytes.map_or_else(|| "—".into(), bytes);
                                let partial = if row.partial {
                                    text(
                                        self.japanese_reasons,
                                        " · 一部を読み取れませんでした",
                                        " · partial read",
                                    )
                                } else {
                                    ""
                                };
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ·  {size} {}{partial}",
                                        row.label,
                                        size_basis(row.size_basis, self.japanese_reasons)
                                    ))
                                    .strong(),
                                );
                                if let Some(path) = &row.path {
                                    ui.label(path);
                                }
                                ui.label(row.reason.text(self.japanese_reasons));
                                #[cfg(target_os = "macos")]
                                if let Some(evidence) = &row.library {
                                    ui.small(evidence.activity_text(self.japanese_reasons));
                                }
                                ui.separator();
                            }
                            for note in &report.notes {
                                ui.label(note);
                            }
                        });
                }
                Some(Err(error)) => {
                    ui.label(format!(
                        "{} · {error:#}",
                        text(
                            self.japanese_reasons,
                            "容量を取得できませんでした",
                            "Could not read storage usage"
                        )
                    ));
                }
                None => {}
            }
            if !as_page {
                close = ui
                    .button(text(self.japanese_reasons, "閉じる", "Close"))
                    .clicked();
            }
        };
        let should_close = if let Some(ui) = page {
            egui::Frame::NONE.show(ui, render);
            false
        } else {
            egui::Modal::new(egui::Id::new("app-storage"))
                .show(ctx, render)
                .should_close()
        };
        if close || should_close {
            self.show_app_storage = false;
        }
        #[cfg(target_os = "macos")]
        if review_sdk {
            self.show_app_storage = false;
            self.app_storage_selected = false;
            self.navigate(shell::Screen::Apps);
            self.sdk.open = true;
            self.scan_sdks(ctx);
        }
        if start {
            self.app_storage = None;
            let xcode = self.xcode_runtimes;
            let library = self.library_inventory;
            self.spawn(
                ctx,
                text(
                    self.japanese_reasons,
                    "アプリの保存容量を調査中…",
                    "Scanning app storage…",
                ),
                false,
                move || Outcome::AppStorage(app_storage::desktop_scan(xcode, library)),
            );
        }
    }
}

fn size_basis(basis: &str, japanese: bool) -> &str {
    if !japanese {
        return basis;
    }
    match basis {
        "allocated" => "ディスク使用量",
        "logical" => "論理容量",
        "reported by Xcode" => "Xcode の報告値",
        other => other,
    }
}

fn display_size(label: &str, japanese: bool) -> String {
    for basis in ["allocated", "logical"] {
        if let Some(amount) = label.strip_suffix(basis) {
            return format!("{amount}{}", size_basis(basis, japanese));
        }
    }
    label.to_owned()
}

fn display_error(error: &anyhow::Error, japanese: bool) -> String {
    if let Some(message) = error.downcast_ref::<Message>() {
        return message.text(japanese).to_owned();
    }
    if japanese {
        format!("処理を完了できませんでした · {error:#}")
    } else {
        format!("{error:#}")
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);
        if self.job.as_ref().is_some_and(|job| job.removing)
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(target_os = "macos")]
        self.desktop_shell(ui);
        #[cfg(not(target_os = "macos"))]
        {
            let panel = egui::CentralPanel::default();
            #[cfg(target_os = "macos")]
            let panel = panel.frame(
                egui::Frame::central_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(24, 14)),
            );
            panel.show_inside(ui, |ui| {
                self.toolbar(ui);
                ui.separator();
                if let Some(job) = &self.job {
                    let progress = job.progress.as_ref().map(|progress| progress.snapshot());
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            if progress
                                .as_ref()
                                .is_some_and(|progress| progress.inventory_done)
                            {
                                text(
                                    self.japanese_reasons,
                                    "候補の安全性と利用状況を確認中…",
                                    "Checking candidate safety and use…",
                                )
                            } else {
                                job.label
                            },
                        );
                    });
                    if let Some(progress) = progress {
                        ui.label(if self.japanese_reasons {
                            format!(
                                "確認済み {} ファイル・{} フォルダ · 論理容量 {}",
                                progress.files,
                                progress.directories,
                                bytes(progress.logical_bytes)
                            )
                        } else {
                            format!(
                                "{} files · {} folders checked · {} logical",
                                progress.files,
                                progress.directories,
                                bytes(progress.logical_bytes)
                            )
                        });
                        if !progress.inventory_done {
                            if let Some(directory) = progress.current_directory {
                                let path = display(&directory);
                                ui.add(egui::Label::new(&path).truncate())
                                    .on_hover_text(path);
                            }
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                }
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).color(Color32::from_rgb(155, 50, 39)));
                }
                if let Some(receipt) = &self.cleanup_receipt {
                    if let Some(error) = &receipt.history_error {
                        ui.label(format!(
                            "{} · {error}",
                            text(
                                self.japanese_reasons,
                                "操作履歴を保存できませんでした",
                                "Could not save cleanup history"
                            )
                        ));
                    }
                    let removed = receipt.outcomes.len() - receipt.held;
                    let japanese = self.japanese_reasons || cfg!(target_os = "macos");
                    let heading = if japanese {
                        format!("削除 {removed} 件・未完了 {} 件", receipt.held)
                    } else {
                        format!("{removed} deleted · {} incomplete", receipt.held)
                    };
                    ui.strong(heading);
                    let change = match (receipt.before, receipt.after) {
                        (Some(before), Some(after)) => format!(
                            "{}{}",
                            if after >= before { "+" } else { "−" },
                            bytes(after.abs_diff(before))
                        ),
                        _ if japanese => "測定できませんでした".into(),
                        _ => "Unavailable".into(),
                    };
                    ui.label(if japanese {
                        format!("実測したボリューム空き容量の変化　{change}")
                    } else {
                        format!("Measured volume free-space change: {change}")
                    });
                    ui.collapsing(
                        if japanese {
                            "項目ごとの結果"
                        } else {
                            "Individual results"
                        },
                        |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(160.0)
                                .show(ui, |ui| {
                                    for (path, result) in &receipt.outcomes {
                                        match result {
                                            Ok(()) => ui.label(path),
                                            Err(reason) => ui.label(format!(
                                                "{path} · {}",
                                                reason.text(japanese)
                                            )),
                                        };
                                    }
                                });
                        },
                    );
                }
                if let Some(scan) = &self.scan {
                    for note in &scan.notes {
                        ui.label(note);
                    }
                }
                ui.add_enabled_ui(self.job.is_none(), |ui| self.workspace(ui));
            });
        }
        self.dialogs(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_browse_orders_files_and_directories_and_can_enter() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        std::fs::create_dir(root.join("folder"))?;
        std::fs::write(root.join("folder/inside"), vec![1; 8192])?;
        std::fs::write(root.join("small"), vec![2; 1024])?;
        let inventory = inventory::scan(&root, 2, None)?;
        let scan = Scan {
            #[cfg(target_os = "macos")]
            kept: Some(keep::KeepRules::default()),
            root: root.clone(),
            policy: Arc::new(super::super::artifacts::Policy {
                roots: vec![],
                outputs: vec![],
                docker_context: None,
            }),
            inventory,
            previous: None,
            recorded: false,
            files: vec![],
            external: vec![],
            notes: vec![],
        };
        let mut app = App {
            location: root.clone(),
            scan: Some(scan),
            ..App::default()
        };
        app.rebuild();
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.rows[0].name, "folder");
        assert_eq!(app.rows[1].name, "small");
        app.location = root.join("folder");
        app.rebuild();
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.rows[0].name, "inside");
        Ok(())
    }

    #[test]
    fn workspace_changes_and_new_jobs_preserve_an_active_removal_and_its_receipt() {
        let ctx = egui::Context::default();
        let (sender, result) = mpsc::channel();
        let mut app = App {
            profile: Profile {
                roots: vec![PathBuf::from("first"), PathBuf::from("second")],
                ..Profile::default()
            },
            job: Some(Job {
                result,
                label: "Removing confirmed output",
                removing: true,
                progress: None,
            }),
            query: "keep this search".into(),
            marked: BTreeSet::from([PathBuf::from("confirmed-output")]),
            ..App::default()
        };

        app.switch_workspace(&ctx, 1);
        app.open_profile(&ctx, "another-profile".into());
        app.add_workspace(&ctx, PathBuf::from("another-workspace"));
        app.refresh(&ctx, true);
        #[cfg(target_os = "macos")]
        {
            for screen in [
                shell::Screen::Apps,
                shell::Screen::Disk,
                shell::Screen::Docker,
                shell::Screen::OldFiles,
                shell::Screen::Workspace,
            ] {
                app.navigate(screen);
            }
            app.scan_sdks(&ctx);
        }
        app.spawn(&ctx, "Another job", false, || {
            Outcome::Inspection(Vec::new())
        });

        assert_eq!(app.index, 0);
        assert_eq!(app.query, "keep this search");
        assert!(app.marked.contains(&PathBuf::from("confirmed-output")));
        assert!(app.status.is_empty());
        let job = app.job.as_ref().unwrap();
        assert!(job.removing);
        assert!(job.progress.is_none());
        sender
            .send(Outcome::Removal(scan::CleanupReceipt {
                outcomes: Vec::new(),
                before: Some(100),
                after: Some(150),
                held: 0,
                history_error: None,
            }))
            .unwrap_or_else(|_| panic!("The original result receiver was dropped"));
        let Outcome::Removal(receipt) = job.result.try_recv().unwrap() else {
            panic!("The original receipt was replaced");
        };
        assert_eq!((receipt.before, receipt.after), (Some(100), Some(150)));
    }
}
