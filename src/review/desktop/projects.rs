use super::super::{artifacts::Producer, history::Delta};
use super::{bytes, display, egui, open_in_finder, App, Message, Outcome, Scan};
use super::{text, ACCENT, LINE, MUTED};
use egui::{Align, RichText, Stroke};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn divider(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1_f32, LINE));
}

// Reserve exactly the height passed to show_rows. Contents cannot move the
// next virtual row when a label is long or the scrollbar changes width.
fn virtual_row(
    ui: &mut egui::Ui,
    id: &Path,
    height: f32,
    selected: bool,
) -> (egui::Ui, egui::Response) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("project-row", id)),
        egui::Sense::click(),
    );
    let duration = ui.style().animation_time.min(0.12);
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), duration);
    let selected =
        ui.ctx()
            .animate_bool_with_time(response.id.with("selected"), selected, duration);
    let opacity = (selected * 0.12 + hover * 0.035).min(0.16);
    ui.painter().rect_filled(
        rect.shrink(1.),
        5.,
        egui::Color32::from_rgba_unmultiplied(
            ACCENT.r(),
            ACCENT.g(),
            ACCENT.b(),
            (opacity * 255.) as u8,
        ),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1_f32, LINE),
    );
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect.shrink2(egui::vec2(8., 4.))),
    );
    row.set_clip_rect(row.clip_rect().intersect(rect));
    (row, response)
}

#[derive(Default)]
pub(super) struct Projects {
    root: PathBuf,
    rows: Vec<Project>,
    bulk_paths: BTreeSet<PathBuf>,
    reclaim: u64,
    selected: Option<PathBuf>,
    focused: Option<PathBuf>,
    query: String,
    by_candidate: bool,
    pub(super) active: bool,
}

struct Project {
    path: PathBuf,
    size: u64,
    reclaim: u64,
    delta: Option<Delta>,
    files: Vec<usize>,
    reviewable_cargo_parts: BTreeMap<PathBuf, usize>,
}

fn name(path: &Path) -> String {
    display(path.file_name().map(Path::new).unwrap_or(path))
}

fn effect(p: Producer, japanese: bool) -> &'static str {
    if !japanese {
        return match p {
            Producer::Cargo | Producer::CargoRelocated => {
                "Cargo rebuilds this output on the next build."
            }
            Producer::CargoIncremental => {
                "Cargo regenerates the incremental cache on the next build."
            }
            Producer::CargoDependencies => {
                "The next build or test regenerates these dependency artifacts."
            }
            Producer::NextJs => "Build again before running the Next.js application.",
            Producer::Vite => "The next build regenerates these distribution files.",
            Producer::PythonBytecode => "Python regenerates bytecode when the code runs again.",
            Producer::DeclaredOutput => {
                "Confirm that the original script can regenerate this dedicated output."
            }
            other => other.explanation().0,
        };
    }
    match p {
        Producer::Cargo | Producer::CargoRelocated => {
            "Cargo のビルド出力。次回のビルドで作り直します。"
        }
        Producer::CargoIncremental => "Cargo の差分ビルド用キャッシュ。次のビルドで作り直します。",
        Producer::CargoDependencies => {
            "Cargo の deps 内のビルド出力。次のビルドやテストで作り直します。"
        }
        Producer::NextJs => "Next.js のビルド出力。実行には再ビルドが必要です。",
        Producer::Vite => "Vite の配布用ファイル。次のビルドで再生成します。",
        Producer::PythonBytecode => "Python のバイトコード。次の実行で再生成できます。",
        Producer::DeclaredOutput => {
            "登録した専用出力先。元のスクリプトで再生成できるか確認してください。"
        }
        other => other.explanation().1,
    }
}

fn candidate_header(ui: &mut egui::Ui, japanese: bool) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.), egui::Sense::hover());
    let mut header =
        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8., 0.))));
    let width = header.available_width();
    let status_width = if width >= 460. { 92. } else { 64. };
    let name_width = (width
        - super::controls::HEIGHT
        - 82.
        - status_width
        - 3. * header.spacing().item_spacing.x)
        .max(40.);
    header.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(super::controls::HEIGHT, 28.),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_width(super::controls::HEIGHT);
            },
        );
        for (label, width, align) in [
            (text(japanese, "項目", "Item"), name_width, Align::Min),
            (text(japanese, "サイズ", "Size"), 82., Align::Max),
            (text(japanese, "状態", "Status"), status_width, Align::Min),
        ] {
            super::controls::cell(ui, width, 28., align, |ui| {
                super::controls::label(ui, label, 12., MUTED, align, egui::Sense::hover());
            });
        }
    });
}

