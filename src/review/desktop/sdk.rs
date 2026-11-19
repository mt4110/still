use super::super::sdk_cleanup::{self, Item, Receipt, Removal, Report, Review, Target};
use super::{bytes, App, Outcome, ACCENT, INK, LINE, MUTED};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    pub report: Option<Result<Report>>,
    pub receipt: Option<Result<Receipt>>,
    marked: BTreeSet<Target>,
    focused: Option<Target>,
    confirmation: Option<Review>,
    error: String,
}

impl App {
    pub(super) fn scan_sdks(&mut self, ctx: &egui::Context) {
        if self.job.is_some() || self.sdk.confirmation.is_some() {
            return;
        }
        self.sdk.marked.clear();
        self.sdk.focused = None;
        self.sdk.report = None;
        self.sdk.receipt = None;
        self.sdk.error.clear();
        let roots = self.profile.roots.clone();
        self.spawn(
            ctx,
            text(
                self.japanese_reasons,
                "インストール済みの SDK を確認中…",
                "Checking installed SDK versions…",
            ),
            false,
            move || Outcome::SdkScan(sdk_cleanup::scan(&roots)),
        );
    }

    pub(super) fn sdk_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.sdk.report.is_none() && self.sdk.receipt.is_none() && self.job.is_none() {
            self.scan_sdks(&ctx);
        }
        let japanese = self.japanese_reasons;
        let busy = self.job.is_some();
        let mut refresh = false;
        let mut preview = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(text(japanese, "アプリと SDK", "Apps & SDKs"));
                ui.label(
                    RichText::new(text(
                        japanese,
                        "Xcode と Android NDK",
                        "Xcode & Android NDK",
                    ))
                    .strong()
                    .color(INK),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh = ui
                    .add_enabled(
                        !busy && self.sdk.confirmation.is_none(),
                        egui::Button::new(text(japanese, "再スキャン", "Scan again")),
                    )
                    .clicked();
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(text(
                japanese,
                "版ごとの容量と利用状況を確認し、選んだ版だけをアンインストールします。",
                "Review each version's size and usage, then choose which versions to uninstall.",
            ))
            .color(MUTED),
        );
        ui.add_space(14.0);

