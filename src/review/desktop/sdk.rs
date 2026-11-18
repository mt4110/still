use super::super::sdk_cleanup::{self, Item, Receipt, Removal, Report, Review, Target};
use super::{bytes, App, Outcome};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    pub report: Option<Result<Report>>,
    pub receipt: Option<Result<Receipt>>,
    marked: BTreeSet<Target>,
    confirmation: Option<Review>,
    error: String,
}

impl App {
    pub(super) fn scan_sdks(&mut self, ctx: &egui::Context) {
        self.sdk.marked.clear();
        self.sdk.report = None;
        self.sdk.receipt = None;
        self.sdk.error.clear();
        let roots = self.profile.roots.clone();
        self.spawn(
            ctx,
            "Checking installed SDK versions…",
            false,
            move || Outcome::SdkScan(sdk_cleanup::scan(&roots)),
        );
    }

    pub(super) fn sdk_dialog(&mut self, ctx: &egui::Context) {
        let mut refresh = false;
        let mut close = false;
        let mut preview = false;
        let mut cancel = false;
        let mut delete = false;
        let busy = self.job.as_ref();
        let state = &mut self.sdk;
        let dialog = egui::Modal::new(egui::Id::new("sdk-storage")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 870.0));
            ui.heading("Xcode & Android NDK");
            if let Some(job) = busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(job.label);
                });
                return;
            }
            if let Some(review) = &state.confirmation {
                ui.label(RichText::new("Uninstall these versions?").strong());
                ui.label("Tests or builds that use them will need the exact versions installed again.");
                egui::ScrollArea::vertical().max_height(285.0).show(ui, |ui| {
                    for item in &review.items {
                        ui.label(RichText::new(item_title(item)).strong());
                        ui.label(item.reason.text(self.japanese_reasons));
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    cancel = ui.button("Back").clicked();
                    delete = ui.add(
                        egui::Button::new(format!("Uninstall {} selected", review.items.len()))
                            .fill(Color32::from_rgb(243, 186, 176)),
                    ).clicked();
                });
                return;
            }
            if let Some(result) = &state.receipt {
                show_receipt(ui, result, self.japanese_reasons);
            } else {
                ui.label("Checks installed versions and local test or build settings under ~/_workspace, ~/AndroidStudioProjects, and your open profile.");
                ui.label("A candidate is based on the use records available here. Check old test or build requirements before selecting.");
                match &state.report {
                    Some(Ok(report)) => {
                        let candidates = report.items.iter().filter(|item| item.selectable).count();
                        let residuals = report.items.iter().filter(|item| matches!(item.target, Target::ResidualRuntime { .. })).count();
                        let protected = report.items.len() - candidates - residuals;
                        ui.label(format!("{candidates} candidates · {protected} protected"));
                        if residuals > 0 {
                            ui.label(if self.japanese_reasons {
                                format!("一部残存 {residuals} 件")
                            } else {
                                format!("{residuals} partially remaining")
                            });
                        }
                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            for item in &report.items {
                                let residual = matches!(item.target, Target::ResidualRuntime { .. });
                                let title = if residual {
                                    format!("{} · {}", item.label, partial_with_size(item.size, self.japanese_reasons))
                                } else {
                                    item_title(item)
                                };
                                let mut checked = state.marked.contains(&item.target);
                                if ui.add_enabled(item.selectable, egui::Checkbox::new(&mut checked, title)).changed() {
                                    if checked {
                                        state.marked.insert(item.target.clone());
                                    } else {
                                        state.marked.remove(&item.target);
                                    }
                                }
                                let reason = item.reason.text(self.japanese_reasons);
                                if residual {
                                    ui.label(reason);
                                } else {
                                    let status = match (item.selectable, self.japanese_reasons) {
                                        (true, false) => "Candidate",
                                        (false, false) => "Protected",
                                        (true, true) => "候補",
                                        (false, true) => "保護",
                                    };
                                    ui.label(format!("{status} · {reason}"));
                                }
                                ui.separator();
                            }
                            for note in &report.notes {
                                ui.label(note);
                            }
                        });
                    }
                    Some(Err(error)) => { ui.label(format!("{error:#}")); }
                    None => {}
                }
            }
            if !state.error.is_empty() {
                ui.label(&state.error);
            }
            ui.horizontal(|ui| {
                close = ui.button("Close").clicked();
                refresh = ui.button("Scan again").clicked();
                if !state.marked.is_empty() {
                    preview = ui.button(format!("Review {} selected…", state.marked.len())).clicked();
                }
            });
        });
        if busy.is_some() {
            return;
        }
        if state.confirmation.is_some() {
            if cancel || dialog.should_close() {
                state.confirmation = None;
            } else if delete {
                let review = state.confirmation.take().unwrap();
                state.report = None;
                state.marked.clear();
                self.spawn(
                    ctx,
                    "Uninstalling confirmed SDK versions…",
                    true,
                    move || Outcome::SdkRemoval(review.apply()),
                );
            }
        } else if close || dialog.should_close() {
            state.open = false;
        } else if refresh {
            self.scan_sdks(ctx);
        } else if preview {
            if let Some(Ok(report)) = &state.report {
                match Review::new(report, &state.marked, self.profile.roots.clone()) {
                    Ok(review) => state.confirmation = Some(review),
                    Err(error) => state.error = format!("{error:#}"),
                }
            }
        }
    }
}