#[derive(Default)]
struct InspectorActions {
    inspect: Option<PathBuf>,
    finder_error: Option<String>,
    keep: Option<(PathBuf, bool)>,
}

fn candidate_inspector(
    ui: &mut egui::Ui,
    candidate: &super::super::artifacts::FileCandidate,
    project: &Project,
    japanese: bool,
    height: f32,
    kept: Option<&super::keep::KeepRules>,
    actions: &mut InspectorActions,
) {
    let path = &candidate.row.path;
    ui.set_max_width(ui.available_width());
    egui::ScrollArea::vertical().id_salt((path, "inspector"))
        .max_height(height).show(ui, |ui| {
            ui.add_space(8.);
            ui.label(RichText::new(name(path)).size(23.).strong());
            ui.label(RichText::new(bytes(candidate.row.usage.size())).size(22.).strong());
            ui.label(RichText::new(text(japanese,
                if candidate.row.usage.allocated_bytes.is_some() { "ディスク使用量" } else { "論理容量" },
                if candidate.row.usage.allocated_bytes.is_some() { "Allocated space" } else { "Logical size" },
            )).size(12.).color(MUTED));
            ui.add_space(16.);
            ui.label(RichText::new(text(japanese, "パス", "Path")).size(12.).color(MUTED));
            ui.add(egui::Label::new(RichText::new(display(path)).monospace().size(12.)).wrap());
            ui.add_space(16.);
            divider(ui);
            ui.add_space(12.);
            ui.strong(text(japanese, "判定理由", "Decision"));
            if let Some(reason) = &candidate.blocked_reason {
                ui.label(RichText::new(text(japanese, "保留", "On hold")).color(MUTED));
                ui.label(reason.text(japanese));
                if let Some(count) = project.reviewable_cargo_parts.get(path) {
                    ui.label(if japanese { format!("全体の削除は保留です。下位の {count} 件は個別に確認できます。") }
                        else { format!("Whole-target removal is on hold. {count} subfolders can be reviewed individually.") });
                }
            } else {
                ui.label(text(japanese, "確認できる掃除候補です。", "Available for cleanup review."));
                let explanation = candidate.producer.explanation();
                ui.label(if japanese { explanation.1 } else { explanation.0 });
            }
            ui.add_space(14.);
            divider(ui);
            ui.add_space(12.);
            ui.strong(text(japanese, "削除した場合", "After removal"));
            ui.label(effect(candidate.producer, japanese));
            ui.add_space(8.);
            ui.label(RichText::new(text(japanese,
                "削除前に、内容の変化と使用中のファイルを再確認します。実際に空いた容量は処理後に測定します。",
                "Contents and open files are checked again before removal. Free-space change is measured afterward.",
            )).size(12.).color(MUTED));
            ui.add_space(18.);
            if let Some(rules) = kept {
                let existing = rules.conflict(path);
                if let Some(held) = existing {
                    ui.small(format!("{} · {}", text(japanese, "残す設定", "Kept folder"), display(held)));
                }
                let label = if existing.is_some() {
                    text(japanese, "この保持設定を解除", "Remove this keep rule")
                } else {
                    text(japanese, "次回以降も残す", "Keep in future scans")
                };
                if super::controls::button(ui, label, false, true).clicked() {
                    actions.keep = Some((existing.unwrap_or(path).to_owned(), existing.is_none()));
                }
            }
            if super::controls::button(ui, text(japanese, "Finder で開く ↗", "Open in Finder ↗"), false, true).clicked() {
                actions.finder_error = open_in_finder(path).err().map(|error| super::display_error(&error, japanese));
            }
            if super::controls::button(ui, text(japanese, "この項目を再確認", "Inspect this item"), false, true).clicked() {
                actions.inspect = Some(path.clone());
            }
        });
}