        let state = &mut self.sdk;
        egui::Panel::bottom("sdk-actions")
            .show_separator_line(false)
            .resizable(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 12)))
            .show_inside(ui, |ui| {
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if state.receipt.is_some() {
                        ui.label(text(japanese, "操作の結果", "Operation results"));
                    } else {
                        let count = state.marked.len();
                        let total = state.report.as_ref().and_then(|result| {
                            result.as_ref().ok().and_then(|report| {
                                report
                                    .items
                                    .iter()
                                    .filter(|item| state.marked.contains(&item.target))
                                    .try_fold(0_u64, |sum, item| {
                                        Some(sum.saturating_add(item.size?))
                                    })
                            })
                        });
                        let summary = if japanese {
                            format!("{count} 件を選択")
                        } else {
                            format!("{count} selected")
                        };
                        ui.label(RichText::new(summary).strong());
                        if count > 0 {
                            ui.label(total.map_or_else(
                                || text(japanese, "容量未確認", "Size unknown").into(),
                                bytes,
                            ));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label =
                                text(japanese, "選択した項目を確認", "Review selected versions");
                            preview = ui
                                .add_enabled(
                                    !busy && count > 0,
                                    egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                        .fill(ACCENT)
                                        .min_size(egui::vec2(190.0, 36.0)),
                                )
                                .clicked();
                        });
                    }
                });
                if !state.error.is_empty() {
                    ui.label(RichText::new(&state.error).color(Color32::from_rgb(153, 56, 37)));
                }
            });

        if let Some(result) = &state.receipt {
            show_receipt(ui, result, japanese);
        } else {
            match &state.report {
                Some(Ok(report)) => {
                    let candidates = report.items.iter().filter(|item| item.selectable).count();
                    let residuals = report
                        .items
                        .iter()
                        .filter(|item| matches!(item.target, Target::ResidualRuntime { .. }))
                        .count();
                    let protected = report.items.len() - candidates - residuals;
                    ui.label(
                        RichText::new(if japanese {
                            format!("候補 {candidates} 件 · 保護 {protected} 件 · 一部残存 {residuals} 件")
                        } else {
                            format!("{candidates} candidates · {protected} protected · {residuals} partially remaining")
                        })
                        .color(MUTED),
                    );
                    ui.add_space(10.0);
                    let wide = ui.available_width() >= 790.0;
                    if wide {
                        egui::Panel::right("sdk-inspector")
                            .resizable(false)
                            .default_size(270.0)
                            .show_inside(ui, |ui| {
                                sdk_inspector(ui, report, state.focused.as_ref(), japanese);
                            });
                    }
                    let detail_height = if !wide && state.focused.is_some() {
                        172.0
                    } else {
                        0.0
                    };
                    sdk_rows(
                        ui,
                        report,
                        &mut state.marked,
                        &mut state.focused,
                        japanese,
                        !busy,
                        detail_height,
                    );
                    if !wide && state.focused.is_some() {
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("sdk-compact-inspector")
                            .max_height(152.0)
                            .show(ui, |ui| {
                                sdk_inspector(ui, report, state.focused.as_ref(), japanese);
                            });
                    }
                    if !report.notes.is_empty() {
                        ui.collapsing(
                            text(japanese, "確認できなかった情報", "Inspection notes"),
                            |ui| {
                                for note in &report.notes {
                                    ui.label(note);
                                }
                            },
                        );
                    }
                }
                Some(Err(error)) => {
                    ui.label(text(
                        japanese,
                        "SDK の確認に失敗しました",
                        "Could not inspect SDKs",
                    ));
                    ui.label(format!("{error:#}"));
                }
                None => {}
            }
        }
        if refresh {
            self.scan_sdks(&ctx);
        } else if preview {
            if let Some(Ok(report)) = &state.report {
                match Review::new(report, &state.marked, self.profile.roots.clone()) {
                    Ok(review) => state.confirmation = Some(review),
                    Err(error) => {
                        state.error = format!(
                            "{} · {error:#}",
                            text(
                                japanese,
                                "選択した版を再確認してください",
                                "Check the selected versions"
                            )
                        );
                    }
                }
            }
        }
    }

    pub(super) fn sdk_dialog(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let Some(review) = &self.sdk.confirmation else {
            return;
        };
        let japanese = self.japanese_reasons;
        let mut cancel = false;
        let mut uninstall = false;
        let dialog = egui::Modal::new(egui::Id::new("sdk-uninstall-confirmation"))
            .frame(super::modal_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(420.0, 650.0));
                ui.heading(text(
                    japanese,
                    "選択した版をアンインストールしますか？",
                    "Uninstall these versions?",
                ));
                ui.label(text(
                    japanese,
                    "この版を使うテストやビルドには、同じ版の再インストールが必要です。",
                    "Tests or builds that use them will need the exact versions installed again.",
                ));
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for item in &review.items {
                            ui.label(
                                RichText::new(item_title(item, japanese))
                                    .strong()
                                    .color(INK),
                            );
                            ui.label(item.reason.text(japanese));
                            ui.add_space(8.0);
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    cancel = ui.button(text(japanese, "戻る", "Back")).clicked();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if japanese {
                            format!("{} 件をアンインストール", review.items.len())
                        } else {
                            format!("Uninstall {} selected", review.items.len())
                        };
                        uninstall = ui
                            .add(
                                egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                    .fill(Color32::from_rgb(180, 48, 42))
                                    .min_size(egui::vec2(190.0, 36.0)),
                            )
                            .clicked();
                    });
                });
            });
        if cancel || dialog.should_close() {
            self.sdk.confirmation = None;
        } else if uninstall {
            let review = self.sdk.confirmation.take().unwrap();
            self.sdk.report = None;
            self.sdk.marked.clear();
            self.sdk.focused = None;
            self.spawn(
                ctx,
                text(
                    japanese,
                    "確認した SDK をアンインストール中…",
                    "Uninstalling confirmed SDK versions…",
                ),
                true,
                move || Outcome::SdkRemoval(review.apply()),
            );
        }
    }
}

