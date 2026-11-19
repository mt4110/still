use super::{egui, icons, monitor, text, App, ACCENT, INK, LINE, MUTED};
use egui::{RichText, Stroke};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Screen {
    #[default]
    Overview,
    Workspace,
    Disk,
    Docker,
    Apps,
    OldFiles,
}

impl Screen {
    fn title(self, japanese: bool) -> &'static str {
        match self {
            Self::Overview => text(japanese, "概要", "Overview"),
            Self::Workspace => text(japanese, "ワークスペース", "Workspace"),
            Self::Disk => text(japanese, "ディスク", "Disk"),
            Self::Docker => "Docker",
            Self::Apps => text(japanese, "アプリと SDK", "Apps & SDKs"),
            Self::OldFiles => text(japanese, "古いファイル", "Old files"),
        }
    }

    fn icon(self) -> icons::Icon {
        match self {
            Self::Overview => icons::Icon::Home,
            Self::Workspace => icons::Icon::Folder,
            Self::Disk => icons::Icon::Disk,
            Self::Docker => icons::Icon::Docker,
            Self::Apps => icons::Icon::Sdk,
            Self::OldFiles => icons::Icon::Clock,
        }
    }
}

fn navigation(ui: &mut egui::Ui, screen: Screen, active: bool, japanese: bool) -> egui::Response {
    let label = screen.title(japanese);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    let selected = ui.ctx().animate_bool(response.id.with("selected"), active);
    let hover = ui
        .ctx()
        .animate_bool(response.id.with("hover"), response.hovered());
    let fill = egui::Color32::from_rgb(242, 243, 246)
        .lerp_to_gamma(egui::Color32::from_rgb(228, 231, 236), hover)
        .lerp_to_gamma(egui::Color32::from_rgb(213, 229, 251), selected);
    ui.painter().rect_filled(rect, 7, fill);
    let color = if active { ACCENT } else { INK };
    icons::paint(
        ui,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 20.0, rect.center().y),
            egui::vec2(19.0, 19.0),
        ),
        screen.icon(),
        color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 40.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(15.0),
        color,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            7,
            Stroke::new(2.0_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl App {
    pub(super) fn navigate(&mut self, screen: Screen) {
        if self.screen == screen {
            return;
        }
        self.monitor.close();
        self.sdk.open = screen == Screen::Apps;
        match screen {
            Screen::Disk => self.monitor.open_tab(monitor::Tab::Disk),
            Screen::Docker => {
                self.monitor.open_tab(monitor::Tab::Docker);
                if self.monitor.context.is_empty() {
                    self.monitor.context = self.profile.docker_context.clone().unwrap_or_default();
                }
            }
            Screen::Overview => self.home.update_capacity(),
            Screen::Apps => self.sdk.open = true,
            _ => {}
        }
        self.screen = screen;
    }

    pub(super) fn desktop_shell(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Compatibility with actions launched from the folder explorer.
        if self.show_stale_files {
            self.show_stale_files = false;
            self.navigate(Screen::OldFiles);
        }
        if self.show_app_storage {
            self.show_app_storage = false;
            self.app_storage_selected = true;
            self.navigate(Screen::Apps);
        }
        egui::Panel::left("navigation")
            .exact_size(190.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(242, 243, 246))
                    .inner_margin(12),
            )
            .show_inside(ui, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("still").size(25.0).strong().color(INK));
                });
                ui.add_space(25.0);
                for screen in [
                    Screen::Overview,
                    Screen::Workspace,
                    Screen::Disk,
                    Screen::Docker,
                    Screen::Apps,
                    Screen::OldFiles,
                ] {
                    if navigation(ui, screen, screen == self.screen, self.japanese_reasons)
                        .clicked()
                    {
                        self.navigate(screen);
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(text(
                                    self.japanese_reasons,
                                    "プライバシーと権限",
                                    "Privacy & access",
                                ))
                                .size(12.0)
                                .color(MUTED),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.show_help = true;
                    }
                    egui::ComboBox::from_id_salt("language")
                        .width(142.0)
                        .selected_text(if self.japanese_reasons {
                            "日本語"
                        } else {
                            "English"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.japanese_reasons, true, "日本語");
                            ui.selectable_value(&mut self.japanese_reasons, false, "English");
                        });
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(24, 16)),
            )
            .show_inside(ui, |ui| {
                self.page_toolbar(ui);
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(14.0);
                self.activity(ui);
                match self.screen {
                    Screen::Overview => self.show_home(ui),
                    Screen::Workspace => {
                        if self.scan.is_some() {
                            self.cleanup_result(ui);
                            if let Some(scan) = &self.scan {
                                for note in &scan.notes {
                                    ui.small(note);
                                }
                            }
                            ui.add_enabled_ui(self.job.is_none(), |ui| self.workspace(ui));
                        } else if self.job.is_none() {
                            self.show_home(ui);
                        }
                    }
                    Screen::Disk => self.monitor_page(ui, monitor::Tab::Disk),
                    Screen::Docker => self.monitor_page(ui, monitor::Tab::Docker),
                    Screen::Apps => {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.app_storage_selected,
                                false,
                                text(self.japanese_reasons, "SDK のバージョン", "SDK versions"),
                            );
                            ui.selectable_value(
                                &mut self.app_storage_selected,
                                true,
                                text(self.japanese_reasons, "アプリの保存容量", "App storage"),
                            );
                        });
                        ui.add_space(12.0);
                        if self.app_storage_selected {
                            self.app_storage_dialog(&ctx, Some(ui));
                        } else {
                            self.sdk_page(ui);
                        }
                    }
                    Screen::OldFiles => self.stale_dialog(&ctx, Some(ui)),
                }
            });
    }

    fn page_toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(self.screen.title(self.japanese_reasons))
                    .color(MUTED)
                    .size(13.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(
                    ui,
                    icons::Icon::Help,
                    text(self.japanese_reasons, "ヘルプ", "Help"),
                )
                .clicked()
                {
                    self.show_help = true;
                }
                if self.screen == Screen::Workspace || self.screen == Screen::Overview {
                    ui.add_enabled_ui(self.job.is_none(), |ui| {
                        ui.menu_button(
                            text(
                                self.japanese_reasons,
                                "調査先とプロファイル",
                                "Workspace & profiles",
                            ),
                            |ui| {
                                if self.scan.is_some()
                                    && ui
                                        .button(text(
                                            self.japanese_reasons,
                                            "プロジェクト一覧",
                                            "Projects",
                                        ))
                                        .clicked()
                                {
                                    self.projects.active = true;
                                    self.navigate(Screen::Workspace);
                                    ui.close();
                                }
                                if self.scan.is_some()
                                    && ui
                                        .button(text(
                                            self.japanese_reasons,
                                            "フォルダ探索・一括操作",
                                            "Explore folders & batch actions",
                                        ))
                                        .clicked()
                                {
                                    self.projects.active = false;
                                    self.marked.clear();
                                    self.rebuild();
                                    self.navigate(Screen::Workspace);
                                    ui.close();
                                }
                                self.toolbar_actions(ui, &ctx);
                            },
                        );
                        if self.scan.is_some()
                            && ui
                                .button(text(self.japanese_reasons, "再スキャン", "Refresh"))
                                .clicked()
                        {
                            self.refresh(&ctx, true);
                        }
                    });
                }
            });
        });
        if self.screen == Screen::Workspace && self.profile.roots.len() > 1 {
            ui.add_enabled_ui(self.job.is_none(), |ui| {
                let mut index = self.index;
                egui::ComboBox::from_id_salt("workspace")
                    .selected_text(super::display(&self.profile.roots[self.index]))
                    .show_ui(ui, |ui| {
                        for (i, root) in self.profile.roots.iter().enumerate() {
                            ui.selectable_value(&mut index, i, super::display(root));
                        }
                    });
                self.switch_workspace(&ctx, index);
            });
        }
    }

    fn activity(&self, ui: &mut egui::Ui) {
        if let Some(job) = &self.job {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgb(243, 247, 254))
                .inner_margin(12)
                .corner_radius(7)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            if job
                                .progress
                                .as_ref()
                                .is_some_and(|p| p.snapshot().inventory_done)
                            {
                                text(
                                    self.japanese_reasons,
                                    "候補の安全性と利用状況を確認中…",
                                    "Checking candidate safety and use…",
                                )
                            } else {
                                job.label
                            },
                        );
                    });
                    if let Some(progress) = &job.progress {
                        let snapshot = progress.snapshot();
                        ui.small(if self.japanese_reasons {
                            format!(
                                "確認済み {} ファイル・{} フォルダ · {}",
                                snapshot.files,
                                snapshot.directories,
                                super::bytes(snapshot.logical_bytes)
                            )
                        } else {
                            format!(
                                "{} files · {} folders checked · {}",
                                snapshot.files,
                                snapshot.directories,
                                super::bytes(snapshot.logical_bytes)
                            )
                        });
                        if let Some(path) = snapshot.current_directory {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(super::display(&path)).small().color(MUTED),
                                )
                                .truncate(),
                            );
                        }
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    }
                });
            ui.add_space(12.0);
        }
        if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).color(egui::Color32::from_rgb(160, 50, 40)));
            ui.add_space(10.0);
        }
    }

    fn cleanup_result(&self, ui: &mut egui::Ui) {
        if let Some(receipt) = &self.cleanup_receipt {
            let actual = match (receipt.before, receipt.after) {
                (Some(before), Some(after)) => format!(
                    "{}{}",
                    if after >= before { "+" } else { "−" },
                    super::bytes(after.abs_diff(before))
                ),
                _ => text(self.japanese_reasons, "測定できませんでした", "Unavailable").into(),
            };
            egui::Frame::NONE
                .stroke(Stroke::new(1.0_f32, LINE))
                .corner_radius(9)
                .inner_margin(16)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.strong(text(self.japanese_reasons, "掃除の結果", "Cleanup result"));
                    ui.label(RichText::new(&actual).size(26.0).strong().color(ACCENT));
                    ui.small(text(
                        self.japanese_reasons,
                        "ボリューム空き容量の実測変化",
                        "Measured change in volume free space",
                    ));
                    ui.label(if self.japanese_reasons {
                        format!(
                            "削除 {} 件 · 保留 {} 件",
                            receipt.outcomes.len() - receipt.held,
                            receipt.held
                        )
                    } else {
                        format!(
                            "{} deleted · {} held",
                            receipt.outcomes.len() - receipt.held,
                            receipt.held
                        )
                    });
                    ui.collapsing(
                        text(
                            self.japanese_reasons,
                            "項目ごとの結果",
                            "Individual results",
                        ),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(120.0)
                                .show(ui, |ui| {
                                    for (path, result) in &receipt.outcomes {
                                        ui.label(match result {
                                            Ok(()) => path.clone(),
                                            Err(reason) => format!(
                                                "{path} · {}",
                                                reason.text(self.japanese_reasons)
                                            ),
                                        });
                                    }
                                });
                        },
                    );
                });
            ui.add_space(12.0);
        }
    }
}