impl Projects {
    pub(super) fn from_scan(scan: &Scan) -> Self {
        let mut projects: BTreeMap<_, _> = scan
            .directories
            .children(&scan.root)
            .map(|r| {
                (
                    r.path.clone(),
                    Project {
                        path: r.path.clone(),
                        size: r.usage.size(),
                        reclaim: 0,
                        delta: scan.delta(&r),
                        files: vec![],
                        reviewable_cargo_parts: BTreeMap::new(),
                    },
                )
            })
            .collect();
        let eligible: BTreeSet<_> = scan
            .files
            .iter()
            .filter(|c| c.blocked_reason.is_none())
            .map(|c| c.row.path.as_path())
            .collect();
        let held_cargo_targets: BTreeSet<_> = scan
            .files
            .iter()
            .filter(|c| {
                c.blocked_reason.is_some()
                    && matches!(c.producer, Producer::Cargo | Producer::CargoRelocated)
            })
            .map(|c| c.row.path.as_path())
            .collect();
        let mut bulk_paths = BTreeSet::new();
        for (index, candidate) in scan.files.iter().enumerate() {
            let path = &candidate.row.path;
            let Some(component) = path
                .strip_prefix(&scan.root)
                .ok()
                .and_then(|p| p.iter().next())
            else {
                continue;
            };
            if let Some(project) = projects.get_mut(&scan.root.join(component)) {
                project.files.push(index);
                if candidate.blocked_reason.is_none()
                    && matches!(
                        candidate.producer,
                        Producer::CargoIncremental | Producer::CargoDependencies
                    )
                {
                    if let Some(target) = path
                        .ancestors()
                        .skip(1)
                        .find(|p| held_cargo_targets.contains(p))
                    {
                        *project
                            .reviewable_cargo_parts
                            .entry(target.to_owned())
                            .or_default() += 1;
                    }
                }
                if candidate.blocked_reason.is_none()
                    && !path.ancestors().skip(1).any(|p| eligible.contains(p))
                {
                    bulk_paths.insert(path.clone());
                    project.reclaim = project.reclaim.saturating_add(candidate.row.usage.size());
                }
            }
        }
        let mut result = Self {
            root: scan.root.clone(),
            reclaim: projects
                .values()
                .fold(0u64, |sum, p| sum.saturating_add(p.reclaim)),
            bulk_paths,
            rows: projects.into_values().collect(),
            active: true,
            ..Default::default()
        };
        result.sort();
        result
    }
    pub(super) fn growth(&self) -> Vec<super::home::Growth> {
        let mut changes: Vec<_> = self
            .rows
            .iter()
            .filter_map(|project| {
                project
                    .delta
                    .filter(|d| d.signed_bytes() > 0)
                    .map(|delta| (delta, &project.path))
            })
            .collect();
        changes.sort_unstable_by(|(a, pa), (b, pb)| {
            b.signed_bytes()
                .cmp(&a.signed_bytes())
                .then_with(|| pa.cmp(pb))
        });
        changes
            .into_iter()
            .take(3)
            .map(|(delta, path)| super::home::Growth {
                path: path.clone(),
                label: delta.label(),
            })
            .collect()
    }

    pub(super) fn open_project(&mut self, path: PathBuf) -> bool {
        if self.rows.iter().any(|project| project.path == path) {
            self.active = true;
            self.selected = Some(path);
            self.focused = None;
            self.query.clear();
            return true;
        }
        false
    }

    pub(super) fn refresh(&mut self, scan: &Scan) {
        let mut next = Self::from_scan(scan);
        if self.root == next.root {
            next.by_candidate = self.by_candidate;
            next.sort();
            next.selected = self
                .selected
                .take()
                .filter(|path| next.rows.iter().any(|p| p.path == *path));
            if next.selected.is_some() {
                next.query = std::mem::take(&mut self.query);
                next.focused = self
                    .focused
                    .take()
                    .filter(|path| scan.files.iter().any(|c| c.row.path == *path));
            }
        }
        *self = next;
    }

    fn sort(&mut self) {
        let value = |p: &Project| if self.by_candidate { p.reclaim } else { p.size };
        self.rows
            .sort_by(|a, b| value(b).cmp(&value(a)).then_with(|| a.path.cmp(&b.path)));
    }
}

impl App {
    pub(super) fn project_screen(&mut self, ui: &mut egui::Ui) {
        if let Some(index) = self
            .projects
            .selected
            .as_ref()
            .and_then(|path| self.projects.rows.iter().position(|p| p.path == *path))
        {
            self.project_detail(ui, index);
        } else {
            self.project_list(ui);
        }
    }

