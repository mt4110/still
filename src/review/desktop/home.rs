use super::{bytes, display, egui, App, Profile};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
struct Recent {
    root: PathBuf,
    profile: Profile,
    name: Option<String>,
    completed: DateTime<Utc>,
}

pub(super) struct Home {
    directory: Option<PathBuf>,
    recent: Option<Recent>,
    capacity: Option<(u64, u64)>,
}

fn recent_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "antigravity", "still").context("still data directory")?;
    Ok(dirs.data_local_dir().join("last-workspace.json"))
}

impl Default for Home {
    fn default() -> Self {
        let mut home = Self {
            directory: BaseDirs::new().map(|dirs| dirs.home_dir().to_owned()),
            recent: recent_path()
                .ok()
                .and_then(|path| serde_json::from_reader(fs::File::open(path).ok()?).ok()),
            capacity: None,
        };
        home.update_capacity();
        home
    }
}

impl Home {
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
    ) -> Result<()> {
        self.update_capacity();
        self.recent = Some(Recent {
            root: root.to_owned(),
            profile: profile.clone(),
            name,
            completed: Utc::now(),
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
        let inset = ((ui.available_width() - 680.0) / 2.0).max(20.0);
        ui.horizontal_top(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - inset).clamp(120.0, 680.0));
                ui.add_space(32.0);
                self.home_content(ui);
            });
        });
    }

    fn home_content(&mut self, ui: &mut egui::Ui) {
        use super::{ACCENT, INK, LINE, MUTED, PAPER};
        use egui::RichText;

        ui.label(
            RichText::new("この Mac の空き容量")
                .size(19.0)
                .strong()
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
            ui.label(RichText::new(format!("{:.0}% 使用中", used * 100.0)).color(MUTED));
        } else {
            ui.label("空き容量を取得できませんでした");
        }
        ui.add_space(24.0);
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
        let open = egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, LINE))
            .corner_radius(9)
            .inner_margin(22)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                if let Some(last) = &self.home.recent {
                    ui.label(RichText::new("前回調べた場所").color(MUTED));
                    ui.label(RichText::new(&path_label).size(22.0).strong().color(INK));
                    let completed = last
                        .completed
                        .with_timezone(&Local)
                        .format("%Y/%m/%d %H:%M")
                        .to_string();
                    ui.label(RichText::new(format!("前回のスキャン　{completed}")).color(MUTED));
                } else {
                    ui.label(RichText::new("開発ワークスペース").color(MUTED));
                    ui.label(RichText::new(&path_label).size(22.0).strong().color(INK));
                }
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                ui.add_space(8.0);
                ui.add(
                    egui::Button::new(
                        RichText::new(format!("{name} を調べる"))
                            .color(PAPER)
                            .strong(),
                    )
                    .fill(ACCENT)
                    .corner_radius(7)
                    .min_size(egui::vec2(180.0, 42.0)),
                )
                .clicked()
            })
            .inner;
        if open {
            if let Err(error) = self.resume_scan(ui.ctx(), path.clone()) {
                self.status = format!("{error:#}");
            }
        } else if let Some(workspace) = workspace.filter(|workspace| *workspace != path) {
            if ui.link("_workspace を調べる").clicked() {
                self.start_workspace_scan(ui.ctx(), workspace);
            }
        }
    }

    fn start_workspace_scan(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.profile = Profile::default();
        self.profile_name = None;
        self.docker_input.clear();
        self.kube_context.clear();
        self.kube_namespace.clear();
        self.baseline.clear();
        self.add_workspace(ctx, path);
    }

    fn resume_scan(&mut self, ctx: &egui::Context, path: PathBuf) -> Result<()> {
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
            "前回の場所がプロファイルから外れています。Open profile から調査先を選んでください。",
        )?;
        self.profile = profile;
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
