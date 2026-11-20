use super::{bytes, display, egui, text, App, Profile};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
struct Recent {
    root: PathBuf,
    profile: Profile,
    name: Option<String>,
    completed: DateTime<Utc>,
    #[serde(default)]
    growth: Vec<Growth>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Growth {
    pub(super) path: PathBuf,
    pub(super) label: String,
}

pub(super) struct Home {
    directory: Option<PathBuf>,
    recent: Option<Recent>,
    pub(super) requested_project: Option<PathBuf>,
    capacity: Option<(u64, u64)>,
    pub(super) last_measurement: Option<(u64, u64)>,
    library_growth: Option<super::super::app_storage::observations::RecentGrowth>,
    library_history_unavailable: bool,
    docker_growth: Option<super::super::docker::storage::observations::Observation>,
    docker_history_unavailable: bool,
}

fn recent_path() -> Result<PathBuf> {
    let dirs = crate::storage_paths::project_dirs().context("still data directory")?;
    Ok(dirs.data_local_dir().join("last-workspace.json"))
}

impl Default for Home {
    fn default() -> Self {
        let mut home = Self {
            directory: BaseDirs::new().map(|dirs| dirs.home_dir().to_owned()),
            requested_project: None,
            recent: recent_path()
                .ok()
                .and_then(|path| serde_json::from_reader(fs::File::open(path).ok()?).ok()),
            capacity: None,
            last_measurement: None,
            library_growth: None,
            library_history_unavailable: false,
            docker_growth: None,
            docker_history_unavailable: false,
        };
        if let Some(last) = &home.recent {
            home.last_measurement = super::super::cleanup_history::last_measurement(&last.root)
                .ok()
                .flatten();
        }
        home.update_capacity();
        home.reload_library_growth();
        home.reload_docker_growth();
        home
    }
}

impl Home {
    pub(super) fn reload_docker_growth(&mut self) {
        match super::super::docker::storage::observations::latest() {
            Ok(value) => {
                self.docker_growth = value;
                self.docker_history_unavailable = false;
            }
            Err(_) => {
                self.docker_growth = None;
                self.docker_history_unavailable = true;
            }
        }
    }

    pub(super) fn reload_library_growth(&mut self) {
        match super::super::app_storage::observations::recent_growth() {
            Ok(value) => {
                self.library_growth = value;
                self.library_history_unavailable = false;
            }
            Err(_) => {
                self.library_growth = None;
                self.library_history_unavailable = true;
            }
        }
    }

    pub(super) fn update_capacity(&mut self) {
        self.capacity = self.directory.as_ref().and_then(|dir| {
            let stat = rustix::fs::statvfs(dir).ok()?;
            let total = stat.f_blocks.checked_mul(stat.f_frsize)?;
            let available = stat.f_bavail.checked_mul(stat.f_frsize)?;
            (total > 0 && available <= total).then_some((available, total))
        });
    }

    pub(super) fn remember(
        &mut self,
        root: &Path,
        profile: &Profile,
        name: Option<String>,
        growth: Vec<Growth>,
    ) -> Result<()> {
        self.update_capacity();
        if self.recent.as_ref().is_none_or(|last| last.root != root) {
            self.last_measurement = super::super::cleanup_history::last_measurement(root)
                .ok()
                .flatten();
        }
        self.recent = Some(Recent {
            root: root.to_owned(),
            profile: profile.clone(),
            name,
            completed: Utc::now(),
            growth,
        });
        let path = recent_path()?;
        let dir = path.parent().context("still data directory")?;
        fs::create_dir_all(dir)?;
        let file = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(file.as_file(), &self.recent)?;
        file.persist(path)?;
        Ok(())
    }
}

impl App {
    pub(super) fn show_home(&mut self, ui: &mut egui::Ui) {
        let pending = self.monitor.observation_pending();
        if pending {
            self.monitor.poll(ui.ctx());
            if !self.monitor.observation_pending() {
                self.home.reload_docker_growth();
            }
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_max_width(820.0);
            ui.add_space(10.0);
            self.home_content(ui);
        });
    }

