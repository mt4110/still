use super::*;

pub(super) enum Action {
    #[cfg(target_os = "macos")]
    Finder,
    Remove,
    Check,
}

pub(super) fn show(
    response: &egui::Response,
    ja: bool,
    has_path: bool,
    can_remove: bool,
    busy: bool,
) -> Option<Action> {
    let mut action = None;
    response.context_menu(|ui| {
        #[cfg(target_os = "macos")]
        if has_path
            && ui
                .button(text(ja, "Finder で表示", "Show in Finder"))
                .clicked()
        {
            action = Some(Action::Finder);
            ui.close();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = has_path;
        if ui
            .add_enabled(
                can_remove && !busy,
                egui::Button::new(text(ja, "削除を確認…", "Review removal…")),
            )
            .on_disabled_hover_text(text(
                ja,
                "この項目は削除対象にできません。状態を確認してください。",
                "This item cannot be removed here. Check its status.",
            ))
            .clicked()
        {
            action = Some(Action::Remove);
            ui.close();
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(text(ja, "状態をチェック", "Check status")),
            )
            .clicked()
        {
            action = Some(Action::Check);
            ui.close();
        }
    });
    action
}

#[cfg(target_os = "macos")]
pub(super) fn reveal(path: &Path) -> Result<()> {
    anyhow::ensure!(path.is_absolute(), "Expected an absolute path");
    std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .status()?
            .success(),
        "Finder could not show {}",
        display(path)
    );
    Ok(())
}

impl App {
    pub(super) fn inspect_path_status(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        detail: String,
    ) {
        let ja = self.japanese_reasons;
        self.spawn(
            ctx,
            text(ja, "項目の状態を確認中…", "Checking item status…"),
            false,
            move || {
                let mut lines = vec![display(&path)];
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => {
                        let kind = if inventory::is_link(&metadata) {
                            text(ja, "リンク", "Link")
                        } else if metadata.is_dir() {
                            text(ja, "フォルダ", "Folder")
                        } else {
                            text(ja, "ファイル", "File")
                        };
                        lines.push(format!("{} · {kind}", text(ja, "存在を確認", "Present")));
                        if metadata.is_file() {
                            lines.push(bytes(metadata.len()));
                        }
                        if let Ok(time) = metadata.modified() {
                            let time = chrono::DateTime::<chrono::Local>::from(time);
                            lines.push(format!(
                                "{} {}",
                                text(ja, "最終更新", "Last modified"),
                                time.format("%Y/%m/%d %H:%M")
                            ));
                        }
                    }
                    Err(error) => lines.push(format!(
                        "{} · {error}",
                        text(
                            ja,
                            "現在の状態を確認できません",
                            "Current status unavailable"
                        )
                    )),
                }
                if !detail.is_empty() {
                    lines.push(format!(
                        "{} · {detail}",
                        text(ja, "調査時の判定", "Scan result")
                    ));
                }
                Outcome::Inspection(lines)
            },
        );
    }
}