    fn project_detail(&mut self, ui: &mut egui::Ui, index: usize) {
        let scan = self.scan.as_ref().unwrap();
        let project = &self.projects.rows[index];
        let japanese = self.japanese_reasons;
        if ui
            .link(if japanese {
                format!("‹ {}", name(&scan.root))
            } else {
                format!("‹ Back to {}", name(&scan.root))
            })
            .clicked()
        {
            self.projects.selected = None;
            self.projects.focused = None;
            self.projects.query.clear();
            return;
        }
        ui.add_space(8.);
        ui.label(RichText::new(name(&project.path)).size(28.).strong());
        ui.add(egui::Label::new(RichText::new(display(&project.path)).color(MUTED)).truncate())
            .on_hover_text(display(&project.path));
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{}  {}",
                text(japanese, "使用量", "Used"),
                bytes(project.size)
            ));
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "{}  {}",
                    text(japanese, "掃除候補", "Candidates"),
                    bytes(project.reclaim)
                ))
                .strong(),
            );
        });
        ui.add_space(14.);
        ui.add(
            super::controls::field(ui, &mut self.projects.query)
                .hint_text(text(japanese, "候補を検索", "Search candidates"))
                .desired_width(320.),
        );
        ui.add_space(10.);
        let query = self.projects.query.to_lowercase();
        let visible: Vec<_> = project
            .files
            .iter()
            .copied()
            .filter(|&i| {
                query.is_empty()
                    || display(&scan.files[i].row.path)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect();
        let mut focused = self
            .projects
            .focused
            .clone()
            .filter(|path| {
                project
                    .files
                    .iter()
                    .any(|&i| scan.files[i].row.path == *path)
            })
            .or_else(|| visible.first().map(|&i| scan.files[i].row.path.clone()));
        let now = std::time::Instant::now();
        let due = if self.job.is_none()
            && !self.show_stale_files
            && !self.show_app_storage
            && !self.sdk.open
            && self.save_as.is_none()
            && self.inspection.is_none()
            && self.confirmation.is_none()
            && ui.ctx().input(|i| i.focused)
        {
            let deadline = self.next_presence_check.get_or_insert(now);
            let due = now >= *deadline;
            if due {
                *deadline = now + std::time::Duration::from_secs(10);
            }
            ui.ctx()
                .request_repaint_after(deadline.saturating_duration_since(now));
            due
        } else {
            false
        };
        let mut missing = BTreeSet::new();
        let mut actions = InspectorActions::default();
        let body_height = (ui.available_height() - 72.).max(160.);
        let wide = ui.available_width() >= 680.;
        let inspector_width = if wide { 280. } else { ui.available_width() };
        let table_width = if wide {
            ui.available_width() - inspector_width - 20.
        } else {
            ui.available_width()
        };
        let table_height = if wide || focused.is_none() {
            body_height
        } else {
            (body_height * 0.55).max(100.)
        };
        let mut table = |ui: &mut egui::Ui, focused: &mut Option<PathBuf>| {
            ui.set_min_size(egui::vec2(table_width, table_height));
            ui.set_max_width(table_width);
            candidate_header(ui, japanese);
            divider(ui);
            if visible.is_empty() {
                ui.add_space(14.);
                ui.label(text(
                    japanese,
                    "条件に合う掃除候補はありません。",
                    "No candidates match this search.",
                ));
            }
            ui.spacing_mut().item_spacing.y = 0.;
            egui::ScrollArea::vertical()
                .id_salt((&project.path, "candidates"))
                .max_height((table_height - 34.).max(60.))
                .show_rows(ui, 52., visible.len(), |ui, range| {
                    for &i in &visible[range] {
                        let c = &scan.files[i];
                        let path = &c.row.path;
                        if due
                            && std::fs::symlink_metadata(path)
                                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                        {
                            missing.insert(path.clone());
                        }
                        let active = focused.as_ref() == Some(path) || self.marked.contains(path);
                        let (mut row, response) = virtual_row(ui, path, 52., active);
                        let mut clicked = response.clicked();
                        let width = row.available_width();
                        let status_width = if width >= 460. { 92. } else { 64. };
                        let name_width = (width
                            - super::controls::HEIGHT
                            - 82.
                            - status_width
                            - 3. * row.spacing().item_spacing.x)
                            .max(40.);
                        row.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(super::controls::HEIGHT, 42.),
                                egui::Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_width(super::controls::HEIGHT);
                                    if c.blocked_reason.is_some() {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(17., 17.),
                                            egui::Sense::hover(),
                                        );
                                        super::icons::paint(
                                            ui,
                                            rect,
                                            super::icons::Icon::Lock,
                                            MUTED,
                                        );
                                    } else {
                                        let mut checked = self.marked.contains(path);
                                        clicked |= ui
                                            .add_enabled(
                                                self.job.is_none(),
                                                egui::Checkbox::without_text(&mut checked),
                                            )
                                            .on_hover_text(text(
                                                japanese,
                                                "この候補を選択",
                                                "Select this candidate",
                                            ))
                                            .changed();
                                    }
                                },
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(name_width, 42.),
                                egui::Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(name_width);
                                    ui.spacing_mut().item_spacing.y = 2.;
                                    ui.spacing_mut().interact_size.y = 21.;
                                    ui.spacing_mut().button_padding.y = 0.;
                                    let relative = path
                                        .strip_prefix(&project.path)
                                        .ok()
                                        .filter(|p| !p.as_os_str().is_empty())
                                        .unwrap_or(path);
                                    clicked |= ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(display(relative)).size(15.0),
                                            )
                                            .selectable(false)
                                            .sense(egui::Sense::click())
                                            .truncate(),
                                        )
                                        .on_hover_text(display(path))
                                        .clicked();
                                    let reason = c.blocked_reason.as_ref().map_or_else(
                                        || effect(c.producer, japanese),
                                        |reason| reason.text(japanese),
                                    );
                                    super::controls::label(
                                        ui,
                                        reason,
                                        12.,
                                        MUTED,
                                        Align::Min,
                                        egui::Sense::hover(),
                                    );
                                },
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(82., 42.),
                                egui::Layout::right_to_left(Align::Center),
                                |ui| {
                                    ui.set_width(82.);
                                    super::controls::label(
                                        ui,
                                        &bytes(c.row.usage.size()),
                                        15.,
                                        super::INK,
                                        Align::Max,
                                        egui::Sense::hover(),
                                    );
                                },
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(status_width, 42.),
                                egui::Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_width(status_width);
                                    let (label, color) = if c.blocked_reason.is_some() {
                                        (text(japanese, "保留", "On hold"), MUTED)
                                    } else {
                                        (text(japanese, "掃除候補", "Candidate"), ACCENT)
                                    };
                                    super::controls::label(
                                        ui,
                                        label,
                                        12.,
                                        color,
                                        Align::Min,
                                        egui::Sense::hover(),
                                    );
                                },
                            );
                        });
                        if clicked {
                            *focused = Some(path.clone());
                            if self.job.is_none()
                                && c.blocked_reason.is_none()
                                && !self.marked.remove(path)
                            {
                                self.marked.retain(|other| {
                                    !other.starts_with(path) && !path.starts_with(other)
                                });
                                self.marked.insert(path.clone());
                            }
                        }
                    }
                });
        };
        if wide {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(table_width, body_height),
                    egui::Layout::top_down(Align::Min),
                    |ui| table(ui, &mut focused),
                );
                ui.add_space(4.);
                let line = ui.cursor().left_top();
                ui.painter().vline(
                    line.x,
                    line.y..=line.y + body_height,
                    Stroke::new(1_f32, LINE),
                );
                ui.add_space(4.);
                ui.allocate_ui_with_layout(
                    egui::vec2(inspector_width, body_height),
                    egui::Layout::top_down(Align::Min),
                    |ui| {
                        if let Some(c) = focused.as_ref().and_then(|path| {
                            project
                                .files
                                .iter()
                                .map(|&i| &scan.files[i])
                                .find(|c| c.row.path == *path)
                        }) {
                            candidate_inspector(
                                ui,
                                c,
                                project,
                                japanese,
                                body_height,
                                scan.kept.as_ref(),
                                &mut actions,
                            );
                        }
                    },
                );
            });
        } else {
            ui.allocate_ui_with_layout(
                egui::vec2(table_width, table_height),
                egui::Layout::top_down(Align::Min),
                |ui| table(ui, &mut focused),
            );
            if let Some(c) = focused.as_ref().and_then(|path| {
                project
                    .files
                    .iter()
                    .map(|&i| &scan.files[i])
                    .find(|c| c.row.path == *path)
            }) {
                divider(ui);
                candidate_inspector(
                    ui,
                    c,
                    project,
                    japanese,
                    (body_height - table_height).max(100.),
                    scan.kept.as_ref(),
                    &mut actions,
                );
            }
        }
        self.projects.focused = focused;
        ui.add_space(8.);
        divider(ui);
        ui.add_space(8.);
        let estimate = scan
            .files
            .iter()
            .filter(|c| self.marked.contains(&c.row.path))
            .fold(0u64, |sum, c| sum.saturating_add(c.row.usage.size()));
        let mut review = false;
        ui.horizontal(|ui| {
            ui.label(if japanese {
                format!(
                    "全プロジェクトで {} 件を選択 · 見込み {}",
                    self.marked.len(),
                    bytes(estimate)
                )
            } else {
                format!(
                    "{} selected across projects · estimated {}",
                    self.marked.len(),
                    bytes(estimate)
                )
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                review = super::controls::button(
                    ui,
                    text(japanese, "選択した項目を確認", "Review selected items"),
                    true,
                    self.job.is_none() && !self.marked.is_empty(),
                )
                .clicked();
            });
        });
        if let Some(error) = actions.finder_error {
            self.status = error;
        }
        if !missing.is_empty() {
            let scan = self.scan.as_mut().unwrap();
            scan.files.retain(|c| !missing.contains(&c.row.path));
            self.marked.retain(|p| !missing.contains(p));
            self.projects.refresh(scan);
            ui.ctx().request_repaint();
            return;
        }
        if self.job.is_some() {
            return;
        }
        if let Some((path, keep)) = actions.keep {
            match super::keep::KeepRules::update(&path, keep) {
                Ok(()) => self.refresh(ui.ctx(), false),
                Err(error) => self.status = super::display_error(&error, japanese),
            }
        } else if let Some(path) = actions.inspect {
            let policy = scan.policy.clone();
            self.spawn(
                ui.ctx(),
                text(japanese, "候補を再確認中…", "Inspecting candidate…"),
                false,
                move || {
                    let detail = match policy.prepare(&path) {
                        Ok(p) => format!(
                            "{} · {} {} · {}",
                            effect(p.producer, japanese),
                            p.files(),
                            text(japanese, "ファイル", "files"),
                            bytes(p.logical_bytes())
                        ),
                        Err(e) => Message::from_error(&e).text(japanese).to_owned(),
                    };
                    Outcome::Inspection(vec![format!("{} · {detail}", display(&path))])
                },
            );
        } else if review {
            let policy = scan.policy.clone();
            let paths = self.marked.iter().cloned().collect();
            self.review_selected(ui.ctx(), policy, paths);
        }
    }

    fn project_list(&mut self, ui: &mut egui::Ui) {
        let scan = self.scan.as_ref().unwrap();
        ui.label(RichText::new(name(&scan.root)).size(26.).strong());
        ui.add(
            egui::Label::new(RichText::new(display(&scan.root)).size(13.).color(MUTED)).truncate(),
        )
        .on_hover_text(display(&scan.root));
        if let Some((p, delta)) = self
            .projects
            .rows
            .iter()
            .filter_map(|p| p.delta.map(|d| (p, d)))
            .filter(|(_, d)| d.signed_bytes() > 0)
            .max_by_key(|(_, d)| d.signed_bytes())
        {
            ui.label(
                RichText::new(format!(
                    "{}　{}（{}）",
                    text(
                        self.japanese_reasons,
                        "前回から最も増えたフォルダ",
                        "Largest increase since last scan"
                    ),
                    name(&p.path),
                    delta.label()
                ))
                .color(ACCENT),
            );
        } else if scan.previous.is_none() {
            ui.label(
                RichText::new(text(
                    self.japanese_reasons,
                    "初回のスキャンです。次回から前回比を表示します。",
                    "First scan. Changes will appear from the next scan.",
                ))
                .color(MUTED),
            );
        }
        let japanese = self.japanese_reasons;
        let mut review = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong(if japanese {
                format!(
                    "掃除候補の合計 {}（{} 件）",
                    bytes(self.projects.reclaim),
                    self.projects.bulk_paths.len()
                )
            } else {
                format!(
                    "Cleanup candidates: {} ({} items)",
                    bytes(self.projects.reclaim),
                    self.projects.bulk_paths.len()
                )
            });
            if self.marked.is_empty() {
                if super::controls::button(
                    ui,
                    text(japanese, "まとめて選択", "Select all"),
                    false,
                    self.job.is_none() && !self.projects.bulk_paths.is_empty(),
                )
                .clicked()
                {
                    self.marked.clone_from(&self.projects.bulk_paths);
                }
            } else {
                if ui
                    .button(text(japanese, "選択を解除", "Clear selection"))
                    .clicked()
                {
                    self.marked.clear();
                }
                let label = if japanese {
                    format!("選択した {} 件を確認", self.marked.len())
                } else {
                    format!("Review {} selected", self.marked.len())
                };
                review = super::controls::button(
                    ui,
                    &label,
                    true,
                    self.job.is_none() && !self.marked.is_empty(),
                )
                .clicked();
            }
        });
        if review {
            let policy = scan.policy.clone();
            let paths = self.marked.iter().cloned().collect();
            self.review_selected(ui.ctx(), policy, paths);
            return;
        }
        ui.add_space(18.);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(text(self.japanese_reasons, "プロジェクト", "Projects"))
                    .size(20.)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let mut by_candidate = self.projects.by_candidate;
                egui::ComboBox::from_id_salt("project-sort")
                    .selected_text(if by_candidate {
                        text(self.japanese_reasons, "候補容量が多い順", "Candidate size")
                    } else {
                        text(self.japanese_reasons, "使用量が多い順", "Used space")
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut by_candidate,
                            false,
                            text(self.japanese_reasons, "使用量が多い順", "Used space"),
                        );
                        ui.selectable_value(
                            &mut by_candidate,
                            true,
                            text(self.japanese_reasons, "候補容量が多い順", "Candidate size"),
                        );
                    });
                if by_candidate != self.projects.by_candidate {
                    self.projects.by_candidate = by_candidate;
                    self.projects.sort();
                }
            });
        });
        ui.label(
            RichText::new(if self.japanese_reasons {
                format!("直下のフォルダ {} 件", self.projects.rows.len())
            } else {
                format!("{} immediate folders", self.projects.rows.len())
            })
            .size(14.)
            .color(MUTED),
        );
        if self
            .projects
            .rows
            .iter()
            .any(|p| p.delta.is_some_and(Delta::is_partial))
        {
            ui.small(text(
                japanese,
                "* は読み取れなかった項目を含む概算です。",
                "* includes a scan with unreadable entries.",
            ));
        }
        ui.add_space(4.);
        divider(ui);
        let available = (ui.available_width()
            - ui.spacing().scroll.allocated_width()
            - 16.
            - 4. * ui.spacing().item_spacing.x)
            .max(1.);
        let column = (available * 0.20).clamp(1., 106.);
        let delta_width = (available * 0.22).clamp(1., 132.);
        let widths = [
            available - 2. * column - delta_width,
            column,
            column,
            delta_width,
        ];
        ui.horizontal(|ui| {
            ui.add_space(6.);
            for (i, label) in [
                text(self.japanese_reasons, "プロジェクト名", "Project"),
                text(self.japanese_reasons, "使用量", "Used"),
                text(self.japanese_reasons, "候補容量", "Candidates"),
                text(self.japanese_reasons, "前回比", "Change"),
            ]
            .iter()
            .enumerate()
            {
                let align = if i == 0 { Align::Min } else { Align::Max };
                ui.allocate_ui_with_layout(
                    egui::vec2(widths[i], 26.),
                    egui::Layout::top_down(align),
                    |ui| {
                        ui.set_width(widths[i]);
                        ui.label(RichText::new(*label).color(MUTED).size(14.));
                    },
                );
            }
        });
        if self.projects.rows.is_empty() {
            ui.label(text(
                self.japanese_reasons,
                "直下にプロジェクトフォルダはありません。",
                "No project folders directly inside this workspace.",
            ));
        }
        egui::ScrollArea::vertical()
            .id_salt(("projects", &self.projects.root))
            .show_rows(ui, 52., self.projects.rows.len(), |ui, range| {
                for p in &self.projects.rows[range] {
                    let (mut row, response) = virtual_row(ui, &p.path, 52., false);
                    if response.clicked() {
                        self.projects.selected = Some(p.path.clone());
                        self.projects.focused = None;
                        self.projects.query.clear();
                    }
                    row.horizontal(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(widths[0], 32.),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_width(widths[0]);
                                let (icon, _) = ui.allocate_exact_size(
                                    egui::vec2(18.0, 18.0),
                                    egui::Sense::hover(),
                                );
                                super::icons::paint(ui, icon, super::icons::Icon::Folder, ACCENT);
                                let link = egui::Label::new(
                                    RichText::new(format!("{} ›", name(&p.path)))
                                        .size(15.0)
                                        .color(ACCENT),
                                )
                                .selectable(false)
                                .sense(egui::Sense::click())
                                .truncate();
                                if ui.add(link).on_hover_text(display(&p.path)).clicked() {
                                    self.projects.selected = Some(p.path.clone());
                                    self.projects.focused = None;
                                    self.projects.query.clear();
                                }
                            },
                        );
                        for (i, label) in [
                            bytes(p.size),
                            bytes(p.reclaim),
                            p.delta.map_or_else(|| "—".into(), |d| d.label()),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let value = RichText::new(label);
                            let value = if i == 0 {
                                value.strong()
                            } else if i == 2 {
                                value.color(MUTED)
                            } else {
                                value.color(ACCENT)
                            };
                            ui.allocate_ui_with_layout(
                                egui::vec2(widths[i + 1], 32.),
                                egui::Layout::right_to_left(Align::Center),
                                |ui| {
                                    ui.set_width(widths[i + 1]);
                                    ui.add(egui::Label::new(value.clone()).truncate())
                                        .on_hover_text(value);
                                },
                            );
                        }
                    });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{
        artifacts::{FileCandidate, Policy},
        inventory::{Inventory, Row, Usage},
    };
    use std::sync::Arc;

    fn sample_scan(root: &Path) -> Scan {
        let row = |path: &str, size| Row {
            path: root.join(path),
            is_dir: true,
            usage: Usage {
                logical_bytes: size,
                ..Default::default()
            },
        };
        let candidate = |path, size, blocked: bool| FileCandidate {
            row: row(path, size),
            producer: Producer::DeclaredOutput,
            blocked_reason: blocked.then(|| Message::new("kept", "保留")),
        };
        Scan {
            kept: Some(super::super::keep::KeepRules::default()),
            root: root.to_owned(),
            directories: super::super::folder_index::Directories::from_inventory(
                root,
                Inventory {
                    directories: vec![row("a", 130), row("b", 120), row("b/other", 40)],
                    matches: vec![],
                    omitted_matches: 0,
                    cargo_marker_dirs: Default::default(),
                },
            ),
            policy: Arc::new(Policy {
                roots: vec![],
                outputs: vec![],
                docker_context: None,
            }),
            previous: None,
            recorded: false,
            files: vec![
                candidate("b/other", 40, false),
                candidate("b/other/child", 20, false),
                FileCandidate {
                    producer: Producer::Cargo,
                    ..candidate("a/target", 100, true)
                },
                FileCandidate {
                    producer: Producer::CargoIncremental,
                    ..candidate("a/target/debug/incremental", 15, false)
                },
            ],
            external: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn rankings_keep_nested_and_blocked_outputs_out_of_the_estimated_total() {
        let root = PathBuf::from("/workspace");
        let scan = sample_scan(&root);
        let mut projects = Projects::from_scan(&scan);
        assert_eq!(projects.rows.len(), 2);
        assert_eq!(projects.rows[0].path, root.join("a"));
        assert_eq!(projects.rows[0].reclaim, 15);
        assert_eq!(
            projects.rows[0].reviewable_cargo_parts[&root.join("a/target")],
            1
        );
        assert_eq!(projects.reclaim, 55);
        assert_eq!(
            projects.bulk_paths,
            BTreeSet::from([
                root.join("b/other"),
                root.join("a/target/debug/incremental")
            ])
        );
        projects.selected = Some(root.join("a"));
        projects.query = "incremental".into();
        projects.refresh(&scan);
        assert_eq!(projects.selected, Some(root.join("a")));
        assert_eq!(projects.query, "incremental");
        projects.by_candidate = true;
        projects.sort();
        assert_eq!(projects.rows[0].path, root.join("b"));
        assert_eq!(projects.rows[0].reclaim, 40);
        assert_eq!(projects.rows[0].files.len(), 2);
    }
    #[test]
    fn clicking_a_name_or_size_toggles_the_candidate_but_a_held_row_only_focuses() {
        let root = Path::new("/workspace");
        let scan = sample_scan(root);
        let mut projects = Projects::from_scan(&scan);
        projects.open_project(root.join("a"));
        let mut app = App {
            projects,
            scan: Some(scan),
            next_presence_check: Some(
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            ),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let render = |app: &mut App, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.project_screen(ui),
            )
        };
        let click = |app: &mut App, label: &str| {
            let frame = render(app, Vec::new());
            let pos = frame
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) if t.galley.text() == label => {
                        Some(t.pos + t.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing row label: {label}"));
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render(app, vec![egui::Event::PointerMoved(pos), button(true)]);
            render(app, vec![button(false)]);
        };
        let nested = root.join("a/target/debug/incremental");
        click(&mut app, "target/debug/incremental");
        assert_eq!(app.marked, BTreeSet::from([nested.clone()]));
        click(&mut app, "100 B");
        assert_eq!(app.projects.focused, Some(root.join("a/target")));
        assert_eq!(app.marked, BTreeSet::from([nested]));
        click(&mut app, "15 B");
        assert!(app.marked.is_empty());
    }
}
