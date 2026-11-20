use super::*;

impl App {
    pub(super) fn stale_dialog(&mut self, ctx: &egui::Context, page: Option<&mut egui::Ui>) {
        #[cfg(target_os = "macos")]
        let mut workspace_requested = false;
        let mut start = false;
        let mut close = false;
        let as_page = page.is_some();
        let busy = self.job.is_some();
        let state = &mut self.stale_files;
        let render = |ui: &mut egui::Ui| {
            if !as_page {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 920.0));
            }
            ui.heading(text(
                self.japanese_reasons,
                "古いファイルを探す",
                "Find old files",
            ));
            ui.label(text(
                self.japanese_reasons,
                "ホーム内の古い大容量ファイルを探します。使用中かどうかは確認しません。",
                "Find old large files in your home folder. Open processes are not checked.",
            ));
            #[cfg(target_os = "macos")]
            if as_page
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(text(
                            self.japanese_reasons,
                            "ワークスペースを開く",
                            "Open Workspace",
                        )),
                    )
                    .clicked()
            {
                workspace_requested = true;
            }
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label(state.root.as_deref().map_or_else(
                        || text(self.japanese_reasons, "ホーム（~）", "Home (~)").into(),
                        display,
                    ));
                    if ui
                        .button(text(
                            self.japanese_reasons,
                            "フォルダを選ぶ…",
                            "Choose folder…",
                        ))
                        .clicked()
                    {
                        let mut picker = rfd::FileDialog::new();
                        if let Some(root) = state.root.clone().or_else(|| {
                            directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned())
                        }) {
                            picker = picker.set_directory(root);
                        }
                        if let Some(path) = picker.pick_folder() {
                            state.root = Some(path);
                            state.report = None;
                            state.error.clear();
                        }
                    }
                    if ui
                        .add_enabled(
                            state.root.is_some(),
                            egui::Button::new(text(
                                self.japanese_reasons,
                                "ホーム全体に戻す",
                                "Use home",
                            )),
                        )
                        .clicked()
                    {
                        state.root = None;
                        state.report = None;
                        state.error.clear();
                    }
                });
                if ui
                    .checkbox(
                        &mut state.include_outputs,
                        text(
                            self.japanese_reasons,
                            "開発プロジェクトの生成物を含める",
                            "Include project build output",
                        ),
                    )
                    .changed()
                {
                    state.report = None;
                }
                ui.small(text(
                    self.japanese_reasons,
                    "隠し項目、Library、node_modules は常に対象外です。",
                    "Hidden items, Library, and node_modules are always excluded.",
                ));
                ui.horizontal(|ui| {
                    ui.label(text(
                        self.japanese_reasons,
                        "最後の活動からの日数",
                        "Days since last activity:",
                    ));
                    ui.add(egui::DragValue::new(&mut state.days).range(1..=3650));
                    ui.label(text(
                        self.japanese_reasons,
                        "最小容量（MiB）",
                        "Minimum size (MiB):",
                    ));
                    ui.add(egui::DragValue::new(&mut state.mib).range(1..=1_000_000));
                    start = ui
                        .button(text(self.japanese_reasons, "調べる", "Scan"))
                        .clicked();
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(text(self.japanese_reasons, "並べ替え", "Sort by"));
                    use crate::cli::StaleSort;
                    for (order, label) in [
                        (
                            StaleSort::Size,
                            text(self.japanese_reasons, "容量が大きい順", "Largest"),
                        ),
                        (
                            StaleSort::Modified,
                            text(self.japanese_reasons, "更新日が古い順", "Oldest modified"),
                        ),
                        (
                            StaleSort::Created,
                            text(self.japanese_reasons, "作成日が古い順", "Oldest created"),
                        ),
                    ] {
                        if ui.selectable_value(&mut state.sort, order, label).changed() {
                            if let Some(report) = &mut state.report {
                                report.set_order(order);
                            }
                        }
                    }
                });
            });
            if busy && !as_page {
                ui.spinner();
            }
            if !state.error.is_empty() {
                ui.label(&state.error);
            }
            if let Some(report) = &state.report {
                ui.label(if self.japanese_reasons {
                    format!(
                        "{} · 該当 {} 件 · 日時不明で除外 {} 件 · 読み取り不可 {} 件",
                        report.root,
                        report.candidates.len() + report.omitted,
                        report.unknown_age,
                        report.skipped
                    )
                } else {
                    format!(
                        "{} · {} matches · {} with unknown dates excluded · {} unread entries",
                        report.root,
                        report.candidates.len() + report.omitted,
                        report.unknown_age,
                        report.skipped
                    )
                });
                if report.omitted > 0 {
                    ui.label(text(
                        self.japanese_reasons,
                        "選んだ順序の上位100件を表示しています。",
                        "Showing the first 100 matches in the selected order.",
                    ));
                }
                if !report.candidates.is_empty() {
                    ui.add(controls::field(ui, &mut state.query).hint_text(text(
                        self.japanese_reasons,
                        "パスで絞り込む",
                        "Filter results by path",
                    )));
                    let query = state.query.to_lowercase();
                    egui::ScrollArea::vertical()
                        .id_salt("personal-file-results")
                        .max_height((ui.available_height() - 46.0).max(120.0))
                        .show(ui, |ui| {
                            for row in report.candidates.iter().filter(|row| {
                                query.is_empty() || row.path.to_lowercase().contains(&query)
                            }) {
                                let name = Path::new(&row.path)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or(&row.path);
                                let title = format!(
                                    "{}  ·  {}  ·  {}",
                                    name,
                                    bytes(row.logical_bytes),
                                    stale::date(Some(row.last_activity_at))
                                );
                                egui::CollapsingHeader::new(title)
                                    .id_salt(&row.path)
                                    .show(ui, |ui| {
                                        ui.label(&row.path);
                                        ui.small(format!(
                                            "{} {}   {} {}   {} {}",
                                            text(self.japanese_reasons, "更新", "Modified"),
                                            stale::date(row.modified_at),
                                            text(self.japanese_reasons, "作成", "Created"),
                                            stale::date(row.created_at),
                                            text(self.japanese_reasons, "アクセス", "Accessed"),
                                            stale::date(row.accessed_at)
                                        ));
                                        ui.small(match row.open_state {
                                            "currently_open" => text(
                                                self.japanese_reasons,
                                                "使用中",
                                                "Open in a process",
                                            ),
                                            "not_seen_open" => text(
                                                self.japanese_reasons,
                                                "使用中のプロセスは見つかりません",
                                                "No open process found",
                                            ),
                                            "unknown" => text(
                                                self.japanese_reasons,
                                                "使用状況を確認できません",
                                                "Could not check open processes",
                                            ),
                                            _ => text(
                                                self.japanese_reasons,
                                                "使用状況は未確認",
                                                "Open processes unchecked",
                                            ),
                                        });
                                        ui.horizontal(|ui| {
                                            #[cfg(target_os = "macos")]
                                            if ui
                                                .button(text(
                                                    self.japanese_reasons,
                                                    "Finder で表示",
                                                    "Show in Finder",
                                                ))
                                                .clicked()
                                            {
                                                if let Err(err) = open_in_finder(&row.native_path) {
                                                    state.error =
                                                        display_error(&err, self.japanese_reasons);
                                                }
                                            }
                                            if ui
                                                .button(text(
                                                    self.japanese_reasons,
                                                    "パスをコピー",
                                                    "Copy path",
                                                ))
                                                .clicked()
                                            {
                                                ctx.copy_text(row.path.clone());
                                            }
                                        });
                                    })
                                    .header_response
                                    .on_hover_text(&row.path);
                            }
                        });
                }
            }
            ui.label(text(
                self.japanese_reasons,
                "アクセス日時だけでは未使用と断定できません。内容と用途を確認してください。",
                "Access dates can be incomplete. Check what a file is before removing it.",
            ));
            if !as_page {
                close = ui
                    .button(text(self.japanese_reasons, "閉じる", "Close"))
                    .clicked();
            }
        };
        let should_close = if let Some(ui) = page {
            egui::Frame::NONE.show(ui, render);
            false
        } else {
            egui::Modal::new(egui::Id::new("old-files"))
                .show(ctx, render)
                .should_close()
        };
        if close || should_close {
            self.show_stale_files = false;
        }
        if start {
            let (root, days, mib, sort, include_outputs) = (
                state.root.clone(),
                state.days,
                state.mib,
                state.sort,
                state.include_outputs,
            );
            state.report = None;
            state.error.clear();
            state.query.clear();
            self.spawn(
                ctx,
                text(
                    self.japanese_reasons,
                    "古いファイルを検索中…",
                    "Looking for old files…",
                ),
                false,
                move || {
                    Outcome::Stale(stale::scan_personal(
                        root.as_deref(),
                        days,
                        mib,
                        sort,
                        include_outputs,
                    ))
                },
            );
        }
        #[cfg(target_os = "macos")]
        if workspace_requested {
            self.navigate(shell::Screen::Workspace);
        }
    }
}
