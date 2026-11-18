#[cfg(target_os = "macos")]
mod home;
#[cfg(target_os = "macos")]
mod projects;
mod scan;
#[cfg(target_os = "macos")]
mod sdk;

use super::message::Message;
use super::{app_storage, bytes, inventory, profile_for, resolve, stale, Profile};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use scan::{display, usage, Baseline, Confirmation, Scan, THREADS};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

const INK: Color32 = Color32::from_rgb(41, 46, 50);
const ACCENT: Color32 = Color32::from_rgb(50, 104, 91);

#[cfg(target_os = "macos")]
const PAPER: Color32 = Color32::from_rgb(250, 249, 246);
#[cfg(target_os = "macos")]
const MUTED: Color32 = Color32::from_rgb(88, 99, 95);
#[cfg(target_os = "macos")]
const LINE: Color32 = Color32::from_rgb(223, 223, 218);

pub fn run(profile: Option<String>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1110.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "still",
        options,
        Box::new(move |cc| {
            configure_ui(&cc.egui_ctx);
            let mut app = App {
                japanese_reasons: cfg!(target_os = "macos"),
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
            .set_title("still could not open")
            .set_description(err.to_string())
            .show();
    }
    result.map_err(|err| anyhow::anyhow!("{err}"))
}

fn configure_ui(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut visuals = egui::Visuals::light();
    let paper = Color32::from_rgb(250, 249, 246);
    let line = Color32::from_rgb(223, 223, 218);
    visuals.panel_fill = paper;
    visuals.window_fill = paper;
    visuals.weak_text_color = Some(Color32::from_rgb(88, 99, 95));
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.inactive.weak_bg_fill = paper;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(236, 238, 232);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
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
    visuals.selection.bg_fill = Color32::from_rgb(201, 234, 226);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size = (font.size + 2.0).max(14.0);
        }
        style.spacing.interact_size.y = 30.0;
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
    #[cfg(target_os = "macos")]
    let font = PathBuf::from("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc");
    #[cfg(target_os = "windows")]
    let font = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into()))
        .join("Fonts")
        .join("YuGothM.ttc");
    if let Ok(data) = std::fs::read(font) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "system-japanese".into(),
            egui::FontData::from_owned(data).into(),
        );
        for family in fonts.families.values_mut() {
            family.push("system-japanese".into());
        }
        ctx.set_fonts(fonts);
    }
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
fn finder_breadcrumb(ui: &mut egui::Ui, path: &Path, japanese: bool) -> Option<String> {
    let mut error = None;
    ui.horizontal_wrapped(|ui| {
        let mut ancestors: Vec<_> = path
            .ancestors()
            .filter(|part| part.file_name().is_some())
            .collect();
        ancestors.reverse();
        for (index, part) in ancestors.into_iter().enumerate() {
            if index > 0 {
                ui.label("/");
            }
            let name = part.file_name().unwrap_or_default().to_string_lossy();
            let action = if japanese {
                "Finder で開く"
            } else {
                "Open in Finder"
            };
            if ui
                .link(name)
                .on_hover_text(format!("{action} · {}", display(part)))
                .clicked()
            {
                error = open_in_finder(part).err().map(|err| err.to_string());
            }
        }
    });
    error
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
    fn label(self) -> &'static str {
        match self {
            Self::Size => "Folder size",
            Self::Largest => "Largest file",
            Self::Change => "Weekly change",
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
    Search(Result<inventory::Inventory>),
    Stale(Result<stale::Report>),
    AppStorage(Result<app_storage::Report>),
    #[cfg(target_os = "macos")]
    SdkScan(Result<super::sdk_cleanup::Report>),
    #[cfg(target_os = "macos")]
    SdkRemoval(Result<super::sdk_cleanup::Receipt>),
    Review(Result<Confirmation>),
    Inspection(Vec<String>),
    Removal(scan::CleanupReceipt),
}

struct Job {
    result: Receiver<Outcome>,
    label: &'static str,
    removing: bool,
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
    job: Option<Job>,
    save_as: Option<String>,
    status: String,
    cleanup_receipt: Option<scan::CleanupReceipt>,
    stale_files: StaleFiles,
    show_stale_files: bool,
    show_app_storage: bool,
    xcode_runtimes: bool,
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
        let (tx, result) = mpsc::channel();
        let ctx = ctx.clone();
        self.status.clear();
        self.job = Some(Job {
            result,
            label,
            removing,
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
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.status =
                    "The operation stopped unexpectedly. Refresh before continuing.".into();
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
                if let Err(err) =
                    self.home
                        .remember(&result.root, &self.profile, self.profile_name.clone())
                {
                    self.status = format!("前回の場所を保存できませんでした: {err:#}");
                }
                self.location = result.root.clone();
                #[cfg(target_os = "macos")]
                {
                    self.projects = projects::Projects::from_scan(&result);
                }
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
            Outcome::Stale(Err(err)) => self.stale_files.error = format!("{err:#}"),
            Outcome::AppStorage(result) => self.app_storage = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkScan(result) => self.sdk.report = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkRemoval(result) => self.sdk.receipt = Some(result),
            Outcome::Review(Ok(confirmation)) => self.confirmation = Some(confirmation),
            Outcome::Inspection(report) => self.inspection = Some(report),
            Outcome::Removal(results) => {
                self.cleanup_receipt = Some(results);
                self.refresh(ctx, false);
            }
            Outcome::Scan(Err(err)) | Outcome::Search(Err(err)) | Outcome::Review(Err(err)) => {
                self.status = format!("{err:#}");
            }
        }
    }

    fn load_names(&mut self) {
        match Profile::names() {
            Ok(names) => self.names = names,
            Err(err) => self.status = format!("Could not list profiles: {err:#}"),
        }
    }

    fn open_profile(&mut self, ctx: &egui::Context, name: String) {
        match Profile::load(&name).and_then(|p| resolve(p, true)) {
            Ok(scope) => {
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
            Err(err) => self.status = format!("{err:#}"),
        }
    }

    fn add_workspace(&mut self, ctx: &egui::Context, path: PathBuf) {
        match inventory::safe_workspace_dir(&path) {
            Ok(path) => {
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
            Err(err) => self.status = format!("{err:#}"),
        }
    }

    fn settings_changed(&mut self) {
        self.profile_name = None;
        self.baseline.clear();
    }

    fn refresh(&mut self, ctx: &egui::Context, record: bool) {
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
        let previous = settings
            .roots
            .get(index)
            .and_then(|path| self.baseline.get(path))
            .cloned();
        self.spawn(ctx, "Scanning workspace…", false, move || {
            Outcome::Scan(Scan::run(
                settings,
                index,
                name.as_deref(),
                previous,
                record,
            ))
        });
    }

    fn search(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.scan else { return };
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let root = scan.root.clone();
        self.marked.clear();
        self.spawn(ctx, "Searching paths…", false, move || {
            Outcome::Search(
                inventory::workspace_dir(&root)
                    .and_then(|root| inventory::scan(&root, THREADS, Some(&query))),
            )
        });
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
                detail_ja: None,
                blocked_reason: None,
                removable: false,
            }
        };
        match self.view {
            View::Folders => self.rows.extend(
                scan.inventory
                    .directories
                    .iter()
                    .filter(|row| row.path.parent() == Some(self.location.as_path()))
                    .map(from_row),
            ),
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

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("still").size(27.0).strong().color(INK));
            ui.separator();
            #[cfg(target_os = "macos")]
            if self.scan.is_some() && ui.button("ホーム").clicked() {
                self.scan = None;
                self.marked.clear();
                self.cleanup_receipt = None;
                self.home.update_capacity();
            }
            #[cfg(target_os = "macos")]
            if self.scan.is_none() || self.projects.active {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("その他の操作", |ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            if self.scan.is_some()
                                && self.projects.active
                                && ui.button("フォルダ探索・一括操作").clicked()
                            {
                                self.marked.clear();
                                self.projects.active = false;
                                self.rebuild();
                                ui.close();
                            }
                            self.toolbar_actions(ui, &ctx);
                        });
                    });
                });
                return;
            }
            self.toolbar_actions(ui, &ctx);
        });
        if self.profile.roots.is_empty()
            || self.scan.is_none()
            || (cfg!(target_os = "macos") && self.profile.roots.len() == 1)
        {
            return;
        }
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
                self.index = selected;
                self.cleanup_receipt = None;
                self.refresh(&ctx, true);
            }
        });
    }

    fn toolbar_actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui
            .button("Find old files…")
            .on_hover_text(if self.japanese_reasons {
                "ホーム内の古い大容量ファイルを探します。"
            } else {
                "Find large, older files in your home folder."
            })
            .clicked()
        {
            self.show_stale_files = true;
        }
        #[cfg(target_os = "macos")]
        if ui
            .button("App storage…")
            .on_hover_text(if self.japanese_reasons {
                "アプリのデータ・キャッシュ・SDK の版ごとの容量を確認します。"
            } else {
                "Inspect app data, caches, and installed SDK versions."
            })
            .clicked()
        {
            self.show_app_storage = true;
        }
        if ui
            .button("Add workspace…")
            .on_hover_text(if self.japanese_reasons {
                "調査する開発フォルダを選び、容量と掃除候補を表示します。"
            } else {
                "Choose a development folder to scan by size and review cleanup candidates."
            })
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.add_workspace(ctx, path);
            }
        }
        ui.menu_button(
            self.profile_name
                .as_deref()
                .unwrap_or("Open profile")
                .to_owned(),
            |ui| {
                if self.names.is_empty() {
                    ui.label("No saved profiles");
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
            .add_enabled(scanned, egui::Button::new("Save as…"))
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
            self.save_as = Some(String::new());
        }
        if ui
            .add_enabled(!self.profile.roots.is_empty(), egui::Button::new("Refresh"))
            .clicked()
        {
            self.cleanup_receipt = None;
            self.refresh(ctx, true);
        }
        ui.separator();
        egui::ComboBox::from_id_salt("reason-language")
            .selected_text(if self.japanese_reasons {
                "理由: 日本語"
            } else {
                "Reasons: English"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.japanese_reasons, false, "English");
                ui.selectable_value(&mut self.japanese_reasons, true, "日本語");
            });
        ui.menu_button("Privacy", |ui| {
                ui.set_width(365.0);
                ui.label(RichText::new("No telemetry or usage analytics").strong());
                ui.label("still does not send user data to its developers. Profiles and scan history stay on this device.");
                ui.separator();
                ui.label("Find old files scans only within your home folder.");
                #[cfg(target_os = "macos")]
                ui.label("Workspace scans use folders inside your home folder. macOS controls access to protected locations.");
                #[cfg(not(target_os = "macos"))]
                ui.label("Workspace scans use the folders you select or load from a profile.");
                ui.label("Docker and Kubernetes use the local connections you select.");
            });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.collapsing("Candidate sources", |ui| {
            let mut changed = false;
            ui.horizontal(|ui| {
                if ui.button("Add Git repository…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        match inventory::safe_workspace_dir(&path) {
                            Ok(path) if !self.profile.git_repos.contains(&path) => {
                                self.profile.git_repos.push(path);
                                changed = true;
                            }
                            Err(err) => self.status = format!("{err:#}"),
                            _ => {}
                        }
                    }
                }
                if !self.profile.git_repos.is_empty()
                    && ui.button("Clear Git repositories").clicked()
                {
                    self.profile.git_repos.clear();
                    changed = true;
                }
            });
            for repo in &self.profile.git_repos {
                ui.label(display(repo));
            }
            ui.horizontal(|ui| {
                ui.label("Local Docker context");
                ui.add(egui::TextEdit::singleline(&mut self.docker_input).hint_text("e.g. colima"));
                if ui.button("Set context").clicked() {
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
                    .checkbox(&mut self.profile.docker_cache, "Include BuildKit cache")
                    .changed();
            }
            ui.separator();
            ui.label("Local Kubernetes / K3s");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.kube_context)
                        .desired_width(140.0)
                        .hint_text("Context"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.kube_namespace)
                        .desired_width(140.0)
                        .hint_text("Namespace"),
                );
                let ready =
                    !self.kube_context.trim().is_empty() && !self.kube_namespace.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("Choose kubeconfig…"))
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
                            Err(err) => self.status = format!("{err:#}"),
                        }
                    }
                }
                if self.profile.kube.is_some() && ui.button("Clear Kubernetes").clicked() {
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
            ui.label("Dedicated script outputs (only folders you can regenerate)");
            ui.horizontal(|ui| {
                if ui.button("Add output folder…").clicked() {
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
                            Err(err) => self.status = format!("{err:#}"),
                        }
                    }
                }
                if !self.profile.outputs.is_empty() && ui.button("Clear outputs").clicked() {
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
        });
    }

    fn workspace(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.scan.is_none() {
            if self.job.is_none() {
                #[cfg(target_os = "macos")]
                self.show_home(ui);
                #[cfg(not(target_os = "macos"))]
                ui.vertical_centered(|ui| {
                    ui.heading("Choose a development workspace");
                    ui.label("Add a folder or open a saved weekly profile.");
                });
            }
            return;
        }
        #[cfg(target_os = "macos")]
        {
            if self.projects.active {
                self.project_screen(ui);
                return;
            } else if ui.link("‹ プロジェクト一覧").clicked() {
                self.marked.clear();
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
            ui.selectable_value(&mut selected, View::Folders, "Folders");
            ui.selectable_value(&mut selected, View::Candidates, "Cleanup candidates");
            if self.view == View::Search {
                ui.selectable_value(&mut selected, View::Search, "Search results");
            }
            if selected != self.view {
                self.view = selected;
                self.rebuild();
            }
            ui.separator();
            if self.view == View::Candidates {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.candidate_filter)
                            .hint_text("Filter candidates"),
                    )
                    .changed()
                {
                    self.marked.clear();
                    self.rebuild();
                }
            } else {
                let input =
                    ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search paths"));
                if ui.button("Search").clicked()
                    || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.search(&ctx);
                }
            }
        });
        #[cfg(target_os = "macos")]
        if self.view == View::Folders {
            if let Some(error) = finder_breadcrumb(ui, &self.location, self.japanese_reasons) {
                self.status = error;
            }
        }
        ui.horizontal_wrapped(|ui| {
            if self.view == View::Folders {
                if let Some(parent) = self
                    .location
                    .parent()
                    .filter(|p| p.starts_with(&self.scan.as_ref().unwrap().root))
                {
                    if ui.button("Up").clicked() {
                        self.location = parent.to_owned();
                        self.rebuild();
                    }
                }
                #[cfg(not(target_os = "macos"))]
                ui.label(display(&self.location));
            } else if self.view == View::Candidates {
                ui.label(format!(
                    "{} candidates  ·  {} selected",
                    self.rows.len(),
                    self.marked.len()
                ));
                if ui.button("Select all eligible")
                    .on_hover_text("Select eligible folders matching this filter. An enclosing folder includes its nested candidates.")
                    .clicked() {
                    let paths: BTreeSet<_> = self.rows.iter().filter(|row| row.removable)
                        .filter_map(|row| row.path.clone()).collect();
                    self.marked = paths.iter().filter(|path|
                        !path.ancestors().skip(1).any(|ancestor| paths.contains(ancestor)))
                        .cloned().collect();
                    let nested = paths.len() - self.marked.len();
                    self.status = if nested > 0 {
                        format!("{nested} nested candidates included in their selected folders")
                    } else { String::new() };
                }
                if ui.add_enabled(!self.marked.is_empty(), egui::Button::new("Clear selection")).clicked() {
                    self.marked.clear();
                }
            } else if self.omitted_matches > 0 {
                ui.label(format!(
                    "{} more matches. Narrow the search to see them.",
                    self.omitted_matches
                ));
            }
            if self.view != View::Candidates {
                let mut sort = self.sort;
                egui::ComboBox::from_id_salt("sort")
                    .selected_text(sort.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut sort, Sort::Size, Sort::Size.label());
                        ui.selectable_value(&mut sort, Sort::Largest, Sort::Largest.label());
                        if self
                            .scan
                            .as_ref()
                            .is_some_and(|scan| scan.previous.is_some())
                        {
                            ui.selectable_value(&mut sort, Sort::Change, Sort::Change.label());
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
                    "Compared with {}",
                    previous
                        .completed_at()
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                ));
            } else if scan.recorded {
                ui.label("First scan saved as the weekly baseline");
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
        let inspect = egui::Button::new(format!("Scan {inspect_count} selected…"));
        if ui.add_enabled(inspect_count > 0, inspect)
            .on_hover_text("Inspect selected folders for tracked files, protected content and build output. Read only.")
            .clicked()
        {
            let paths: Vec<_> = if self.marked.is_empty() {
                focused.cloned().into_iter().collect()
            } else {
                self.marked.iter().cloned().collect()
            };
            let policy = self.scan.as_ref().unwrap().policy.clone();
            let japanese = self.japanese_reasons;
            self.spawn(ctx, "Inspecting selected folders…", false, move || {
                let mut report = Vec::with_capacity(paths.len());
                for path in paths {
                    let detail = match policy.prepare(&path) {
                        Ok(item) => {
                            let (en, ja) = item.producer.explanation();
                            let reason = if japanese { ja } else { en };
                            format!("{} files · {} logical · {reason}", item.files(), bytes(item.logical_bytes()))
                        }
                        Err(err) => Message::from_error(&err).text(japanese).to_owned(),
                    };
                    report.push(format!("{} · {detail}", display(&path)));
                }
                Outcome::Inspection(report)
            });
        }
        let review = egui::Button::new(format!("Review {} selected…", self.marked.len()));
        if ui.add_enabled(!self.marked.is_empty(), review).clicked() {
            let paths = self.marked.iter().cloned().collect();
            let policy = self.scan.as_ref().unwrap().policy.clone();
            self.spawn(ctx, "Checking selected outputs…", false, move || {
                Outcome::Review(Confirmation::prepare(policy, paths))
            });
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.label(match self.view {
                View::Folders => "No subfolders here.",
                View::Candidates => "No cleanup candidates in this workspace.",
                View::Search => "No paths matched.",
            });
            return;
        }
        let candidates = self.view == View::Candidates;
        let show_delta = !candidates && self.scan.as_ref().is_some_and(|s| s.previous.is_some());
        ui.horizontal(|ui| {
            ui.add_space(if candidates { 32.0 } else { 0.0 });
            ui.add_sized(
                [136.0, 23.0],
                egui::Label::new(RichText::new("Size").strong()),
            );
            if show_delta {
                ui.add_sized(
                    [90.0, 23.0],
                    egui::Label::new(RichText::new("Change").strong()),
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
            ui.label(RichText::new("Path").strong());
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
                                    .on_hover_text("Include in cleanup review")
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
                        ui.add_sized([136.0, 23.0], egui::Label::new(&row.size).truncate());
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
                        let response = ui.selectable_label(self.focused == Some(index), &row.name);
                        if response.clicked() {
                            self.focused = Some(index);
                        }
                        if !candidates && row.folder && response.double_clicked() {
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
                        self.status = error.to_string();
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
            if !candidates && row.folder && ui.button("Open folder").clicked() {
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
        if self.show_stale_files {
            self.stale_dialog(ctx);
        }
        if self.show_app_storage {
            self.app_storage_dialog(ctx);
        }
        #[cfg(target_os = "macos")]
        if self.sdk.open {
            self.sdk_dialog(ctx);
        }
        if let Some(name) = &mut self.save_as {
            let mut close = false;
            let mut save = false;
            let dialog = egui::Modal::new(egui::Id::new("save-profile")).show(ctx, |ui| {
                ui.heading("Save weekly profile");
                ui.label("Use lowercase letters, digits, hyphens or underscores.");
                ui.text_edit_singleline(name);
                ui.horizontal(|ui| {
                    close = ui.button("Cancel").clicked();
                    save = ui.button("Save profile").clicked();
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
                        self.status = format!("{err:#}");
                        self.save_as = None;
                    }
                }
            }
        }
        if let Some(report) = &self.inspection {
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("inspection")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(560.0, 940.0));
                ui.heading("Selected folder scan");
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
                close = ui.button("Close").clicked();
            });
            if close || dialog.should_close() {
                self.inspection = None;
            }
        }
        if let Some(confirmation) = &self.confirmation {
            let mut delete = false;
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("delete-review")).show(ctx, |ui| {
                ui.set_min_width(500.0);
                ui.heading(format!(
                    "Delete {} folders permanently?",
                    confirmation.items().len()
                ));
                ui.label("These folders will be removed immediately when you confirm.");
                egui::ScrollArea::vertical().max_height(235.0).show_rows(
                    ui,
                    58.0,
                    confirmation.items().len(),
                    |ui, range| {
                        for item in &confirmation.items()[range] {
                            let path = display(&item.path);
                            ui.add(egui::Label::new(&path).truncate())
                                .on_hover_text(path);
                            ui.label(format!(
                                "{} files · {} logical · {}",
                                item.files(),
                                bytes(item.logical_bytes()),
                                item.producer.reason()
                            ));
                            ui.separator();
                        }
                    },
                );
                ui.horizontal(|ui| {
                    close = ui.button("Cancel").clicked();
                    delete = ui
                        .add(
                            egui::Button::new("Delete selected")
                                .fill(Color32::from_rgb(243, 186, 176)),
                        )
                        .clicked();
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
                self.spawn(ctx, "Deleting confirmed outputs…", true, move || {
                    Outcome::Removal(confirmation.remove(&root))
                });
            }
        }
    }

    fn app_storage_dialog(&mut self, ctx: &egui::Context) {
        let busy = self.job.is_some();
        let mut start = false;
        let mut close = false;
        #[cfg(target_os = "macos")]
        let mut review_sdk = false;
        let dialog = egui::Modal::new(egui::Id::new("app-storage")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 920.0));
            ui.heading("App storage");
            ui.label("Review SDKs, browser caches, and protected app data by size.");
            ui.label(
                "The scan reads metadata from known folders in your home. This view is read-only.",
            );
            ui.add_enabled_ui(!busy, |ui| {
                ui.checkbox(
                    &mut self.xcode_runtimes,
                    "Ask Xcode for installed simulator runtimes",
                );
                start = ui.button("Scan").clicked();
                #[cfg(target_os = "macos")]
                {
                    review_sdk = ui.button("Review Xcode & NDK versions…").clicked();
                }
            });
            if busy {
                ui.spinner();
            }
            match &self.app_storage {
                Some(Ok(report)) => {
                    ui.label(format!("{} items", report.rows.len()));
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for row in &report.rows {
                                let size = row.size_bytes.map_or_else(|| "—".into(), bytes);
                                let partial = if row.partial { " · partial read" } else { "" };
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ·  {size} {}{partial}",
                                        row.label, row.size_basis
                                    ))
                                    .strong(),
                                );
                                if let Some(path) = &row.path {
                                    ui.label(path);
                                }
                                ui.label(row.reason.text(self.japanese_reasons));
                                ui.separator();
                            }
                            for note in &report.notes {
                                ui.label(note);
                            }
                        });
                }
                Some(Err(error)) => {
                    ui.label(format!("{error:#}"));
                }
                None => {}
            }
            close = ui.button("Close").clicked();
        });
        if close || dialog.should_close() {
            self.show_app_storage = false;
        }
        #[cfg(target_os = "macos")]
        if review_sdk {
            self.show_app_storage = false;
            self.sdk.open = true;
            self.scan_sdks(ctx);
        }
        if start {
            self.app_storage = None;
            let xcode = self.xcode_runtimes;
            self.spawn(ctx, "Scanning app storage…", false, move || {
                Outcome::AppStorage(app_storage::scan(xcode))
            });
        }
    }

    fn stale_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let busy = self.job.is_some();
        let state = &mut self.stale_files;
        let dialog = egui::Modal::new(egui::Id::new("old-files")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 920.0));
            ui.heading("Find old files");
            ui.label("Search only within your home folder. still has no telemetry.");
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label(state.root.as_deref().map_or_else(|| "Home (~)".into(), display));
                    if ui.button("Choose folder…").clicked() {
                        let mut picker = rfd::FileDialog::new();
                        if let Some(root) = state.root.clone().or_else(|| {
                            directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned())
                        }) {
                            picker = picker.set_directory(root);
                        }
                        if let Some(path) = picker.pick_folder() {
                            state.root = Some(path);
                            state.report = None;
                            state.error.clear();
                        }
                    }
                    if ui.add_enabled(state.root.is_some(), egui::Button::new("Use home")).clicked() {
                        state.root = None;
                        state.report = None;
                        state.error.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Days since last activity:");
                    ui.add(egui::DragValue::new(&mut state.days).range(1..=3650));
                    ui.label("Minimum size (MiB):");
                    ui.add(egui::DragValue::new(&mut state.mib).range(1..=1_000_000));
                    start = ui.button("Scan").clicked();
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Sort by");
                    use crate::cli::StaleSort;
                    for (order, label) in [(StaleSort::Size, "Largest"), (StaleSort::Modified, "Oldest modified"), (StaleSort::Created, "Oldest created")] {
                        if ui.selectable_value(&mut state.sort, order, label).changed() {
                            if let Some(report) = &mut state.report {
                                report.set_order(order);
                            }
                        }
                    }
                });
            });
            if busy {
                ui.spinner();
            }
            if !state.error.is_empty() {
                ui.label(&state.error);
            }
            if let Some(report) = &state.report {
                ui.label(format!(
                    "{} · {} matches · {} with unknown dates excluded · {} unread entries",
                    report.root, report.candidates.len() + report.omitted, report.unknown_age, report.skipped
                ));
                if report.omitted > 0 {
                    ui.label("Showing the first 100 matches in the selected order.");
                }
                if !report.candidates.is_empty() {
                    ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Filter results by path"));
                    let query = state.query.to_lowercase();
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for row in report.candidates.iter().filter(|row| {
                            query.is_empty() || row.path.to_lowercase().contains(&query)
                        }) {
                            ui.label(RichText::new(&row.path).strong());
                            ui.horizontal_wrapped(|ui| {
                                let open = match row.open_state {
                                    "currently_open" => "Open in a process",
                                    "not_seen_open" => "No open process found",
                                    "unknown" => "Could not check open processes",
                                    _ => "Open processes unchecked",
                                };
                                ui.label(format!(
                                    "{} logical  ·  Latest activity {}  ·  Modified {}  ·  Created {}  ·  Accessed {}  ·  {}",
                                    bytes(row.logical_bytes),
                                    stale::date(Some(row.last_activity_at)),
                                    stale::date(row.modified_at),
                                    stale::date(row.created_at),
                                    stale::date(row.accessed_at),
                                    open
                                ));
                                #[cfg(target_os = "macos")]
                                if ui.button(if self.japanese_reasons { "Finder で表示" } else { "Show in Finder" }).clicked() {
                                    if let Err(err) = open_in_finder(&row.native_path) {
                                        state.error = err.to_string();
                                    }
                                }
                                if ui.button("Copy path").clicked() {
                                    ctx.copy_text(row.path.clone());
                                }
                            });
                            ui.separator();
                        }
                    });
                }
            }
            ui.label("Access dates can be incomplete. Check what a file is before removing it.");
            close = ui.button("Close").clicked();
        });
        if close || dialog.should_close() {
            self.show_stale_files = false;
        }
        if start {
            let (root, days, mib, sort) = (state.root.clone(), state.days, state.mib, state.sort);
            state.report = None;
            state.error.clear();
            state.query.clear();
            self.spawn(ctx, "Looking for old files…", false, move || {
                Outcome::Stale(stale::scan(root.as_deref(), days, mib, sort))
            });
        }
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
        let panel = egui::CentralPanel::default();
        #[cfg(target_os = "macos")]
        let panel = panel.frame(
            egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::symmetric(24, 14)),
        );
        panel.show_inside(ui, |ui| {
            ui.add_enabled_ui(self.job.is_none(), |ui| self.toolbar(ui));
            ui.separator();
            if let Some(job) = &self.job {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(job.label);
                });
            }
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(Color32::from_rgb(155, 50, 39)));
            }
            if let Some(receipt) = &self.cleanup_receipt {
                let removed = receipt.outcomes.len() - receipt.held;
                let japanese = self.japanese_reasons || cfg!(target_os = "macos");
                let heading = if japanese {
                    format!("削除 {removed} 件・保留 {} 件", receipt.held)
                } else {
                    format!("{removed} deleted · {} held", receipt.held)
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
                                        Err(reason) => {
                                            ui.label(format!("{path} · {}", reason.text(japanese)))
                                        }
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
        self.dialogs(&ctx);
    }
}