fn item_title(item: &Item) -> String {
    match item.size {
        Some(size) => format!("{} · {} {}", item.label, bytes(size), item.basis),
        None => item.label.clone(),
    }
}

fn partial_label(japanese: bool) -> &'static str {
    if japanese {
        "一部残存"
    } else {
        "Partially remaining"
    }
}

fn partial_with_size(image_bytes: Option<u64>, japanese: bool) -> String {
    let status = partial_label(japanese);
    match image_bytes {
        Some(size) => format!("{status} {}", bytes(size)),
        None => format!(
            "{status} · {}",
            if japanese {
                "容量未確認"
            } else {
                "size unknown"
            }
        ),
    }
}

fn show_receipt(ui: &mut egui::Ui, result: &Result<Receipt>, japanese: bool) {
    match result {
        Err(error) => {
            ui.label(format!("{error:#}"));
        }
        Ok(receipt) => {
            let (heading, before_label, after_label, measured_label) = if japanese {
                ("Mac データボリュームの空き容量", "削除前", "削除後", "実測")
            } else {
                (
                    "Mac data volume free space",
                    "Before",
                    "After",
                    "Measured change",
                )
            };
            ui.label(RichText::new(heading).strong());
            ui.label(format!("{before_label} · {}", bytes(receipt.before)));
            match &receipt.after {
                Ok(after) => {
                    let sign = if *after >= receipt.before { "+" } else { "−" };
                    ui.label(format!("{after_label} · {}", bytes(*after)));
                    ui.label(
                        RichText::new(format!(
                            "{measured_label} {sign}{}",
                            bytes(after.abs_diff(receipt.before))
                        ))
                        .strong(),
                    );
                }
                Err(error) => {
                    ui.label(format!(
                        "Could not measure free space after cleanup · {error:#}"
                    ));
                }
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .show(ui, |ui| {
                    for (label, result) in &receipt.results {
                        let detail = match result {
                            Ok(Removal::Removed) => {
                                if japanese { "削除済み" } else { "Removed" }.into()
                            }
                            Ok(Removal::Partial { image_bytes }) => {
                                let status = partial_with_size(*image_bytes, japanese);
                                if image_bytes.is_some() {
                                    let note = if japanese {
                                        "確認できた残存イメージ"
                                    } else {
                                        "Remaining images observed"
                                    };
                                    format!("{status} · {note}")
                                } else {
                                    status
                                }
                            }
                            Ok(Removal::Unverified) => if japanese {
                                "削除結果を確認できませんでした"
                            } else {
                                "Could not verify complete removal"
                            }
                            .into(),
                            Err(error) => format!("{error:#}"),
                        };
                        ui.label(format!("{label} · {detail}"));
                    }
                });
        }
    }
}
