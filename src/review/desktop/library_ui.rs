use super::*;

impl App {
    pub(super) fn app_storage_dialog(&mut self, ctx: &egui::Context, page: Option<&mut egui::Ui>) {
        let busy = self.job.is_some();
        let mut start = false;
        let mut close = false;
        let as_page = page.is_some();
        #[cfg(target_os = "macos")]
        let mut review_sdk = false;
        #[cfg(target_os = "macos")]
        let mut cache_action = None;
        let render = |ui: &mut egui::Ui| {
            if !as_page {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(590.0, 920.0));
            }
            ui.heading(text(
                self.japanese_reasons,
                "アプリと SDK の容量",
                "App storage",
            ));
            ui.label(text(
                self.japanese_reasons,
                "SDK、ブラウザキャッシュ、アプリのデータを容量順に確認します。",
                "Review SDKs, browser caches, and protected app data by size.",
            ));
            ui.label(text(
                self.japanese_reasons,
                "所有元と再生成方法を確認できたキャッシュは、内容を確認してから整理できます。",
                "Review an eligible cache separately before removing it.",
            ));
            ui.add_enabled_ui(!busy, |ui| {
                ui.checkbox(
                    &mut self.xcode_runtimes,
                    text(
                        self.japanese_reasons,
                        "Xcode のインストール済みランタイムも確認する",
                        "Ask Xcode for installed simulator runtimes",
                    ),
                );
                ui.checkbox(
                    &mut self.library_inventory,
                    text(
                        self.japanese_reasons,
                        "Library の主な保存先を確認する（時間がかかる場合があります）",
                        "Inventory major Library folders (may take time)",
                    ),
                );
                start = ui
                    .button(text(self.japanese_reasons, "調べる", "Scan"))
                    .clicked();
                #[cfg(target_os = "macos")]
                {
                    review_sdk = ui
                        .button(text(
                            self.japanese_reasons,
                            "Xcode と NDK を版ごとに確認…",
                            "Review Xcode & NDK versions…",
                        ))
                        .clicked();
                }
            });
            if busy && !as_page {
                ui.spinner();
            }
            #[cfg(target_os = "macos")]
            self.library_panel.show_receipt(ui, self.japanese_reasons);
            match &self.app_storage {
                Some(Ok(report)) => {
                    ui.label(if self.japanese_reasons {
                        format!("{} 件", report.rows.len())
                    } else {
                        format!("{} items", report.rows.len())
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("app-storage-rows")
                        .max_height((ui.available_height() - 32.0).max(160.0))
                        .show(ui, |ui| {
                            for (index, row) in report.rows.iter().enumerate() {
                                let size = row.size_bytes.map_or_else(|| "—".into(), bytes);
                                let partial = if row.partial {
                                    text(
                                        self.japanese_reasons,
                                        " · 一部を読み取れませんでした",
                                        " · partial read",
                                    )
                                } else {
                                    ""
                                };
                                let title = format!(
                                    "{}  ·  {size} {}{partial}",
                                    row.label,
                                    size_basis(row.size_basis, self.japanese_reasons)
                                );
                                #[cfg(target_os = "macos")]
                                let title = format!(
                                    "{title}{}",
                                    self.library_panel.marker(row, index, self.japanese_reasons)
                                );
                                let header = egui::CollapsingHeader::new(title).id_salt((
                                    row.path.as_deref(),
                                    &row.label,
                                    index,
                                ));
                                header.show(ui, |ui| {
                                    if let Some(path) = &row.path {
                                        ui.label(path);
                                    }
                                    ui.label(row.reason.text(self.japanese_reasons));
                                    #[cfg(target_os = "macos")]
                                    {
                                        if let Some(evidence) = &row.library {
                                            ui.small(evidence.activity_text(self.japanese_reasons));
                                        }
                                        if let Some(action) = self.library_panel.row(
                                            ui,
                                            row,
                                            index,
                                            busy,
                                            self.japanese_reasons,
                                        ) {
                                            cache_action = Some(action);
                                        }
                                    }
                                });
                                ui.separator();
                            }
                            for note in &report.notes {
                                ui.label(note);
                            }
                        });
                }
                Some(Err(error)) => {
                    ui.label(format!(
                        "{} · {error:#}",
                        text(
                            self.japanese_reasons,
                            "容量を取得できませんでした",
                            "Could not read storage usage"
                        )
                    ));
                }
                None => {}
            }
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
            egui::Modal::new(egui::Id::new("app-storage"))
                .show(ctx, render)
                .should_close()
        };
        if close || should_close {
            self.show_app_storage = false;
        }
        #[cfg(target_os = "macos")]
        if review_sdk {
            self.show_app_storage = false;
            self.app_storage_selected = false;
            self.navigate(shell::Screen::Apps);
            self.sdk.open = true;
            self.scan_sdks(ctx);
        }
        #[cfg(target_os = "macos")]
        if let Some(action) = cache_action {
            self.cache_action(ctx, action);
        }
        if start {
            self.app_storage = None;
            #[cfg(target_os = "macos")]
            {
                self.library_panel = Panel::default();
            }
            let xcode = self.xcode_runtimes;
            let library = self.library_inventory;
            self.spawn(
                ctx,
                text(
                    self.japanese_reasons,
                    "アプリの保存容量を調査中…",
                    "Scanning app storage…",
                ),
                false,
                move || Outcome::AppStorage(app_storage::desktop_scan(xcode, library)),
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) use mac::{Panel, SavedReceipt};

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use crate::review::{app_storage::known_cache, cleanup_history};
    use keep::KeepRules;

    #[derive(Default)]
    pub(in crate::review::desktop) struct Panel {
        kept: Option<Result<KeepRules>>,
        cache_row: Option<(usize, PathBuf)>,
        settings_error: Option<String>,
        pub(in crate::review::desktop) review: Option<Result<known_cache::PreparedCache>>,
        pub(in crate::review::desktop) receipt: Option<Result<SavedReceipt>>,
    }

    pub(in crate::review::desktop) struct SavedReceipt {
        record: cleanup_history::Record,
        history_error: Option<String>,
    }

    pub(super) enum Action {
        Keep(PathBuf, bool),
        Review(PathBuf),
    }

    impl Panel {
        pub(in crate::review::desktop) fn scanned(&mut self, result: &Result<app_storage::Report>) {
            self.load_kept();
            self.cache_row =
                result.as_ref().ok().and_then(|report| {
                    report.rows.iter().enumerate().find_map(|(index, row)| {
                        known_cache::candidate(row).map(|path| (index, path))
                    })
                });
        }

        fn target(&self, index: usize) -> Option<&Path> {
            self.cache_row
                .as_ref()
                .filter(|(candidate, _)| *candidate == index)
                .map(|(_, path)| path.as_path())
        }

        pub(super) fn marker(
            &self,
            row: &app_storage::Row,
            index: usize,
            ja: bool,
        ) -> &'static str {
            let candidate = self.target(index);
            if candidate.is_none() && row.library.is_none() {
                return "";
            }
            let path = candidate.or(row.native_path.as_deref());
            if path.is_some_and(|path| {
                self.kept
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .is_some_and(|rules| rules.conflict(path).is_some())
            }) {
                text(ja, " · 残す設定", " · Kept")
            } else if candidate.is_some() {
                text(ja, " · HTTP キャッシュを確認", " · Review HTTP cache")
            } else {
                ""
            }
        }

        pub(in crate::review::desktop) fn load_kept(&mut self) {
            self.kept = Some(KeepRules::load());
        }

        pub(super) fn row(
            &self,
            ui: &mut egui::Ui,
            row: &app_storage::Row,
            index: usize,
            busy: bool,
            ja: bool,
        ) -> Option<Action> {
            let candidate = self.target(index);
            if candidate.is_none() && row.library.is_none() {
                return None;
            }
            let keep_path = candidate.or(row.native_path.as_deref())?;
            let mut action = None;
            let rules = match &self.kept {
                Some(Ok(rules)) => rules,
                _ => return None,
            };
            let conflict = rules.conflict(keep_path);
            if let Some(path) = conflict {
                ui.label(format!(
                    "{} · {}",
                    text(ja, "残す設定", "Keep rule"),
                    display(path)
                ));
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(text(
                            ja,
                            "この「残す」設定を解除",
                            "Remove this keep rule",
                        )),
                    )
                    .clicked()
                {
                    action = Some(Action::Keep(path.to_owned(), false));
                }
            } else {
                if let Some(path) = candidate {
                    ui.label(text(ja,"通常プロファイルの HTTP キャッシュを確認できます。再取得が必要なため、次のページ表示が遅くなる場合があります。","Review the default profile’s HTTP cache. The browser may need to download resources again."));
                    ui.small(display(path));
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(text(
                                ja,
                                "Library の整理で残す",
                                "Keep in Library cleanup",
                            )),
                        )
                        .clicked()
                    {
                        action = Some(Action::Keep(keep_path.to_owned(), true));
                    }
                    if let Some(path) = candidate {
                        if controls::button(
                            ui,
                            text(
                                ja,
                                "既定プロファイルの HTTP キャッシュを確認…",
                                "Review default profile HTTP cache…",
                            ),
                            true,
                            !busy,
                        )
                        .clicked()
                        {
                            action = Some(Action::Review(path.to_owned()));
                        }
                    }
                });
            }
            action
        }