    fn home_content(&mut self, ui: &mut egui::Ui) {
        use super::{ACCENT, INK, LINE, MUTED};
        use egui::RichText;

        ui.label(
            RichText::new(text(
                self.japanese_reasons,
                "この Mac のストレージ",
                "Storage on this Mac",
            ))
            .size(26.0)
            .strong()
            .color(INK),
        );
        ui.label(
            RichText::new(text(
                self.japanese_reasons,
                "容量を調べ、理由を確かめてから整理します。",
                "Explore your storage and review what can be cleaned up.",
            ))
            .color(MUTED),
        );
        ui.add_space(28.0);
        ui.label(
            RichText::new(text(
                self.japanese_reasons,
                "この Mac の空き容量",
                "Free space on this Mac",
            ))
            .size(14.0)
            .color(INK),
        );
        if let Some((free, total)) = self.home.capacity {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(bytes(free)).size(34.0).strong().color(INK));
                ui.label(
                    RichText::new(format!("/ {}", bytes(total)))
                        .size(19.0)
                        .color(MUTED),
                );
            });
            let used = 1.0 - free as f32 / total as f32;
            let (bar, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 7.0), egui::Sense::hover());
            ui.painter().rect_filled(bar, 4, LINE);
            let used_bar =
                egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * used, bar.height()));
            ui.painter().rect_filled(used_bar, 4, ACCENT);
            ui.label(
                RichText::new(format!(
                    "{:.0}% {}",
                    used * 100.0,
                    text(self.japanese_reasons, "使用中", "used")
                ))
                .color(MUTED),
            );
        } else {
            ui.label(text(
                self.japanese_reasons,
                "空き容量を取得できませんでした",
                "Could not read free space",
            ));
        }
        ui.add_space(24.0);
        ui.label(
            RichText::new(text(
                self.japanese_reasons,
                "容量の大きい場所を探す",
                "Find large folders",
            ))
            .size(17.0)
            .strong(),
        );
        ui.label(text(self.japanese_reasons, "選んだ範囲を読み取り専用で調べ、容量順に開けます。権限のない場所や別ボリュームは対象外になります。", "Scan a selected area read-only and browse by size. Inaccessible paths and other volumes are excluded."));
        let mut chosen = None;
        ui.horizontal(|ui| {
            if super::controls::button(
                ui,
                text(
                    self.japanese_reasons,
                    "ホーム全体を調べる",
                    "Scan entire home",
                ),
                self.home.recent.is_none(),
                self.job.is_none(),
            )
            .clicked()
            {
                chosen = self.home.directory.clone();
            }
            if super::controls::button(
                ui,
                text(
                    self.japanese_reasons,
                    "システム領域を調べる",
                    "Scan system volume",
                ),
                false,
                self.job.is_none(),
            )
            .clicked()
            {
                chosen = Some(PathBuf::from("/"));
            }
            if super::controls::button(
                ui,
                text(self.japanese_reasons, "フォルダを選ぶ…", "Choose a folder…"),
                false,
                self.job.is_none(),
            )
            .clicked()
            {
                chosen = rfd::FileDialog::new().pick_folder();
            }
        });
        if let Some(root) = chosen {
            self.start_browse_scan(ui.ctx(), root);
            return;
        }
        ui.add_space(32.0);
        let workspace = self
            .home
            .directory
            .as_ref()
            .map(|dir| dir.join("_workspace"));
        let target = self
            .home
            .recent
            .as_ref()
            .map(|last| &last.root)
            .or(workspace.as_ref());
        let Some(path) = target.cloned() else { return };
        let path_label = self
            .home
            .directory
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
            .map(|relative| display(&Path::new("~").join(relative)))
            .unwrap_or_else(|| display(&path));
        ui.label(
            RichText::new(text(self.japanese_reasons, "週次の掃除", "Weekly cleanup"))
                .size(17.0)
                .strong(),
        );
        ui.add_space(4.0);
        let open = egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, LINE))
            .corner_radius(10)
            .inner_margin(22)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                if let Some(last) = &self.home.recent {
                    ui.label(
                        RichText::new(text(
                            self.japanese_reasons,
                            "前回調べた場所",
                            "Previous workspace",
                        ))
                        .color(MUTED),
                    );
                    ui.label(RichText::new(&path_label).size(20.0).strong().color(INK));
                    let completed = last
                        .completed
                        .with_timezone(&Local)
                        .format("%Y/%m/%d %H:%M")
                        .to_string();
                    ui.label(
                        RichText::new(format!(
                            "{}　{completed}",
                            text(self.japanese_reasons, "前回のスキャン", "Last scanned")
                        ))
                        .color(MUTED),
                    );
                    if let Some((before, after)) = self.home.last_measurement {
                        let change = format!(
                            "{}{}",
                            if after >= before { "+" } else { "−" },
                            bytes(after.abs_diff(before))
                        );
                        ui.label(
                            RichText::new(format!(
                                "{}　{change}",
                                text(
                                    self.japanese_reasons,
                                    "前回の整理時の空き容量変化",
                                    "Free-space change at last cleanup"
                                )
                            ))
                            .strong(),
                        );
                    }
                } else {
                    ui.label(
                        RichText::new(text(
                            self.japanese_reasons,
                            "開発ワークスペース",
                            "Development workspace",
                        ))
                        .color(MUTED),
                    );
                    ui.label(RichText::new(&path_label).size(20.0).strong().color(INK));
                }
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                ui.add_space(8.0);
                let label = if self.japanese_reasons {
                    format!("{name} を調べる")
                } else {
                    format!("Scan {name}")
                };
                super::controls::button(ui, &label, self.home.recent.is_some(), self.job.is_none())
                    .clicked()
            })
            .inner;
        let mut requested = None;
        if let Some(last) = self.home.recent.as_ref().filter(|r| !r.growth.is_empty()) {
            ui.add_space(12.);
            ui.strong(text(
                self.japanese_reasons,
                "前回のスキャンで増えた場所",
                "Growth at last scan",
            ));
            for item in &last.growth {
                let name = display(item.path.file_name().map(Path::new).unwrap_or(&item.path));
                if ui
                    .add_enabled(
                        self.job.is_none(),
                        egui::Link::new(format!(
                            "{name}　{} ›",
                            if self.japanese_reasons {
                                item.label.strip_prefix("new ").map_or_else(
                                    || item.label.clone(),
                                    |size| format!("新規 {size}"),
                                )
                            } else {
                                item.label.clone()
                            }
                        )),
                    )
                    .on_hover_text(display(&item.path))
                    .clicked()
                {
                    requested = Some(item.path.clone());
                }
            }
        }
        ui.add_space(22.);
        ui.strong(text(
            self.japanese_reasons,
            "アプリと Docker の確認",
            "App storage and Docker",
        ));
        if let Some(library) = &self.home.library_growth {
            ui.small(if self.japanese_reasons {
                format!(
                    "Library の前回の調査: {}（割当容量）",
                    library
                        .completed
                        .with_timezone(&Local)
                        .format("%Y/%m/%d %H:%M")
                )
            } else {
                format!(
                    "Last Library scan: {} (allocated size)",
                    library
                        .completed
                        .with_timezone(&Local)
                        .format("%Y/%m/%d %H:%M")
                )
            });
            if library.rows.is_empty() {
                ui.small(text(self.japanese_reasons, "比較できる増加はありません。初回・部分調査では差分を表示できません。", "No comparable increases. A first or partial scan may not produce a comparison."));
            } else {
                ui.small(text(self.japanese_reasons, "前回の調査で増えた場所。詳細は「アプリの容量」を開き、必要に応じて再調査してください。", "Increases at the last scan. Open app storage and scan again if needed to inspect details."));
            }
            for row in &library.rows {
                let previous = row.previous.with_timezone(&Local).format("%Y/%m/%d %H:%M");
                let label = if self.japanese_reasons {
                    format!("{}　+{}　({previous} 比)", row.label, bytes(row.bytes))
                } else {
                    format!("{}　+{} since {previous}", row.label, bytes(row.bytes))
                };
                ui.label(label);
            }
        } else if self.home.library_history_unavailable {
            ui.small(text(
                self.japanese_reasons,
                "Library の前回の観測を読み込めませんでした。",
                "The previous Library observation could not be read.",
            ));
        } else {
            ui.small(text(self.japanese_reasons, "Library はまだ観測していません。明示して調査すると、比較可能な次回から増加を表示します。", "Library has not been inventoried. Comparable increases appear after a later explicit scan."));
        }
        ui.horizontal(|ui| {
            if super::controls::button(
                ui,
                text(
                    self.japanese_reasons,
                    "アプリの容量を開く",
                    "Open app storage",
                ),
                false,
                self.job.is_none(),
            )
            .clicked()
            {
                self.app_storage_selected = true;
                self.navigate(super::shell::Screen::Apps);
            }
            if super::controls::button(
                ui,
                text(self.japanese_reasons, "Docker を開く", "Open Docker"),
                false,
                self.job.is_none(),
            )
            .clicked()
            {
                self.navigate(super::shell::Screen::Docker);
            }
        });
        if self.monitor.observation_pending() {
            ui.small(text(
                self.japanese_reasons,
                "Docker の前回比較を記録中…",
                "Recording Docker comparison…",
            ));
        }
        if self.monitor.observation_failed() {
            ui.small(text(self.japanese_reasons, "Docker比較の一部を記録できませんでした。以下は以前の観測の場合があります。", "Some Docker comparisons could not be recorded. The observation below may be older."));
        }
        if let Some(docker) = &self.home.docker_growth {
            ui.small(format!(
                "Docker {} · {}",
                docker.context,
                docker
                    .measured
                    .with_timezone(&Local)
                    .format("%Y/%m/%d %H:%M")
            ));
            if let Some(prior) = docker.prior {
                ui.small(if self.japanese_reasons {
                    format!(
                        "{} 比（Docker表示の概算差）",
                        prior.with_timezone(&Local).format("%Y/%m/%d %H:%M")
                    )
                } else {
                    format!(
                        "Approximate change since {}",
                        prior.with_timezone(&Local).format("%Y/%m/%d %H:%M")
                    )
                });
                for (category, difference) in &docker.changes {
                    let label = match category.as_str() {
                        "Images" => text(self.japanese_reasons, "イメージ", "Images"),
                        "Containers" => text(self.japanese_reasons, "コンテナ", "Containers"),
                        "Local Volumes" => {
                            text(self.japanese_reasons, "ローカルボリューム", "Local volumes")
                        }
                        "Build Cache" => {
                            text(self.japanese_reasons, "ビルドキャッシュ", "Build cache")
                        }
                        _ => continue,
                    };
                    let sign = if *difference < 0 { "−" } else { "+" };
                    ui.label(format!(
                        "{label}　{sign}{}",
                        bytes(difference.unsigned_abs() as u64)
                    ));
                }
                ui.small(text(self.japanese_reasons, "種類ごとの値には重複や丸め誤差があり、SSDの回収量ではありません。", "Category values can overlap and are rounded; these are not SSD space reclaimed."));
            } else {
                ui.small(text(
                    self.japanese_reasons,
                    "比較できる以前のDocker観測はありません。",
                    "No comparable earlier Docker observation.",
                ));
            }
        } else if self.home.docker_history_unavailable {
            ui.small(text(
                self.japanese_reasons,
                "Dockerの観測履歴を読み込めませんでした。",
                "Could not read Docker observation history.",
            ));
        } else {
            ui.small(text(self.japanese_reasons, "Dockerの容量を調査すると、比較可能な次回から種類ごとの増減を表示します。", "After an explicit Docker scan, comparable changes appear by category on a later scan."));
        }
        if let Some(project) = requested {
            if self.scan.as_ref().is_some_and(|s| s.root == path)
                && self.projects.open_project(project.clone())
            {
                self.marked.clear();
                self.navigate(super::shell::Screen::Workspace);
            } else {
                match self.resume_scan(ui.ctx(), path) {
                    Ok(()) => self.home.requested_project = Some(project),
                    Err(error) => self.status = super::display_error(&error, self.japanese_reasons),
                }
            }
            return;
        }
        if open {
            if let Err(error) = self.resume_scan(ui.ctx(), path.clone()) {
                self.status = super::display_error(&error, self.japanese_reasons);
            }
        } else if let Some(workspace) = workspace.filter(|workspace| *workspace != path) {
            if ui
                .link(text(
                    self.japanese_reasons,
                    "_workspace を調べる",
                    "Scan _workspace",
                ))
                .clicked()
            {
                self.start_workspace_scan(ui.ctx(), workspace);
            }
        }
    }

    fn start_workspace_scan(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.job.is_some() {
            return;
        }
        self.profile = Profile::default();
        self.profile_name = None;
        self.docker_input.clear();
        self.kube_context.clear();
        self.kube_namespace.clear();
        self.baseline.clear();
        self.browse_folders = false;
        self.add_workspace(ctx, path);
    }

    fn start_browse_scan(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.job.is_some() {
            return;
        }
        match super::super::inventory::safe_dir(&path) {
            Ok(path) => {
                self.profile = Profile {
                    roots: vec![path],
                    ..Profile::default()
                };
                self.profile_name = None;
                self.index = 0;
                self.baseline.clear();
                self.browse_folders = true;
                self.read_only_browse = true;
                self.view = super::View::Folders;
                self.cleanup_receipt = None;
                self.navigate(super::shell::Screen::Workspace);
                self.refresh(ctx, false);
            }
            Err(error) => self.status = super::display_error(&error, self.japanese_reasons),
        }
    }

    fn resume_scan(&mut self, ctx: &egui::Context, path: PathBuf) -> Result<()> {
        if self.job.is_some() {
            return Ok(());
        }
        let Some(last) = &self.home.recent else {
            self.start_workspace_scan(ctx, path);
            return Ok(());
        };
        let profile = match &last.name {
            Some(name) => Profile::load(name)?,
            None => last.profile.clone(),
        };
        let index = profile
            .roots
            .iter()
            .position(|root| root == &path)
            .context(
            text(self.japanese_reasons, "前回の場所がプロファイルから外れています。Open profile から調査先を選んでください。", "The previous workspace is no longer in the profile. Choose a workspace from Open profile."),
        )?;
        self.browse_folders = false;
        self.profile = profile;
        self.read_only_browse = false;
        self.profile_name = last.name.clone();
        self.index = index;
        self.docker_input = self.profile.docker_context.clone().unwrap_or_default();
        self.kube_context = self
            .profile
            .kube
            .as_ref()
            .map(|k| k.context.clone())
            .unwrap_or_default();
        self.kube_namespace = self
            .profile
            .kube
            .as_ref()
            .map(|k| k.namespace.clone())
            .unwrap_or_default();
        self.baseline.clear();
        self.refresh(ctx, true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_scan_switches_from_candidates_to_folders() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().canonicalize()?;
        std::fs::create_dir(root.join("large-folder"))?;
        let mut app = App {
            view: super::super::View::Candidates,
            ..App::default()
        };
        app.start_browse_scan(&egui::Context::default(), root.clone());
        assert!(matches!(app.view, super::super::View::Folders));
        assert!(app.read_only_browse);
        let result = app
            .job
            .take()
            .context("scan did not start")?
            .result
            .recv_timeout(std::time::Duration::from_secs(10))?;
        let super::super::Outcome::Scan(scan) = result else {
            anyhow::bail!("unexpected operation")
        };
        let scan = scan?;
        assert!(scan
            .directories
            .children(&root)
            .any(|row| row.path == root.join("large-folder")));
        assert!(scan.files.is_empty());
        Ok(())
    }
}
