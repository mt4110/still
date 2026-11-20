use super::{display, egui, text, App, Outcome};
use crate::review::history::{self, HistoryPoint};
use anyhow::Result;
use std::path::PathBuf;

const DISPLAY_LIMIT: usize = 100;

#[derive(Default)]
pub(super) struct Panel {
    pub(super) review: Option<Result<(PathBuf, Vec<HistoryPoint>)>>,
}

impl App {
    pub(super) fn history_menu(&mut self, ui: &mut egui::Ui) {
        let Some(scan) = &self.scan else { return };
        let root = scan.root.clone();
        let profile = self.profile_name.clone();
        if ui
            .add_enabled(
                self.job.is_none(),
                egui::Button::new(text(
                    self.japanese_reasons,
                    "この場所の容量履歴…",
                    "This workspace's size history…",
                )),
            )
            .clicked()
        {
            ui.close();
            self.spawn(
                ui.ctx(),
                text(
                    self.japanese_reasons,
                    "履歴を読み込み中…",
                    "Loading scan history…",
                ),
                false,
                move || {
                    Outcome::HistoryLoad(
                        history::load_timeline(&root, profile.as_deref(), DISPLAY_LIMIT)
                            .map(|entries| (root, entries)),
                    )
                },
            );
        }
    }

    pub(super) fn history_dialog(&mut self, ctx: &egui::Context) {
        let ja = self.japanese_reasons;
        let Some(review) = &self.timeline.review else {
            return;
        };
        let mut close = false;
        let modal = egui::Modal::new("workspace-history".into()).show(ctx, |ui| {
            ui.heading(text(ja, "この場所の容量履歴", "Workspace size history"));
            match review {
                Ok((root, points)) => {
                    ui.label(display(root));
                    if points.is_empty() {
                        ui.label(text(ja, "まだ履歴がありません。この機能の追加前の観測は含まれません。", "No history recorded yet. Observations from before this feature are not included."));
                    } else {
                        ui.small(text(ja, "新しい順に最大100回の調査を表示します。読み取れなかった項目などを含む場合は一部未走査と表示します。", "Up to 100 scans, newest first. Partial means some entries could not be inspected or were skipped."));
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            for point in points {
                                ui.separator();
                                ui.strong(point.completed_at.with_timezone(&chrono::Local).format("%Y/%m/%d %H:%M:%S").to_string());
                                ui.label(format!("{} {} · {} {}", text(ja, "論理容量", "Logical"), super::bytes(point.logical_bytes), text(ja, "割当容量", "Allocated"), point.allocated_bytes.map(super::bytes).unwrap_or_else(|| text(ja, "不明", "Unavailable").into())));
                                if point.partial { ui.small(text(ja, "一部未走査", "Partial observation")); }
                            }
                        });
                    }
                }
                Err(err) => { ui.label(text(ja, "履歴を読み込めませんでした。", "Could not load scan history.")); ui.label(format!("{err:#}")); }
            }
            close = ui.button(text(ja, "閉じる", "Close")).clicked();
        });
        if close || modal.should_close() {
            self.timeline.review = None;
        }
    }
}