fn sdk_rows(
    ui: &mut egui::Ui,
    report: &Report,
    marked: &mut BTreeSet<Target>,
    focused: &mut Option<Target>,
    japanese: bool,
    enabled: bool,
    detail_height: f32,
) {
    let width = ui.available_width();
    let name_width = (width - 294.0).max(115.0);
    ui.horizontal(|ui| {
        ui.add_space(30.0);
        ui.add_sized(
            [name_width, 22.0],
            egui::Label::new(text(japanese, "項目", "Version")).halign(egui::Align::Min),
        );
        ui.add_sized(
            [90.0, 22.0],
            egui::Label::new(text(japanese, "容量", "Size")).halign(egui::Align::Max),
        );
        ui.label(text(japanese, "状態", "Status"));
    });
    ui.separator();
    let notes_height = if report.notes.is_empty() { 0.0 } else { 32.0 };
    let height = (ui.available_height() - detail_height - notes_height).max(96.0);
    egui::ScrollArea::vertical()
        .id_salt("sdk-versions")
        .max_height(height)
        .auto_shrink([false, false])
        .show_rows(ui, 48.0, report.items.len(), |ui, range| {
            for index in range {
                let item = &report.items[index];
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 48.0),
                    egui::Sense::click(),
                );
                let selected = focused.as_ref() == Some(&item.target);
                let tint = ui.ctx().animate_bool_with_time(
                    egui::Id::new(("sdk-row-focus", index)),
                    selected || response.hovered(),
                    ui.style().animation_time.min(0.12),
                );
                ui.painter().rect_filled(
                    rect,
                    4.0,
                    Color32::from_rgba_unmultiplied(
                        ACCENT.r(),
                        ACCENT.g(),
                        ACCENT.b(),
                        (tint * 24.0) as u8,
                    ),
                );
                ui.painter().hline(
                    rect.x_range(),
                    rect.bottom() - 0.5,
                    egui::Stroke::new(1.0_f32, LINE),
                );
                let mut row = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("sdk-version", index))
                        .max_rect(rect.shrink2(egui::vec2(4.0, 7.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                row.set_clip_rect(row.clip_rect().intersect(rect));
                let mut checked = marked.contains(&item.target);
                if row
                    .add_enabled(
                        enabled && item.selectable,
                        egui::Checkbox::without_text(&mut checked),
                    )
                    .on_hover_text(text(
                        japanese,
                        "アンインストールの確認に含める",
                        "Include in uninstall review",
                    ))
                    .changed()
                {
                    if checked {
                        marked.insert(item.target.clone());
                    } else {
                        marked.remove(&item.target);
                    }
                    *focused = Some(item.target.clone());
                }
                let name_response = row
                    .add_sized(
                        [name_width, 28.0],
                        egui::Label::new(RichText::new(&item.label).color(INK))
                            .selectable(false)
                            .sense(egui::Sense::click())
                            .halign(egui::Align::Min)
                            .truncate(),
                    )
                    .on_hover_text(&item.label)
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                name_response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        enabled,
                        selected,
                        &item.label,
                    )
                });
                if name_response.clicked() {
                    *focused = Some(item.target.clone());
                }
                row.add_sized(
                    [90.0, 28.0],
                    egui::Label::new(
                        RichText::new(item.size.map_or_else(|| "—".into(), bytes)).color(INK),
                    )
                    .halign(egui::Align::Max)
                    .truncate(),
                );
                let status = item_status(item, japanese);
                row.add(egui::Label::new(RichText::new(&status).color(INK)).truncate())
                    .on_hover_text(format!("{status} · {}", item.reason.text(japanese)));
                if response.clicked() {
                    *focused = Some(item.target.clone());
                }
            }
        });
}

