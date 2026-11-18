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

const INK: Color32 = Color32::from_rgb(25, 38, 46);
const ACCENT: Color32 = Color32::from_rgb(19, 109, 99);

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
            let mut app = App::default();
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
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = Color32::from_rgb(246, 248, 247);
    visuals.override_text_color = Some(INK);
    visuals.selection.bg_fill = Color32::from_rgb(201, 234, 226);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
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
    Removal(Vec<String>),
}

struct Job {
    result: Receiver<Outcome>,
    label: &'static str,
    removing: bool,
}

struct StaleFiles {
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
    confirmation: Option<Confirmation>,
    job: Option<Job>,
    save_as: Option<String>,
    status: String,
    removal_results: Vec<String>,
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
            Outcome::Stale(Err(err)) => self.stale_files.error = format!("{err:#}"),
            Outcome::AppStorage(result) => self.app_storage = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkScan(result) => self.sdk.report = Some(result),
            #[cfg(target_os = "macos")]
            Outcome::SdkRemoval(result) => self.sdk.receipt = Some(result),
            Outcome::Review(Ok(confirmation)) => self.confirmation = Some(confirmation),
            Outcome::Removal(results) => {
                self.removal_results = results;
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
                self.removal_results.clear();
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
                self.removal_results.clear();
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
        self.rows
            .sort_by(|a, b| b.order.cmp(&a.order).then(a.name.cmp(&b.name)));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("still").size(28.0).strong().color(ACCENT));
            ui.separator();
            if ui.button("Find old files…").clicked() {
                self.show_stale_files = true;
            }
            #[cfg(target_os = "macos")]
            if ui.button("App storage…").clicked() {
                self.show_app_storage = true;
            }
            if ui.button("Add workspace…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.add_workspace(&ctx, path);
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
                        self.open_profile(&ctx, name);
                    }
                },
            );
            let scanned = self.scan.is_some();
            if ui
                .add_enabled(scanned, egui::Button::new("Save as…"))
                .clicked()
            {
                self.save_as = Some(String::new());
            }
            if ui
                .add_enabled(!self.profile.roots.is_empty(), egui::Button::new("Refresh"))
                .clicked()
            {
                self.removal_results.clear();
                self.refresh(&ctx, true);
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
        });
        if self.profile.roots.is_empty() {
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
                self.removal_results.clear();
                self.refresh(&ctx, true);
            }
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
                ui.add_space(70.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Choose a development workspace");
                    ui.label("Add a folder or open a saved weekly profile.");
                });
            }
            return;
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
            let input =
                ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search paths"));
            if ui.button("Search").clicked()
                || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.search(&ctx);
            }
        });
        ui.horizontal(|ui| {
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
                ui.label(display(&self.location));
            } else if self.view == View::Candidates {
                ui.label(format!(
                    "{} candidates  ·  {} selected",
                    self.rows.len(),
                    self.marked.len()
                ));
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
            let button = egui::Button::new(format!("Review {} selected…", self.marked.len()));
            if ui.add_enabled(!self.marked.is_empty(), button).clicked() {
                let paths = self.marked.iter().cloned().collect();
                let policy = self.scan.as_ref().unwrap().policy.clone();
                self.spawn(&ctx, "Checking selected outputs…", false, move || {
                    Outcome::Review(Confirmation::prepare(policy, paths))
                });
            }
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
        egui::ScrollArea::vertical()
            .id_salt("workspace-rows")
            .max_height((ui.available_height() - 132.0).max(110.0))
            .show_rows(ui, 26.0, self.rows.len(), |ui, range| {
                for index in range {
                    let row = &self.rows[index];
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
        if let Some(row) = self.focused.and_then(|i| self.rows.get(i)) {
            ui.separator();
            if let Some(path) = &row.path {
                ui.label(display(path));
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
        if let Some(confirmation) = &self.confirmation {
            let mut delete = false;
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("delete-review")).show(ctx, |ui| {
                ui.set_min_width(500.0);
                ui.heading("Delete these folders permanently?");
                ui.label("These folders will be removed immediately when you confirm.");
                egui::ScrollArea::vertical()
                    .max_height(235.0)
                    .show(ui, |ui| {
                        for item in confirmation.items() {
                            ui.label(display(&item.path));
                            ui.label(format!(
                                "{} files · {} logical · {}",
                                item.files(),
                                bytes(item.logical_bytes()),
                                item.producer.reason()
                            ));
                            ui.separator();
                        }
                    });
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
                let confirmation = self.confirmation.take().unwrap();
                self.spawn(ctx, "Deleting confirmed outputs…", true, move || {
                    Outcome::Removal(confirmation.remove())
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
                    ui.label("Showing the 100 largest results. Choose a smaller folder to narrow the scan.");
                }
                if !report.candidates.is_empty() {
                    ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Filter results by path"));
                    let query = state.query.to_lowercase();
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for row in report.candidates.iter().filter(|row| {
                            query.is_empty() || row.path.to_lowercase().contains(&query)
                        }) {
                            ui.label(RichText::new(&row.path).strong());
                            ui.horizontal(|ui| {
                                let open = match row.open_state {
                                    "currently_open" => "Open in a process",
                                    "not_seen_open" => "No open process found",
                                    "unknown" => "Could not check open processes",
                                    _ => "Open processes unchecked",
                                };
                                ui.label(format!(
                                    "{} logical  ·  Latest activity {}  ·  Modified {}  ·  Accessed {}  ·  {}",
                                    bytes(row.logical_bytes),
                                    stale::date(Some(row.last_activity_at)),
                                    stale::date(row.modified_at),
                                    stale::date(row.accessed_at),
                                    open
                                ));
                                if ui.small_button("Copy path").clicked() {
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
            let (root, days, mib) = (state.root.clone(), state.days, state.mib);
            state.report = None;
            state.error.clear();
            state.query.clear();
            self.spawn(ctx, "Looking for old files…", false, move || {
                Outcome::Stale(stale::scan(root.as_deref(), days, mib))
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
        egui::CentralPanel::default().show_inside(ui, |ui| {
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
            if !self.removal_results.is_empty() {
                ui.collapsing("Cleanup results", |ui| {
                    for result in &self.removal_results {
                        ui.label(result);
                    }
                });
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
