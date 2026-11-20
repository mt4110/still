use super::{display, egui, text, App, Outcome, Scan};
use crate::review::filesystem::recovery::ReviewedStage;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct Panel {
    locations: Option<Vec<(PathBuf, PathBuf)>>,
    pub(super) review: Option<Result<ReviewedStage>>,
}

fn locations(scan: &Scan) -> Vec<(PathBuf, PathBuf)> {
    scan.directories
        .stage_paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.strip_prefix(".still-stage-")?;
            let id = uuid::Uuid::parse_str(name).ok()?;
            if id.get_version_num() != 4 || id.to_string() != name {
                return None;
            }
            let root = scan
                .policy
                .roots
                .iter()
                .filter(|root| path.starts_with(root) && path != *root)
                .max_by_key(|root| root.components().count())?;
            Some((root.clone(), path.clone()))
        })
        .collect()
}

impl App {
    pub(super) fn stage_menu(&mut self, ui: &mut egui::Ui) {
        let Some(scan) = &self.scan else { return };
        if scan.policy.roots.is_empty() {
            return;
        }
        if ui
            .button(text(
                self.japanese_reasons,
                "残ったステージを確認…",
                "Review leftover stages…",
            ))
            .clicked()
        {
            self.stages.locations = Some(locations(scan));
            ui.close();
        }
    }

    pub(super) fn stage_dialog(&mut self, ctx: &egui::Context) {
        let ja = self.japanese_reasons;
        if let Some(paths) = &self.stages.locations {
            let mut selected = None;
            let mut close = false;
            let modal = egui::Modal::new("stage-list".into()).show(ctx, |ui| {
                ui.heading(text(ja, "残ったステージ", "Leftover stages"));
                if paths.is_empty() {
                    ui.label(text(
                        ja,
                        "今回のスキャンではステージが見つかりませんでした。",
                        "This scan found no leftover stages.",
                    ));
                } else {
                    ui.label(text(
                        ja,
                        "確認する場所を選んでください。",
                        "Choose a stage to inspect.",
                    ));
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (root, path) in paths {
                                if ui.button(display(path)).clicked() {
                                    selected = Some((root.clone(), path.clone()));
                                }
                            }
                        });
                }
                close = ui.button(text(ja, "閉じる", "Close")).clicked();
            });
            if close || modal.should_close() || selected.is_some() {
                self.stages.locations = None;
            }
            if let Some((root, path)) = selected {
                self.spawn(
                    ctx,
                    text(ja, "残存ファイルを確認中…", "Inspecting stage…"),
                    false,
                    move || Outcome::StageReview(ReviewedStage::inspect(&root, &path)),
                );
            }
        }
        if let Some(review) = &self.stages.review {
            let mut remove = false;
            let mut close = false;
            let modal = egui::Modal::new("stage-review".into()).show(ctx, |ui| {
                ui.heading(text(ja, "ステージの後片付け", "Stage cleanup"));
                match review {
                    Ok(stage) => {
                        ui.label(display(&stage.path));
                        ui.label(text(ja, "空のフォルダ、Finder のメタデータ、整理時の記録だけが残っています。このステージを完全に削除します。", "This stage contains only empty folders, Finder metadata, or a staging note. Removing it is permanent."));
                        remove = ui.button(text(ja, "このステージを削除", "Remove this stage")).clicked();
                    }
                    Err(err) => {
                        ui.label(text(ja, "後片付けを保留しました。", "Stage cleanup is on hold."));
                        ui.label(format!("{err:#}"));
                    }
                }
                close = ui.button(text(ja, "閉じる", "Close")).clicked();
            });
            if remove {
                if let Some(Ok(review)) = self.stages.review.take() {
                    self.spawn(
                        ctx,
                        text(ja, "ステージを削除中…", "Removing stage…"),
                        true,
                        move || Outcome::StageRemoval(review.remove()),
                    );
                }
            } else if close || modal.should_close() {
                self.stages.review = None;
            }
        }
    }
}