fn sdk_inspector(ui: &mut egui::Ui, report: &Report, focused: Option<&Target>, japanese: bool) {
    ui.add_space(10.0);
    let Some(item) =
        focused.and_then(|target| report.items.iter().find(|item| &item.target == target))
    else {
        ui.label(
            RichText::new(text(japanese, "項目を選択", "Select a version"))
                .strong()
                .color(INK),
        );
        ui.label(text(
            japanese,
            "行を選ぶと、容量と判定理由を確認できます。",
            "Select a row to see its size and review basis.",
        ));
        return;
    };
    ui.label(RichText::new(&item.label).size(19.0).strong().color(INK));
    ui.add_space(6.0);
    ui.label(
        RichText::new(item_status(item, japanese))
            .strong()
            .color(INK),
    );
    if !matches!(item.target, Target::ResidualRuntime { .. }) {
        ui.label(
            RichText::new(item.size.map_or_else(
                || text(japanese, "容量未確認", "Size unknown").into(),
                bytes,
            ))
            .size(22.0)
            .strong()
            .color(INK),
        );
        ui.label(
            RichText::new(item_basis(item, japanese))
                .small()
                .color(MUTED),
        );
    }
    ui.add_space(16.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(
        RichText::new(text(japanese, "確認した根拠", "Review basis"))
            .strong()
            .color(INK),
    );
    ui.label(item.reason.text(japanese));
    ui.add_space(16.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(
        RichText::new(text(
            japanese,
            "アンインストールした場合",
            "After uninstalling",
        ))
        .strong()
        .color(INK),
    );
    ui.label(if matches!(item.target, Target::ResidualRuntime { .. }) {
        text(japanese, "イメージの残存を確認しています。この項目は選択できません。", "Remaining images were observed. This item cannot be selected.")
    } else if item.selectable {
        text(japanese, "この版を使うテストやビルドには、同じ版を再インストールする必要があります。", "Tests or builds using this version will require the same version to be installed again.")
    } else {
        text(japanese, "利用中、または確認が必要なため保護しています。この項目は選択できません。", "This version is protected because it is in use or needs further checks. It cannot be selected.")
    });
}

fn text(japanese: bool, ja: &'static str, en: &'static str) -> &'static str {
    if japanese {
        ja
    } else {
        en
    }
}

fn item_basis(item: &Item, japanese: bool) -> &str {
    if japanese {
        match item.basis {
            "Xcode estimate" => "Xcode の見積もり",
            "remaining image" => "残存イメージ",
            "allocated" => "ディスク使用量",
            other => other,
        }
    } else {
        item.basis
    }
}

fn item_title(item: &Item, japanese: bool) -> String {
    match item.size {
        Some(size) => format!(
            "{} · {}（{}）",
            item.label,
            bytes(size),
            item_basis(item, japanese)
        ),
        None => item.label.clone(),
    }
}

fn item_status(item: &Item, japanese: bool) -> String {
    if matches!(item.target, Target::ResidualRuntime { .. }) {
        partial_with_size(item.size, japanese)
    } else if item.selectable {
        text(japanese, "候補", "Candidate").into()
    } else {
        text(japanese, "保護", "Protected").into()
    }
}

fn partial_with_size(image_bytes: Option<u64>, japanese: bool) -> String {
    let status = text(japanese, "一部残存", "Partially remaining");
    match image_bytes {
        Some(size) => format!("{status} {}", bytes(size)),
        None => format!(
            "{status} · {}",
            text(japanese, "容量未確認", "size unknown")
        ),
    }
}

fn show_receipt(ui: &mut egui::Ui, result: &Result<Receipt>, japanese: bool) {
    ui.heading(text(japanese, "操作の結果", "Operation results"));
    ui.add_space(12.0);
    match result {
        Err(error) => {
            ui.label(text(
                japanese,
                "アンインストール結果を確認できませんでした",
                "Could not verify the uninstall result",
            ));
            ui.label(format!("{error:#}"));
        }
        Ok(receipt) => {
            ui.label(
                RichText::new(text(
                    japanese,
                    "実測した空き容量の変化",
                    "Measured free-space change",
                ))
                .color(MUTED),
            );
            match &receipt.after {
                Ok(after) => {
                    let sign = if *after >= receipt.before { "+" } else { "−" };
                    ui.label(
                        RichText::new(format!(
                            "{} {sign}{}",
                            text(japanese, "実測", "Measured change"),
                            bytes(after.abs_diff(receipt.before))
                        ))
                        .size(26.0)
                        .strong()
                        .color(INK),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} {} · {} {}",
                            text(japanese, "削除前", "Before"),
                            bytes(receipt.before),
                            text(japanese, "削除後", "After"),
                            bytes(*after)
                        ))
                        .color(MUTED),
                    );
                }
                Err(error) => {
                    ui.label(text(
                        japanese,
                        "削除後の空き容量を取得できませんでした",
                        "Could not measure free space after cleanup",
                    ));
                    ui.label(format!(
                        "{} {}",
                        text(japanese, "削除前", "Before"),
                        bytes(receipt.before)
                    ));
                    ui.label(format!("{error:#}"));
                }
            }
            ui.add_space(16.0);
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("sdk-removal-results")
                .show(ui, |ui| {
                    for (label, result) in &receipt.results {
                        ui.add_space(12.0);
                        ui.label(RichText::new(label).strong().color(INK));
                        let detail = match result {
                            Ok(Removal::Removed) => text(japanese, "削除済み", "Removed").into(),
                            Ok(Removal::Partial { image_bytes }) => {
                                partial_with_size(*image_bytes, japanese)
                            }
                            Ok(Removal::Unverified) => text(
                                japanese,
                                "削除結果を確認できませんでした",
                                "Could not verify complete removal",
                            )
                            .into(),
                            Err(error) => format!(
                                "{} · {error:#}",
                                text(
                                    japanese,
                                    "処理を完了できませんでした",
                                    "Could not complete the operation"
                                )
                            ),
                        };
                        ui.label(RichText::new(detail).color(INK));
                        if matches!(
                            result,
                            Ok(Removal::Partial {
                                image_bytes: Some(_)
                            })
                        ) {
                            ui.label(
                                RichText::new(text(
                                    japanese,
                                    "確認できた残存イメージの容量です。",
                                    "Observed size of the remaining images.",
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                        ui.add_space(12.0);
                        ui.separator();
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_version_name_opens_details_without_selecting_it_for_removal() {
        let ctx = egui::Context::default();
        let target = Target::ResidualRuntime {
            identifier: "test-runtime".into(),
            build: "test-build".into(),
        };
        let report = Report {
            items: vec![Item {
                target: target.clone(),
                label: "Protected SDK".into(),
                size: Some(1024),
                basis: "remaining image",
                reason: super::super::Message::new("Protected", "保護"),
                selectable: false,
            }],
            ..Report::default()
        };
        let mut marked = BTreeSet::new();
        let mut focused = None;
        let mut render = |events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| sdk_rows(ui, &report, &mut marked, &mut focused, false, true, 0.0),
            )
        };
        let first = render(Vec::new());
        let position = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Protected SDK" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("the protected version name must remain visible");
        let button = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = render(vec![egui::Event::PointerMoved(position), button(true)]);
        let _ = render(vec![button(false)]);
        assert_eq!(focused, Some(target));
        assert!(marked.is_empty());
    }
}
