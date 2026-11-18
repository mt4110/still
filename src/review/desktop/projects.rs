use super::super::{artifacts::Producer, history::Delta};
use super::{bytes, display, egui, open_in_finder, App, Confirmation, Message, Outcome, Scan};
use super::{ACCENT, LINE, MUTED, PAPER};
use egui::{Align, RichText, Stroke};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn divider(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1_f32, LINE));
}

fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(PAPER))
        .fill(ACCENT)
        .corner_radius(6.)
        .min_size(egui::vec2(0., 34.))
}

#[derive(Default)]
pub(super) struct Projects {
    rows: Vec<Project>,
    selected: Option<PathBuf>,
    by_candidate: bool,
    pub(super) active: bool,
}

struct Project {
    path: PathBuf,
    size: u64,
    reclaim: u64,
    delta: Option<Delta>,
    files: Vec<usize>,
}

fn name(path: &Path) -> String {
    display(path.file_name().map(Path::new).unwrap_or(path))
}

fn effect(p: Producer) -> &'static str {
    match p {
        Producer::Cargo | Producer::CargoRelocated => {
            "Cargo のビルド出力。次回のビルドで作り直します。"
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

impl Projects {
    pub(super) fn from_scan(scan: &Scan) -> Self {
        let mut projects: BTreeMap<_, _> = scan
            .inventory
            .directories
            .iter()
            .filter(|r| r.path.parent() == Some(scan.root.as_path()))
            .map(|r| {
                (
                    r.path.clone(),
                    Project {
                        path: r.path.clone(),
                        size: r.usage.size(),
                        reclaim: 0,
                        delta: scan.delta(r),
                        files: vec![],
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
                    && !path.ancestors().skip(1).any(|p| eligible.contains(p))
                {
                    project.reclaim = project.reclaim.saturating_add(candidate.row.usage.size());
                }
            }
        }
        let mut result = Self {
            rows: projects.into_values().collect(),
            active: true,
            ..Default::default()
        };
        result.sort();
        result
    }
    fn sort(&mut self) {
        let value = |p: &Project| if self.by_candidate { p.reclaim } else { p.size };
        self.rows
            .sort_by(|a, b| value(b).cmp(&value(a)).then_with(|| a.path.cmp(&b.path)));
    }
}

impl App {
    pub(super) fn project_screen(&mut self, ui: &mut egui::Ui) {
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(24, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
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
            });
    }

    fn project_detail(&mut self, ui: &mut egui::Ui, index: usize) {
        let scan = self.scan.as_ref().unwrap();
        let project = &self.projects.rows[index];
        if ui
            .link(format!("‹ {} の一覧へ", name(&scan.root)))
            .clicked()
        {
            self.projects.selected = None;
            self.marked.clear();
            return;
        }
        ui.add_space(6.);
        ui.label(RichText::new(name(&project.path)).size(33.).strong());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("使用量  {}", bytes(project.size))).color(MUTED));
            ui.label(
                RichText::new(format!("候補容量  {}", bytes(project.reclaim)))
                    .color(ACCENT)
                    .strong(),
            );
        });
        ui.add_space(14.);
        ui.label(RichText::new("掃除候補と保留理由").size(22.).strong());
        divider(ui);
        if project.files.is_empty() {
            ui.label("このフォルダで確認できた生成物候補はありません。");
        }
        let mut inspect = None;
        let mut missing = BTreeSet::new();
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
        egui::ScrollArea::vertical()
            .id_salt(&project.path)
            .max_height((ui.available_height() - 76.).max(120.))
            .show_rows(ui, 113., project.files.len(), |ui, range| {
                for i in range {
                    let c = &scan.files[project.files[i]];
                    let path = &c.row.path;
                    if due
                        && std::fs::symlink_metadata(path)
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        missing.insert(path.clone());
                    }
                    ui.horizontal(|ui| {
                        let mut selected = self.marked.contains(path);
                        if ui
                            .add_enabled(
                                c.blocked_reason.is_none(),
                                egui::Checkbox::without_text(&mut selected),
                            )
                            .changed()
                        {
                            if selected {
                                self.marked.retain(|other| {
                                    !other.starts_with(path) && !path.starts_with(other)
                                });
                                self.marked.insert(path.clone());
                            } else {
                                self.marked.remove(path);
                            }
                        }
                        let relative = path
                            .strip_prefix(&project.path)
                            .ok()
                            .filter(|p| !p.as_os_str().is_empty())
                            .unwrap_or(path);
                        ui.strong(bytes(c.row.usage.size()));
                        ui.add(
                            egui::Label::new(RichText::new(display(relative)).size(18.).strong())
                                .truncate(),
                        )
                        .on_hover_text(display(path));
                    });
                    let reason = match &c.blocked_reason {
                        Some(reason) => {
                            format!("保留 · {}", reason.text(self.japanese_reasons))
                        }
                        None if c.producer == Producer::DeclaredOutput => effect(c.producer).into(),
                        None => format!("{} Git 追跡ファイルなし。", effect(c.producer)),
                    };
                    ui.add(egui::Label::new(RichText::new(&reason).color(MUTED)).truncate())
                        .on_hover_text(&reason);
                    ui.horizontal(|ui| {
                        if ui.add(primary("Finder で開く ↗")).clicked() {
                            if let Err(err) = open_in_finder(path) {
                                self.status = err.to_string();
                            }
                        }
                        if ui.link("この項目を再確認").clicked() {
                            inspect = Some(path.clone());
                        }
                    });
                    divider(ui);
                }
            });
        let mut review = false;
        if !self.marked.is_empty() {
            let estimate = project
                .files
                .iter()
                .map(|&i| &scan.files[i])
                .filter(|c| {
                    self.marked.contains(&c.row.path)
                        && !c
                            .row
                            .path
                            .ancestors()
                            .skip(1)
                            .any(|p| self.marked.contains(p))
                })
                .fold(0u64, |sum, c| sum.saturating_add(c.row.usage.size()));
            review = ui
                .add(primary(format!(
                    "選んだ {} 件を確認 · 見込み {}",
                    self.marked.len(),
                    bytes(estimate)
                )))
                .clicked();
        }
        if !missing.is_empty() {
            let selected = project.path.clone();
            let scan = self.scan.as_mut().unwrap();
            scan.files.retain(|c| !missing.contains(&c.row.path));
            self.marked.retain(|p| !missing.contains(p));
            self.projects = Projects::from_scan(scan);
            self.projects.selected = Some(selected);
            ui.ctx().request_repaint();
            return;
        }
        if let Some(path) = inspect {
            let policy = scan.policy.clone();
            let ja = self.japanese_reasons;
            self.spawn(ui.ctx(), "候補を再確認中…", false, move || {
                let detail = match policy.prepare(&path) {
                    Ok(p) => format!(
                        "{} · {} ファイル · {}",
                        effect(p.producer),
                        p.files(),
                        bytes(p.logical_bytes())
                    ),
                    Err(e) => Message::from_error(&e).text(ja).to_owned(),
                };
                Outcome::Inspection(vec![format!("{} · {detail}", display(&path))])
            });
        } else if review {
            let policy = scan.policy.clone();
            let paths = self.marked.iter().cloned().collect();
            self.spawn(
                ui.ctx(),
                "選択した候補を確認中…",
                false,
                move || Outcome::Review(Confirmation::prepare(policy, paths)),
            );
        }
    }

    fn project_list(&mut self, ui: &mut egui::Ui) {
        let scan = self.scan.as_ref().unwrap();
        ui.label(RichText::new(name(&scan.root)).size(34.).strong());
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
                    "前回から最も増えたフォルダ　{}（{}）",
                    name(&p.path),
                    delta.label()
                ))
                .color(ACCENT),
            );
        } else if scan.previous.is_none() {
            ui.label(
                RichText::new("初回のスキャンです。次回から前回比を表示します。").color(MUTED),
            );
        }
        ui.add_space(18.);
        ui.horizontal(|ui| {
            ui.label(RichText::new("プロジェクト").size(22.).strong());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let mut by_candidate = self.projects.by_candidate;
                egui::ComboBox::from_id_salt("project-sort")
                    .selected_text(if by_candidate {
                        "候補容量が多い順"
                    } else {
                        "使用量が多い順"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut by_candidate, false, "使用量が多い順");
                        ui.selectable_value(&mut by_candidate, true, "候補容量が多い順");
                    });
                if by_candidate != self.projects.by_candidate {
                    self.projects.by_candidate = by_candidate;
                    self.projects.sort();
                }
            });
        });
        ui.label(
            RichText::new(format!("直下のフォルダ {} 件", self.projects.rows.len()))
                .size(14.)
                .color(MUTED),
        );
        ui.add_space(4.);
        divider(ui);
        let widths = [(ui.available_width() - 345.).max(140.), 105., 110., 95.];
        ui.horizontal(|ui| {
            for (i, label) in ["プロジェクト名", "使用量", "候補容量", "前回比"]
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
            ui.label("直下にプロジェクトフォルダはありません。");
        }
        egui::ScrollArea::vertical().id_salt("projects").show_rows(
            ui,
            43.,
            self.projects.rows.len(),
            |ui, range| {
                for p in &self.projects.rows[range] {
                    divider(ui);
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(widths[0], 32.),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_width(widths[0]);
                                let link = egui::Button::new(
                                    RichText::new(format!("{} ›", name(&p.path)))
                                        .color(ACCENT)
                                        .strong(),
                                )
                                .truncate()
                                .frame_when_inactive(false);
                                if ui.add(link).on_hover_text(display(&p.path)).clicked() {
                                    self.projects.selected = Some(p.path.clone());
                                    self.marked.clear();
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
                                    ui.add(egui::Label::new(value).truncate());
                                },
                            );
                        }
                    });
                }
            },
        );
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

    #[test]
    fn rankings_keep_nested_and_blocked_outputs_out_of_the_estimated_total() {
        let root = PathBuf::from("/workspace");
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
        let scan = Scan {
            root: root.clone(),
            inventory: Inventory {
                directories: vec![row("a", 130), row("b", 120), row("b/other", 40)],
                matches: vec![],
                omitted_matches: 0,
                cargo_marker_dirs: Default::default(),
            },
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
                candidate("a/kept", 100, true),
            ],
            external: vec![],
            notes: vec![],
        };
        let mut projects = Projects::from_scan(&scan);
        assert_eq!(projects.rows.len(), 2);
        assert_eq!(projects.rows[0].path, root.join("a"));
        assert_eq!(projects.rows[0].reclaim, 0);
        projects.by_candidate = true;
        projects.sort();
        assert_eq!(projects.rows[0].path, root.join("b"));
        assert_eq!(projects.rows[0].reclaim, 40);
        assert_eq!(projects.rows[0].files.len(), 2);
    }
}
