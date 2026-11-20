use super::{display, egui, text, App, Outcome, Scan};
use crate::review::filesystem::recovery::{RestoreStage, ReviewedStage};
use anyhow::Result;
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct Panel {
    locations: Option<Vec<(PathBuf, PathBuf)>>,
    pub(super) review: Option<Result<ReviewedStage>>,
    pub(super) restore: Option<Result<RestoreStage>>,
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
    pub(super) fn open_stage_list(&mut self) {
        if let Some(scan) = &self.scan {
            if !scan.policy.roots.is_empty() {
                self.stages.locations = Some(locations(scan));
            }
        }
    }

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
            self.open_stage_list();
            ui.close();
        }
    }

    pub(super) fn stage_dialog(&mut self, ctx: &egui::Context) {
        let ja = self.japanese_reasons;
        if let Some(paths) = &self.stages.locations {
            let mut selected = None;
            let mut close = false;
            let modal = egui::Modal::new("stage-list".into()).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(200.0, 720.0));
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
                        "今回のスキャンで見つかった場所です。確認するステージを選んでください。",
                        "These stages were found in the last scan. Choose one to inspect.",
                    ));
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (root, path) in paths {
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(display(path)).wrap());
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button(text(ja, "後片付け", "Clean up")).clicked()
                                        {
                                            selected = Some((root.clone(), path.clone(), false));
                                        }
                                        if ui
                                            .button(text(ja, "復元を確認", "Review restore"))
                                            .clicked()
                                        {
                                            selected = Some((root.clone(), path.clone(), true));
                                        }
                                    });
                                    ui.separator();
                                });
                            }
                        });
                }
                close = ui.button(text(ja, "閉じる", "Close")).clicked();
            });
            if close || modal.should_close() || selected.is_some() {
                self.stages.locations = None;
            }
            if let Some((root, path, restore)) = selected {
                self.spawn(
                    ctx,
                    text(ja, "残存ファイルを確認中…", "Inspecting stage…"),
                    false,
                    move || {
                        if restore {
                            Outcome::StageRestoreReview(RestoreStage::inspect(&root, &path))
                        } else {
                            Outcome::StageReview(ReviewedStage::inspect(&root, &path))
                        }
                    },
                );
            }
        }
        if let Some(review) = &self.stages.restore {
            let mut restore = false;
            let mut close = false;
            let mut finder_error = None;
            let modal = egui::Modal::new("stage-restore".into()).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(200.0, 720.0));
                ui.heading(text(ja, "残存データの復元", "Restore remaining data"));
                match review {
                    Ok(stage) => {
                        ui.label(format!("{}: {}", text(ja, "移動元", "From"), display(&stage.path.join("payload"))));
                        ui.label(format!("{}: {}", text(ja, "記録された移動先", "Recorded destination"), display(&stage.original)));
                        ui.label(text(ja, "記録は由来や完全性の証明ではありません。整理中に一部が削除済みの可能性があります。内容と移動先を確認してください。既存の移動先は上書きしません。", "The record does not prove origin or completeness; some data may already have been removed. Check the contents and destination. An existing destination will not be overwritten."));
                        if ui.button(text(ja, "残存データを Finder で開く", "Open remaining data in Finder")).clicked() {
                            finder_error = super::open_in_finder(&stage.path.join("payload")).err();
                        }
                        restore = ui.button(text(ja, "残存データをこの場所に復元", "Restore remaining data here")).clicked();
                    }
                    Err(err) => { ui.label(format!("{err:#}")); }
                }
                close = ui.button(text(ja, "閉じる", "Close")).clicked();
            });
            if let Some(error) = finder_error {
                self.status = format!("{error:#}");
            }
            if restore {
                if let Some(Ok(review)) = self.stages.restore.take() {
                    self.spawn(
                        ctx,
                        text(ja, "復元中…", "Restoring…"),
                        true,
                        move || Outcome::StageRestore(review.restore()),
                    );
                }
            } else if close || modal.should_close() {
                self.stages.restore = None;
            }
        }
        if let Some(review) = &self.stages.review {
            let mut remove = false;
            let mut close = false;
            let modal = egui::Modal::new("stage-review".into()).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(200.0, 720.0));
                ui.heading(text(ja, "ステージの後片付け", "Stage cleanup"));
                match review {
                    Ok(stage) => {
                        ui.label(display(&stage.path));
                        ui.label(text(ja, "空のフォルダ、Finder のメタデータ、整理時の記録だけが残っています。このステージを完全に削除します。", "This stage contains only empty folders, Finder metadata, or a staging note. Removing it is permanent."));
                        remove = ui.button(text(ja, "このステージを削除", "Remove this stage")).clicked();
                    }
                    Err(err) => {
                        ui.label(text(ja, "後片付けを保留しました。", "Stage cleanup is on hold."));
                        ui.label(text(ja, "ステージに残った内容を安全に片付けられるか確認できませんでした。", "Could not verify that the remaining contents are safe to clean up."));
                        ui.collapsing(text(ja, "詳細", "Details"), |ui| { ui.label(format!("{err:#}")); });
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