        pub(super) fn show_receipt(&mut self, ui: &mut egui::Ui, ja: bool) {
            if let Some(Err(err)) = self.kept.as_ref() {
                ui.label(format!(
                    "{} · {err:#}",
                    text(
                        ja,
                        "「残す」設定を取得できないため整理を保留します",
                        "Cleanup on hold: keep rules unavailable"
                    )
                ));
            }
            if let Some(err) = &self.settings_error {
                ui.label(format!(
                    "{} · {err}",
                    text(ja, "設定を保存できませんでした", "Could not save keep rule")
                ));
            }
            let Some(receipt) = &self.receipt else { return };
            let close = egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(text(ja,"HTTP キャッシュの整理結果","HTTP cache cleanup result"));
                match receipt {
                    Err(err) => {
                        ui.label(text(ja,"整理の完了を確認できませんでした。再スキャンで現在の状態を確認してください。","Could not confirm completion. Scan again to check current usage."));
                        ui.label(format!("{err:#}"));
                    }
                    Ok(saved) => {
                        let record = &saved.record;
                        for item in &record.items {
                            let status = match item.status.as_str() {
                                "removed" => text(ja,"削除済み","Removed"),
                                "incomplete" => text(ja,"未完了","Incomplete"),
                                other => other,
                            };
                            ui.label(format!("{status} · {}", display(&item.path)));
                            if let Some(reason) = &item.reason { ui.small(reason); }
                            if item.original_path_exists != Some(false) {
                                ui.small(match item.original_path_exists {
                                    Some(true) => text(ja,"元の場所にデータが残っています。","Data remains at the original path."),
                                    _ => text(ja,"元の場所の状態は未確認です。","Original path could not be verified."),
                                });
                            }
                        }
                        if let Some((before, after)) = record.available_before.zip(record.available_after) {
                            let sign = if after < before { "−" } else { "+" };
                            ui.label(format!("{} {} → {} ({sign}{})", text(ja,"ボリュームの空き容量","Volume free space"), bytes(before), bytes(after), bytes(after.abs_diff(before))));
                        } else {
                            ui.label(text(ja,"空き容量の差は測定できませんでした。","Free-space change unavailable."));
                        }
                        if let Some(err) = &saved.history_error {
                            ui.small(format!("{} · {err}", text(ja,"履歴を保存できませんでした","Could not save history")));
                        }
                    }
                }
                ui.button(text(ja,"結果を閉じる","Dismiss result")).clicked()
            }).inner;
            if close {
                self.receipt = None;
            }
        }
    }

    impl App {
        pub(super) fn cache_action(&mut self, ctx: &egui::Context, action: Action) {
            if self.job.is_some() {
                return;
            }
            match action {
                Action::Keep(path, keep) => {
                    self.library_panel.settings_error = KeepRules::update(&path, keep)
                        .err()
                        .map(|e| format!("{e:#}"));
                    self.library_panel.load_kept();
                }
                Action::Review(path) => {
                    let docker = self.profile.docker_context.clone();
                    self.library_panel.receipt = None;
                    self.spawn(
                        ctx,
                        text(
                            self.japanese_reasons,
                            "キャッシュの利用状況を確認中…",
                            "Checking cache usage…",
                        ),
                        false,
                        move || {
                            Outcome::CachePrepared((|| {
                                KeepRules::load()?.check(&path)?;
                                known_cache::prepare(&path, docker.as_deref())
                            })())
                        },
                    );
                }
            }
        }

        pub(in crate::review::desktop) fn library_confirmation(&mut self, ctx: &egui::Context) {
            let Some(result) = &self.library_panel.review else {
                return;
            };
            let ja = self.japanese_reasons;
            let mut remove = false;
            let mut close = false;
            let dialog = egui::Modal::new("library-cache-confirmation".into()).frame(modal_frame(ctx)).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 120.0).clamp(400.0, 650.0));
                match result {
                    Err(err) => {
                        ui.heading(text(ja,"整理を保留しました","Cleanup on hold"));
                        ui.label(format!("{err:#}"));
                        close = ui.button(text(ja,"閉じる","Close")).clicked();
                    }
                    Ok(review) => {
                        ui.heading(text(ja,"この HTTP キャッシュを削除しますか？","Remove this HTTP cache?"));
                        ui.label(display(&review.path));
                        ui.label(format!("{} · {}", text(ja,"再生成するアプリ","Regenerated by"), display(&review.bundle_path)));
                        ui.label(format!("{} {}　{} {}　{} {}", text(ja,"ディスク使用量","Allocated"), bytes(review.allocated_bytes), text(ja,"論理容量","Logical"), bytes(review.logical_bytes), text(ja,"ファイル数","Files"),review.files));
                        ui.label(text(ja,"完全削除します。次回の閲覧時に必要なコンテンツを再取得します。オフラインでは再取得できません。","Removal is permanent. Chrome downloads resources again when you visit pages. Downloads require a network connection."));
                        ui.horizontal(|ui| {
                            close = ui.button(text(ja,"キャンセル","Cancel")).clicked();
                            remove = controls::destructive_button(ui, text(ja,"確認したキャッシュを削除","Remove reviewed cache"), self.job.is_none()).clicked();
                        });
                    }
                }
            });
            if close || dialog.should_close() {
                self.library_panel.review = None;
            } else if remove {
                let Some(Ok(review)) = self.library_panel.review.take() else {
                    return;
                };
                self.spawn(
                    ctx,
                    text(
                        ja,
                        "確認したキャッシュを削除中…",
                        "Removing reviewed cache…",
                    ),
                    true,
                    move || {
                        Outcome::CacheFinished((|| {
                            KeepRules::load()?.check(&review.path)?;
                            let record = known_cache::apply(review)?;
                            #[cfg(not(test))]
                            let history_error = cleanup_history::save(&record)
                                .err()
                                .map(|e| format!("{e:#}"));
                            #[cfg(test)]
                            let history_error = None;
                            Ok(SavedReceipt {
                                record,
                                history_error,
                            })
                        })())
                    },
                );
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use app_storage::known_cache;

    #[test]
    fn escaping_a_prepared_cache_review_keeps_files_and_never_starts_removal() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let review = known_cache::prepared_fixture_for_ui(&fixture)?;
        let index = review.path.join("index");
        let original = std::fs::read(&index)?;
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.library_panel.review = Some(Ok(review));
        let _ = ctx.run_ui(Default::default(), |_| app.library_confirmation(&ctx));
        assert!(app.library_panel.review.is_some());
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |_| app.library_confirmation(&ctx));
        assert!(app.library_panel.review.is_none());
        assert!(app.job.is_none());
        assert!(app.library_panel.receipt.is_none());
        assert_eq!(std::fs::read(index)?, original);
        Ok(())
    }
}
