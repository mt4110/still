use super::*;
use crate::review::{
    candidates::DesktopTarget, docker::ImageRemovalReview, worktrees::RemovalReview,
};

pub(super) enum ExternalReview {
    Image(ImageRemovalReview),
    Worktree(RemovalReview),
}

pub(super) struct ExternalReceipt {
    target: DesktopTarget,
    context: Option<String>,
    result: Result<()>,
    before: Option<u64>,
    after: Option<u64>,
}

impl ExternalReview {
    fn target(&self) -> DesktopTarget {
        match self {
            Self::Image(review) => DesktopTarget::Image(review.id().to_owned()),
            Self::Worktree(review) => DesktopTarget::Worktree {
                repo: review.repo.clone(),
                path: review.path.clone(),
            },
        }
    }

    fn apply(self) -> ExternalReceipt {
        let target = self.target();
        let (context, volume) = match &self {
            Self::Image(review) => (
                Some(review.context.clone()),
                directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()),
            ),
            Self::Worktree(review) => (None, review.path.parent().map(Path::to_path_buf)),
        };
        let free = || {
            volume
                .as_deref()
                .and_then(super::super::filesystem::available)
        };
        let before = free();
        let result = match self {
            Self::Image(review) => review.apply(),
            Self::Worktree(review) => review.apply(),
        };
        ExternalReceipt {
            target,
            context,
            result,
            before,
            after: free(),
        }
    }
}

impl App {
    pub(super) fn review_external(&mut self, ctx: &egui::Context, target: DesktopTarget) {
        let Some(scan) = self.scan.as_ref() else {
            return;
        };
        match target {
            DesktopTarget::Image(id) => {
                if let Some(context) = scan.policy.docker_context.clone() {
                    self.review_docker_image(ctx, context, id);
                }
            }
            DesktopTarget::Worktree { repo, path } => {
                let policy = Arc::clone(&scan.policy);
                let base = self
                    .profile
                    .git_base
                    .clone()
                    .unwrap_or_else(|| "refs/heads/main".into());
                self.spawn(
                    ctx,
                    text(
                        self.japanese_reasons,
                        "ワークツリーを再確認中…",
                        "Reviewing worktree…",
                    ),
                    false,
                    move || {
                        Outcome::ExternalReview(
                            RemovalReview::prepare(&repo, &path, &policy.roots, &base)
                                .map(ExternalReview::Worktree),
                        )
                    },
                );
            }
        }
    }

    pub(super) fn review_docker_image(&mut self, ctx: &egui::Context, context: String, id: String) {
        self.spawn(
            ctx,
            text(
                self.japanese_reasons,
                "イメージを再確認中…",
                "Reviewing image…",
            ),
            false,
            move || {
                Outcome::ExternalReview(
                    ImageRemovalReview::prepare(&context, &id).map(ExternalReview::Image),
                )
            },
        );
    }

    pub(super) fn record_external_removal(&mut self, receipt: ExternalReceipt) {
        if receipt.result.is_ok() {
            if let Some(scan) = &mut self.scan {
                if receipt.context.is_none() || scan.policy.docker_context == receipt.context {
                    scan.external
                        .retain(|row| row.desktop_target.as_ref() != Some(&receipt.target));
                    self.rebuild();
                }
            }
            #[cfg(target_os = "macos")]
            if matches!(receipt.target, DesktopTarget::Image(_)) {
                self.monitor.refresh_images();
            }
        }
        self.external_receipt = Some(receipt);
    }

