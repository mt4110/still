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
        #[cfg(target_os = "macos")]
        let mut row_status = None;
        #[cfg(target_os = "macos")]
        let mut group_check = None;
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
                "この一覧は調査用です。整理できるキャッシュや SDK には、専用の確認操作が表示されます。",
                "This list is for inspection. Supported caches and SDKs have separate review actions.",
            ));
            #[cfg(target_os = "macos")]
            {
                group_check = self.library_panel.group_ui(ui, busy, self.japanese_reasons);
                self.library_panel
                    .assertion_overview(ui, busy, self.japanese_reasons);
            }
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
                                let access = self.library_panel.access(row, index);
                                #[cfg(target_os = "macos")]
                                let title =
                                    format!("{title}{}", access.label(self.japanese_reasons));
                                let title = if row.path.is_some() {
                                    format!(
                                        "{title} · {}",
                                        modified_caption(row.modified_at, self.japanese_reasons)
                                    )
                                } else {
                                    title
                                };
                                let header = egui::CollapsingHeader::new(title).id_salt((
                                    row.path.as_deref(),
                                    &row.label,
                                    index,
                                ));
                                ui.horizontal(|ui| {
                                    #[cfg(target_os = "macos")]
                                    if access.is_locked() {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(14.0, 14.0),
                                            egui::Sense::hover(),
                                        );
                                        icons::paint(ui, rect, icons::Icon::Lock, MUTED);
                                    }
                                    let item = header.show(ui, |ui| {
                                        if let Some(path) = &row.path {
                                            ui.label(path);
                                        }
                                        ui.label(row.reason.text(self.japanese_reasons));
                                        #[cfg(target_os = "macos")]
                                        {
                                            if let Some(evidence) = &row.library {
                                                ui.small(
                                                    evidence.activity_text(self.japanese_reasons),
                                                );
                                                ui.small(
                                                    evidence.name_hints_text(self.japanese_reasons),
                                                );
                                                self.library_panel.uninstall_row(
                                                    ui,
                                                    evidence,
                                                    busy,
                                                    self.japanese_reasons,
                                                );
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
                                    #[cfg(target_os = "macos")]
                                    if let Some(action) = item_menu::show(
                                        &item.header_response,
                                        self.japanese_reasons,
                                        row.native_path.is_some(),
                                        !access.is_locked(),
                                        busy,
                                    ) {
                                        match action {
                                            item_menu::Action::Finder => {
                                                if let Some(path) = &row.native_path {
                                                    if let Err(error) = item_menu::reveal(path) {
                                                        self.status = display_error(
                                                            &error,
                                                            self.japanese_reasons,
                                                        );
                                                    }
                                                }
                                            }
                                            item_menu::Action::Check => {
                                                row_status = Some((
                                                    row.native_path.clone(),
                                                    row.label.clone(),
                                                    row.reason
                                                        .text(self.japanese_reasons)
                                                        .to_owned(),
                                                ))
                                            }
                                            item_menu::Action::Remove => {
                                                if let Some(path) = self.library_panel.target(index)
                                                {
                                                    cache_action =
                                                        Some(mac::Action::Review(path.to_owned()));
                                                }
                                            }
                                        }
                                    }
                                    #[cfg(not(target_os = "macos"))]
                                    let _ = item;
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
        #[cfg(target_os = "macos")]
        if let Some((path, label, detail)) = row_status {
            if let Some(path) = path {
                self.inspect_path_status(ctx, path, detail);
            } else {
                self.inspection = Some(vec![label, detail]);
            }
        }
        #[cfg(target_os = "macos")]
        self.library_panel
            .uninstall_modal(ctx, self.japanese_reasons);
        #[cfg(target_os = "macos")]
        if let Some((app, group)) = group_check {
            self.library_panel.group_result = None;
            self.spawn(
                ctx,
                text(
                    self.japanese_reasons,
                    "アプリの署名と宣言を確認中…",
                    "Checking the app signature and declaration…",
                ),
                false,
                move || Outcome::GroupLink(app_storage::group_link::inspect(&app, &group)),
            );
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
    use crate::review::{
        app_storage::{
            known_cache,
            uninstall_assertions::{Records, Scope},
        },
        cleanup_history,
    };
    use keep::KeepRules;

    #[derive(Default)]
    pub(in crate::review::desktop) struct Panel {
        kept: Option<Result<KeepRules>>,
        cache_row: Option<(usize, PathBuf)>,
        settings_error: Option<String>,
        pub(in crate::review::desktop) review: Option<Result<known_cache::PreparedCache>>,
        pub(in crate::review::desktop) receipt: Option<Result<SavedReceipt>>,
        group_app: Option<PathBuf>,
        group_id: String,
        pub(in crate::review::desktop) group_result:
            Option<Result<app_storage::group_link::Declaration>>,
        assertions: Option<Result<Records>>,
        observed_registered_ids: std::collections::BTreeSet<String>,
        assertion_draft: Option<AssertionDraft>,
        assertion_error: Option<String>,
    }

    struct AssertionDraft {
        id: String,
        registered: bool,
        scope: Option<Scope>,
        acknowledged: bool,
    }

    fn scope_text(scope: Scope, ja: bool) -> &'static str {
        match scope {
            Scope::System => text(ja, "/Applications のみ", "/Applications only"),
            Scope::User => text(ja, "~/Applications のみ", "~/Applications only"),
            Scope::Both => text(
                ja,
                "/Applications と ~/Applications",
                "/Applications and ~/Applications",
            ),
        }
    }

    pub(in crate::review::desktop) struct SavedReceipt {
        record: cleanup_history::Record,
        history_error: Option<String>,
    }

    pub(super) enum Action {
        Keep(PathBuf, bool),
        Review(PathBuf),
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Access {
        Reviewable,
        Kept,
        Unavailable,
        ReadOnly,
    }

    impl Access {
        pub(super) fn is_locked(self) -> bool {
            self != Self::Reviewable
        }
        pub(super) fn label(self, ja: bool) -> &'static str {
            match self {
                Self::Reviewable => text(ja, " · HTTP キャッシュを確認", " · Review HTTP cache"),
                Self::Kept => text(ja, " · 残す設定", " · Kept"),
                Self::Unavailable => text(ja, " · 確認保留", " · Review unavailable"),
                Self::ReadOnly => {
                    text(ja, " · この画面では閲覧のみ", " · View only on this screen")
                }
            }
        }
    }

    impl Panel {
        fn overview_draft(&self, id: String) -> AssertionDraft {
            AssertionDraft {
                registered: self.observed_registered_ids.contains(&id),
                id,
                scope: None,
                acknowledged: false,
            }
        }

        pub(super) fn assertion_overview(&mut self, ui: &mut egui::Ui, busy: bool, ja: bool) {
            if self.assertions.is_none() {
                self.assertions = Some(Records::load());
            }
            let mut revoke: Option<String> = None;
            let mut update: Option<String> = None;
            let mut reload = false;
            ui.collapsing(text(ja, "自分のアンインストール確認記録", "Your uninstall confirmation records"), |ui| {
                ui.small(text(ja, "本人の申告です。OSの検出とは別で、削除可否には使いません。", "Your statements are separate from OS detection and never control deletion."));
                match &self.assertions {
                    Some(Ok(records)) => {
                        if records.entries().next().is_none() { ui.small(text(ja, "記録はありません。", "No records.")); }
                        for (id, record) in records.entries() {
                            let date = record.updated_at.with_timezone(&chrono::Local).format("%Y/%m/%d %H:%M");
                            ui.label(format!("{id} · {} · {} · {date}", scope_text(record.scope, ja), if record.confirmed() { text(ja, "本人確認済み", "Confirmed by you") } else { text(ja, "撤回済み", "Revoked") }));
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!busy, egui::Button::new(text(ja, "再確認…", "Confirm again…"))).clicked() { update = Some(id.to_owned()); }
                                if record.confirmed() && ui.add_enabled(!busy, egui::Button::new(text(ja, "撤回", "Revoke"))).clicked() { revoke = Some(id.to_owned()); }
                            });
                        }
                    }
                    Some(Err(error)) => { ui.label(format!("{} · {error:#}", text(ja, "確認記録を読み込めません", "Could not read confirmations"))); }
                    None => {}
                }
                reload = ui.add_enabled(!busy, egui::Button::new(text(ja, "記録を再読み込み", "Reload records"))).clicked();
                if let Some(error) = &self.assertion_error { ui.label(format!("{} · {error}", text(ja, "記録を更新できません", "Could not update confirmation"))); }
            });
            if reload {
                self.assertions = Some(Records::load());
            }
            if let Some(id) = revoke {
                match Records::revoke(&id) {
                    Ok(records) => {
                        self.assertions = Some(Ok(records));
                        self.assertion_error = None;
                    }
                    Err(error) => self.assertion_error = Some(format!("{error:#}")),
                }
            }
            if let Some(id) = update {
                self.assertion_draft = Some(self.overview_draft(id));
                self.assertion_error = None;
            }
        }

        pub(super) fn group_ui(
            &mut self,
            ui: &mut egui::Ui,
            busy: bool,
            ja: bool,
        ) -> Option<(PathBuf, String)> {
            let mut check = false;
            ui.collapsing(text(ja, "選んだアプリの App Group 宣言を照合…", "Check an app's App Group declaration…"), |ui| {
                ui.label(text(ja, "アプリ1つの署名付き宣言と指定したIDを完全一致で照合します。", "Compare one selected app's signed declaration with an exact group ID."));
                ui.small(text(ja, "署名の整合性は発行元の真正性を証明しません。他の利用元や使用中かどうかも分かりません。同時変更をすべて検出できる保証はありません。", "Signature integrity does not prove publisher identity, other group members, or whether the group is in use. Concurrent changes may not always be detected."));
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.button(text(ja, "アプリを選ぶ…", "Choose app…")).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Application", &["app"]).pick_file() {
                            self.group_app = Some(path);
                            self.group_result = None;
                        }
                    }
                    if let Some(path) = &self.group_app { ui.small(display(path)); }
                    ui.horizontal(|ui| {
                        ui.label(text(ja, "App Group ID", "App Group ID"));
                        if ui.text_edit_singleline(&mut self.group_id).changed() { self.group_result = None; }
                    });
                    check = ui.add_enabled(self.group_app.is_some() && !self.group_id.is_empty(), egui::Button::new(text(ja, "宣言を照合", "Check declaration"))).clicked();
                });
                if let Some(result) = &self.group_result {
                    use app_storage::group_link::Declaration;
                    match result {
                        Ok(Declaration::Match) => { ui.label(text(ja, "このアプリの署名付き宣言に、指定IDの完全一致があります。", "The app's signed declaration contains the exact ID.")); }
                        Ok(Declaration::NoMatch) => { ui.label(text(ja, "このアプリの署名付き宣言に、指定IDはありません。", "The app's signed declaration does not contain the ID.")); }
                        Ok(Declaration::NoGroups) => { ui.label(text(ja, "このアプリの署名に App Groups の宣言はありません。", "The app signature has no App Groups declaration.")); }
                        Err(error) => { ui.label(text(ja, "宣言を確認できませんでした。", "The declaration could not be verified.")); ui.small(format!("{error:#}")); }
                    }
                }
            });
            check.then(|| (self.group_app.clone().unwrap(), self.group_id.clone()))
        }

        pub(in crate::review::desktop) fn scanned(&mut self, result: &Result<app_storage::Report>) {
            self.load_kept();
            self.assertions = Some(Records::load());
            self.observed_registered_ids = result
                .as_ref()
                .ok()
                .into_iter()
                .flat_map(|report| report.rows.iter())
                .filter_map(|row| {
                    let evidence = row.library.as_ref()?;
                    evidence
                        .registered_bundle_observed()
                        .then(|| evidence.bundle_id().map(str::to_owned))
                        .flatten()
                })
                .collect();
            self.cache_row =
                result.as_ref().ok().and_then(|report| {
                    report.rows.iter().enumerate().find_map(|(index, row)| {
                        known_cache::candidate(row).map(|path| (index, path))
                    })
                });
        }

        pub(super) fn uninstall_row(
            &mut self,
            ui: &mut egui::Ui,
            evidence: &app_storage::library::Evidence,
            busy: bool,
            ja: bool,
        ) {
            let Some(id) = evidence.bundle_id() else {
                return;
            };
            ui.add_space(6.);
            ui.label(text(
                ja,
                "アンインストールについての自分の確認",
                "Your uninstall confirmation",
            ));
            let mut revoke = false;
            match &self.assertions {
                Some(Ok(records)) => match records.get(id) {
                    Some(record) => {
                        let timestamp = record
                            .updated_at
                            .with_timezone(&chrono::Local)
                            .format("%Y/%m/%d %H:%M");
                        let scope = scope_text(record.scope, ja);
                        ui.small(if ja {
                            format!(
                                "{} · {scope} · {timestamp}",
                                if record.confirmed() {
                                    "本人が確認済みとして記録"
                                } else {
                                    "本人が確認記録を撤回"
                                }
                            )
                        } else {
                            format!(
                                "{} · {scope} · {timestamp}",
                                if record.confirmed() {
                                    "Confirmed by you"
                                } else {
                                    "Confirmation revoked by you"
                                }
                            )
                        });
                        if record.confirmed() {
                            revoke = ui
                                .add_enabled(
                                    !busy,
                                    egui::Button::new(text(
                                        ja,
                                        "この確認を撤回",
                                        "Revoke confirmation",
                                    )),
                                )
                                .clicked();
                        }
                    }
                    None => {
                        ui.small(text(
                            ja,
                            "本人の確認記録はありません。",
                            "No confirmation from you is recorded.",
                        ));
                    }
                },
                Some(Err(error)) => {
                    ui.small(format!(
                        "{} · {error:#}",
                        text(
                            ja,
                            "確認記録を読み込めません",
                            "Could not read confirmations"
                        )
                    ));
                    return;
                }
                None => return,
            }
            if revoke {
                match Records::revoke(id) {
                    Ok(records) => {
                        self.assertions = Some(Ok(records));
                        self.assertion_error = None;
                    }
                    Err(error) => self.assertion_error = Some(format!("{error:#}")),
                }
            }
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(text(
                        ja,
                        "自分の確認を記録・更新…",
                        "Record or update your confirmation…",
                    )),
                )
                .clicked()
            {
                self.assertion_draft = Some(AssertionDraft {
                    id: id.into(),
                    registered: evidence.registered_bundle_observed(),
                    scope: None,
                    acknowledged: false,
                });
                self.assertion_error = None;
            }
            if let Some(error) = &self.assertion_error {
                ui.small(format!(
                    "{} · {error}",
                    text(ja, "記録を更新できません", "Could not update confirmation")
                ));
            }
        }

        pub(super) fn uninstall_modal(&mut self, ctx: &egui::Context, ja: bool) {
            let Some(draft) = &mut self.assertion_draft else {
                return;
            };
            let mut save = false;
            let mut close = false;
            let modal = egui::Modal::new("uninstall-user-confirmation".into()).show(ctx, |ui| {
                ui.heading(text(ja, "自分の確認を記録", "Record your confirmation"));
                ui.label(format!("Bundle ID: {}", draft.id));
                ui.label(text(ja, "フォルダ名から推定したIDです。対象のアプリとIDを自分で確かめてください。", "This ID comes from the folder name. Verify the app and ID yourself."));
                ui.small(text(ja, "この確認記録からOS登録の有無は分かりません。", "This confirmation does not establish OS registration status."));
                if draft.registered { ui.label(text(ja, "注意: 今回の調査では、このIDの登録済みアプリも観測しています。", "Note: this scan also observed a registered app with this ID.")); }
                ui.label(text(ja, "確認した場所を選んでください。ほかの場所は確認範囲に含みません。", "Choose the locations you checked; other locations are outside this record."));
                for scope in [Scope::System, Scope::User, Scope::Both] {
                    ui.radio_value(&mut draft.scope, Some(scope), scope_text(scope, ja));
                }
                ui.checkbox(&mut draft.acknowledged, text(ja, "このIDのアプリを自分でアンインストールし、選んだ場所を確認しました。", "I uninstalled the app with this ID and checked the selected locations."));
                ui.small(text(ja, "本人の申告として日時と範囲を保存します。OSやstillによる証明ではなく、削除可否や自動選択には使いません。", "This records your statement with its time and scope. It is not proof by the OS or still and never controls deletion or automatic selection."));
                ui.horizontal(|ui| {
                    close = ui.button(text(ja, "キャンセル", "Cancel")).clicked();
                    save = ui.add_enabled(draft.scope.is_some() && draft.acknowledged, egui::Button::new(text(ja, "確認を記録", "Record confirmation"))).clicked();
                });
            });
            if save {
                let draft = self.assertion_draft.take().expect("open confirmation");
                match Records::confirm(&draft.id, draft.scope.expect("scope selected")) {
                    Ok(records) => {
                        self.assertions = Some(Ok(records));
                        self.assertion_error = None;
                    }
                    Err(error) => self.assertion_error = Some(format!("{error:#}")),
                }
            } else if close || modal.should_close() {
                self.assertion_draft = None;
            }
        }

        pub(super) fn target(&self, index: usize) -> Option<&Path> {
            self.cache_row
                .as_ref()
                .filter(|(candidate, _)| *candidate == index)
                .map(|(_, path)| path.as_path())
        }

        pub(super) fn access(&self, row: &app_storage::Row, index: usize) -> Access {
            let candidate = self.target(index);
            let path = candidate.or(row.native_path.as_deref());
            if path.is_some_and(|path| {
                self.kept
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .is_some_and(|rules| rules.conflict(path).is_some())
            }) {
                Access::Kept
            } else if candidate.is_some() {
                if matches!(self.kept, Some(Ok(_))) {
                    Access::Reviewable
                } else {
                    Access::Unavailable
                }
            } else {
                Access::ReadOnly
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

    #[cfg(test)]
    mod confirmation_tests {
        use super::{Access, Panel};
        use crate::review::{app_storage::Row, desktop::keep::KeepRules, message::Message};

        #[test]
        fn access_marks_only_available_cache_as_reviewable() {
            let row = Row {
                label: "Chrome".into(),
                path: None,
                modified_at: None,
                native_path: None,
                size_bytes: Some(10),
                size_basis: "allocated",
                partial: false,
                reason: Message::new("example", "例"),
                library: None,
            };
            let mut panel = Panel::default();
            assert_eq!(panel.access(&row, 0), Access::ReadOnly);
            panel.cache_row = Some((0, "/tmp/example".into()));
            assert_eq!(panel.access(&row, 0), Access::Unavailable);
            panel.kept = Some(Ok(KeepRules::default()));
            assert_eq!(panel.access(&row, 0), Access::Reviewable);
            assert!(!panel.access(&row, 0).is_locked());
            assert!(panel.access(&row, 1).is_locked());
        }
        #[test]
        fn overview_reconfirmation_warns_for_observed_registered_id() {
            let mut panel = Panel::default();
            panel
                .observed_registered_ids
                .insert("com.example.app".into());
            assert!(panel.overview_draft("com.example.app".into()).registered);
            assert!(!panel.overview_draft("com.example.other".into()).registered);
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
