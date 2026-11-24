use super::{egui, text, App};

impl App {
    pub(super) fn help_dialog(&mut self, ctx: &egui::Context) {
        let japanese = self.japanese_reasons;
        let dialog = egui::Modal::new(egui::Id::new("app-help")).frame(super::modal_frame(ctx)).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 90.0).clamp(580.0, 730.0));
            ui.heading(text(japanese, "still の使い方", "Using still"));
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 210.0).clamp(180.0, 470.0)).show(ui, |ui| {
                for (title, body) in if japanese {
                    [
                        ("毎週の掃除", "開発フォルダを調べると、プロジェクトごとの容量と掃除候補が出ます。候補を選び、理由と対象を確認してから削除します。「別名で保存」で週次プロファイルを作ると、次回から前回との差分も見られます。"),
                        ("保留の理由", "追跡ファイル、使用中のファイル、ソースやログなどが混ざる場合は保留します。行を選んで具体的な理由を確認できます。"),
                        ("ディスクと Docker", "サイドバーの「ディスク」で空き容量と読み書き速度を確認できます。「Docker」ではローカル接続先のコンテナ、イメージ、ボリューム、ビルドキャッシュを容量順に確認できます。"),
                        ("古いファイルと SDK", "サイドバーから古いファイルやアプリの保存容量を調べられます。Xcode と Android NDK は、版ごとに利用状況を確認して選んだ分をアンインストールできます。"),
                        ("削除と回収量", "生成物の削除と SDK のアンインストールは確認画面の実行ボタンで始まります。Mac では生成物をゴミ箱へ移動するか、完全消去するかを選べます。容量の実測値には、同時に動く他のアプリの影響も含まれます。"),
                        ("プライバシーと権限", "テレメトリーや利用分析の自動送信はありません。プロファイルと履歴はこの端末に保存します。Mac のファイル調査はホーム配下で行い、OS のアクセス許可に従います。ディスク画面はそのボリュームの物理ディスク統計、Docker は選んだローカル接続先のメタデータを読み取ります。"),
                    ]
                } else {
                    [
                        ("Weekly cleanup", "Scan a development folder to see project sizes and cleanup candidates. Select candidates and review their reasons before confirming deletion. Save a weekly profile to compare future scans."),
                        ("Why an item is held", "Tracked files, open files, source data and protected logs can hold a candidate. Select the row to read its specific reason."),
                        ("Disk and Docker", "Open Disk in the sidebar to see capacity and read/write activity. Docker lists local containers, images, volumes and build cache by reported size."),
                        ("Old files and SDKs", "The sidebar opens old-file discovery and app storage. Review Xcode and Android NDK versions individually before confirming their uninstall."),
                        ("Deletion and recovery", "The final confirmation starts deletion or SDK uninstall. On Mac, you can move generated outputs to Trash or delete them permanently. Measured free-space changes also include other apps running at the same time."),
                        ("Privacy and access", "No automatic telemetry or usage analytics. Profiles and history stay on this device. Mac file scans stay under home and follow OS permissions. Disk activity reads physical-device counters for that volume. Docker reads metadata from the selected local context."),
                    ]
                } {
                    ui.strong(title);
                    ui.label(body);
                    ui.add_space(12.0);
                }
            });
            ui.separator();
            ui.small(format!("still {}", env!("CARGO_PKG_VERSION")));
            ui.button(text(japanese, "閉じる", "Close")).clicked()
        });
        if dialog.inner || dialog.should_close() {
            self.show_help = false;
        }
    }
}