    pub(super) fn external_cleanup_dialog(&mut self, ctx: &egui::Context) {
        let ja = self.japanese_reasons;
        if let Some(Err(error)) = &self.external_review {
            let error = display_error(error, ja);
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("external-review-error"))
                .frame(modal_frame(ctx))
                .show(ctx, |ui| {
                    ui.heading(text(
                        ja,
                        "削除候補を確認できませんでした",
                        "Could not confirm removal candidate",
                    ));
                    ui.add(egui::Label::new(error).wrap());
                    close = ui.button(text(ja, "閉じる", "Close")).clicked();
                });
            if close || dialog.should_close() {
                self.external_review = None;
            }
            return;
        }
        if let Some(Ok(review)) = &self.external_review {
            let mut cancel = false;
            let mut remove = false;
            let dialog = egui::Modal::new(egui::Id::new("external-removal-confirmation"))
                .frame(modal_frame(ctx)).show(ctx, |ui| {
                    ui.set_max_width(600.0);
                    match review {
                        ExternalReview::Image(review) => {
                            ui.heading(text(ja, "この Docker イメージを削除しますか？", "Remove this Docker image?"));
                            ui.label(format!("{}: {}", text(ja, "接続先", "Context"), review.context));
                            ui.add(egui::Label::new(review.id()).wrap());
                            for reference in review.evidence() {
                                ui.add(egui::Label::new(reference).wrap());
                            }
                            for (label, value) in [
                                (text(ja, "作成日時", "Created"), review.created),
                                (text(ja, "最終タグ付け", "Last tagged"), review.last_tagged),
                            ] {
                                if let Some(value) = value {
                                    ui.label(format!("{label}: {}", value.with_timezone(&chrono::Local).format("%Y/%m/%d %H:%M")));
                                }
                            }
                            ui.label(text(ja, "コンテナ参照とタグを再確認します。共有レイヤーが残り、必要になれば再取得が必要です。", "Container references and tags are checked again. Shared layers may remain, and the image may need to be pulled again."));
                        }
                        ExternalReview::Worktree(review) => {
                            ui.heading(text(ja, "このワークツリーを削除しますか？", "Remove this worktree?"));
                            ui.add(egui::Label::new(display(&review.path)).wrap());
                            ui.label(format!("{}: {}", text(ja, "Git で確認した統合先", "Integration ref verified by Git"), review.base));
                            ui.add(egui::Label::new(format!("HEAD: {}", review.head())).wrap());
                            ui.label(text(ja, "統合先に含まれ、変更済み・未追跡・無視対象のファイルがないことを Git で確認しました。実行前に再確認し、Git で作業フォルダと登録を削除します。ブランチは保持します。", "Git verified that the integration ref contains HEAD and that the checkout has no modified, untracked or ignored files. Git checks again before removing the checkout and its registration. The branch is retained."));
                        }
                    }
                    ui.horizontal(|ui| {
                        cancel = ui.button(text(ja, "キャンセル", "Cancel")).clicked();
                        remove = controls::destructive_button(ui, text(ja, "確認した1件を削除", "Remove this reviewed item"), self.job.is_none()).clicked();
                    });
                });
            if cancel || dialog.should_close() {
                self.external_review = None;
            } else if remove {
                if let Some(Ok(review)) = self.external_review.take() {
                    self.spawn(
                        ctx,
                        text(ja, "確認した項目を削除中…", "Removing the reviewed item…"),
                        true,
                        move || Outcome::ExternalRemoval(review.apply()),
                    );
                }
            }
        } else if let Some(receipt) = &self.external_receipt {
            let mut close = false;
            let dialog = egui::Modal::new(egui::Id::new("external-removal-result"))
                .frame(modal_frame(ctx))
                .show(ctx, |ui| {
                    ui.set_max_width(600.0);
                    ui.heading(text(ja, "削除結果", "Removal result"));
                    let name = match &receipt.target {
                        DesktopTarget::Image(id) => id.clone(),
                        DesktopTarget::Worktree { path, .. } => display(path),
                    };
                    ui.add(egui::Label::new(name).wrap());
                    match &receipt.result {
                        Ok(()) => {
                            ui.label(text(
                                ja,
                                "対象が削除されたことを確認しました。",
                                "Removal was verified.",
                            ));
                        }
                        Err(error) => {
                            ui.label(text(
                                ja,
                                "削除の完了を確認できませんでした。",
                                "Removal could not be confirmed.",
                            ));
                            ui.add(egui::Label::new(display_error(error, ja)).wrap());
                        }
                    }
                    if let Some((before, after)) = receipt.before.zip(receipt.after) {
                        ui.label(format!(
                            "{} {}{}",
                            text(ja, "空き容量の変化", "Free-space change"),
                            if after >= before { "+" } else { "−" },
                            bytes(after.abs_diff(before))
                        ));
                    }
                    close = ui.button(text(ja, "閉じる", "Close")).clicked();
                });
            if close || dialog.should_close() {
                self.external_receipt = None;
            }
        }
    }
}
