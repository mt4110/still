use super::*;
use crate::review::desktop::{controls, modal_frame, Outcome};
use chrono::{DateTime, Local, TimeDelta, Utc};
use egui::{Align, Color32, RichText, Sense, Stroke};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Order {
    #[default]
    Stopped,
    Created,
    ImageLastStart,
    Size,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Filter {
    #[default]
    All,
    Reviewable,
    Stopped,
    Running,
}

enum Action {
    None,
    Scan,
    Review,
    ReviewCache,
    ReviewImage(String),
    ReviewContainer(String),
    ReviewCacheItem(String),
    CheckResource(storage::Kind, String, String),
}

fn activity(item: &storage::Resource) -> Option<DateTime<Utc>> {
    match item.status.as_str() {
        "exited" => item.finished,
        "created" => item.created,
        _ => None,
    }
}

fn date(value: Option<DateTime<Utc>>, japanese: bool) -> String {
    value
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| text(japanese, "不明", "Unknown").into())
}

fn docker_size(value: &str) -> String {
    storage::size_bytes(value)
        .map(bytes)
        .unwrap_or_else(|| value.to_owned())
}

fn relative_time(value: &str, japanese: bool) -> String {
    if !japanese {
        return value.to_owned();
    }
    if value == "Just now" || value == "just now" {
        return "たった今".into();
    }
    if let Some(translated) = match value.to_ascii_lowercase().as_str() {
        "less than a second ago" => Some("1秒未満前"),
        "about a minute ago" => Some("約1分前"),
        "about an hour ago" => Some("約1時間前"),
        "about a day ago" => Some("約1日前"),
        "about a week ago" => Some("約1週間前"),
        "about a month ago" => Some("約1か月前"),
        "about a year ago" => Some("約1年前"),
        _ => None,
    } {
        return translated.into();
    }
    let parts: Vec<_> = value.split_whitespace().collect();
    if let [number, unit, "ago"] = parts.as_slice() {
        if number.parse::<u64>().is_ok() {
            let unit = match *unit {
                "second" | "seconds" => "秒",
                "minute" | "minutes" => "分",
                "hour" | "hours" => "時間",
                "day" | "days" => "日",
                "week" | "weeks" => "週間",
                "month" | "months" => "か月",
                "year" | "years" => "年",
                _ => return value.to_owned(),
            };
            return format!("{number}{unit}前");
        }
    }
    value.to_owned()
}

fn name(item: &storage::Resource) -> String {
    if item.name.trim().is_empty() {
        item.id.chars().take(12).collect()
    } else {
        item.name.clone()
    }
}

fn reason(kind: storage::Kind, item: &storage::Resource, japanese: bool) -> &str {
    match kind {
        storage::Kind::Container => item.reason.text(japanese),
        storage::Kind::Image => text(
            japanese,
            "参照と共有レイヤーを表示します。選んだイメージが削除できるか個別に確認できます。",
            "Review references and shared layers. Check whether the selected image can be removed.",
        ),
        storage::Kind::Volume => text(
            japanese,
            "永続データを保持する可能性があるため保護しています。",
            "Protected because it may contain persistent application data.",
        ),
        storage::Kind::Cache => text(
            japanese,
            "容量を確認できます。削除する項目は「未使用キャッシュを確認…」から選べます。",
            "Review usage here. Open Review unused cache to select records for deletion.",
        ),
    }
}

fn matches(
    item: &storage::Resource,
    query: &str,
    filter: Filter,
    days: u16,
    now: DateTime<Utc>,
) -> bool {
    let status = match filter {
        Filter::All => true,
        Filter::Reviewable => item.eligible,
        Filter::Stopped => matches!(item.status.as_str(), "exited" | "created"),
        Filter::Running => matches!(item.status.as_str(), "running" | "paused" | "restarting"),
    };
    let query = query.to_lowercase();
    status
        && (query.is_empty()
            || [&item.name, &item.id, &item.status, &item.detail]
                .iter()
                .any(|field| field.to_lowercase().contains(&query)))
        && (days == 0
            || activity(item).is_some_and(|time| time <= now - TimeDelta::days(i64::from(days))))
}

fn ordered(a: &storage::Resource, b: &storage::Resource, order: Order) -> std::cmp::Ordering {
    fn dates<T: Ord>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
        match (a, b) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    }
    match order {
        Order::Stopped => dates(activity(a), activity(b)),
        Order::Created => dates(a.created.or(a.image_created), b.created.or(b.image_created)),
        Order::ImageLastStart => dates(
            a.image_last_start.map(std::cmp::Reverse),
            b.image_last_start.map(std::cmp::Reverse),
        ),
        Order::Size => b
            .estimated_bytes
            .or_else(|| storage::size_bytes(&b.size))
            .cmp(&a.estimated_bytes.or_else(|| storage::size_bytes(&a.size))),
    }
    .then_with(|| a.name.cmp(&b.name))
    .then_with(|| a.id.cmp(&b.id))
}

fn selection_size(
    items: &[storage::Resource],
    marked: &BTreeSet<String>,
    japanese: bool,
) -> String {
    let mut total = 0u64;
    let mut unknown = 0;
    for item in items.iter().filter(|item| marked.contains(&item.id)) {
        match item.estimated_bytes {
            Some(size) => total = total.saturating_add(size),
            None => unknown += 1,
        }
    }
    if unknown == 0 {
        bytes(total)
    } else if japanese {
        format!("{} ＋ 不明 {unknown} 件", bytes(total))
    } else {
        format!("{} + {unknown} unknown", bytes(total))
    }
}

pub(super) struct CacheOutputRow {
    id: String,
    reclaimable: String,
    size: String,
    last_accessed: String,
}

fn parse_cache_output(output: &str) -> (Vec<CacheOutputRow>, Option<String>, bool) {
    let mut rows = Vec::new();
    let mut total = None;
    let mut header = false;
    let mut unrecognized = false;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.split_whitespace().collect::<Vec<_>>()
            == ["ID", "RECLAIMABLE", "SIZE", "LAST", "ACCESSED"]
        {
            header = true;
        } else if let Some(value) = line.strip_prefix("Total:") {
            total = Some(value.trim().to_owned());
        } else if header {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() >= 4 && matches!(parts[1], "true" | "false") {
                rows.push(CacheOutputRow {
                    id: parts[0].to_owned(),
                    reclaimable: parts[1].to_owned(),
                    size: parts[2].to_owned(),
                    last_accessed: parts[3..].join(" "),
                });
            } else {
                unrecognized = true;
            }
        } else {
            unrecognized = true;
        }
    }
    (rows, total, unrecognized)
}

type ParsedOutput = (Vec<CacheOutputRow>, Option<String>, bool);
pub(super) struct CacheReceipt {
    raw: storage::CacheCleanupResult,
    parsed: ParsedOutput,
}
fn cache_output_table(ui: &mut egui::Ui, output: &str, parsed: &ParsedOutput, japanese: bool) {
    let (rows, total, unrecognized) = parsed;
    if !rows.is_empty() {
        ui.label(if japanese {
            format!("Docker が報告した項目: {} 件", rows.len())
        } else {
            format!("{} records reported by Docker", rows.len())
        });
        ui.small(text(
            japanese,
            "ID　／　回収可能　／　容量　／　最終使用",
            "ID   /   Reclaimable   /   Size   /   Last used",
        ));
        egui::ScrollArea::both()
            .id_salt("docker-cache-result-table")
            .max_height(220.0)
            .show_rows(
                ui,
                ui.text_style_height(&egui::TextStyle::Body),
                rows.len(),
                |ui, range| {
                    egui::Grid::new("docker-cache-result-grid")
                        .min_col_width(68.0)
                        .striped(true)
                        .spacing([14.0, ui.spacing().item_spacing.y])
                        .show(ui, |ui| {
                            for row in &rows[range] {
                                ui.monospace(&row.id);
                                ui.label(if row.reclaimable == "true" {
                                    text(japanese, "はい", "Yes")
                                } else {
                                    text(japanese, "いいえ", "No")
                                });
                                ui.label(docker_size(&row.size));
                                ui.label(relative_time(&row.last_accessed, japanese));
                                ui.end_row();
                            }
                        });
                },
            );
    }
    if let Some(total) = &total {
        ui.label(format!(
            "{}: {}",
            text(japanese, "Docker の報告容量", "Docker reported"),
            docker_size(total)
        ));
    }
    if *unrecognized || (rows.is_empty() && total.is_none() && !output.trim().is_empty()) {
        ui.label(text(
            japanese,
            "一部の出力を表にできませんでした。元の出力を確認してください。",
            "Some output could not be shown as a table; inspect the original output.",
        ));
    }
    if !output.trim().is_empty() {
        ui.collapsing(
            text(japanese, "Docker の元の出力", "Original Docker output"),
            |ui| {
                egui::ScrollArea::both()
                    .id_salt("docker-cache-raw-output")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.monospace(output);
                    });
            },
        );
    }
}

impl Panel {
    pub(in crate::review::desktop) fn refresh_images(&mut self) {
        self.selected = None;
        self.refresh_pending = true;
    }
    pub(in crate::review::desktop) fn set_cache_review(
        &mut self,
        result: Result<storage::CacheCleanup>,
    ) {
        match result {
            Ok(review) => {
                self.cache_review = Some(review);
                self.docker_error.clear();
            }
            Err(error) => self.docker_error = format!("Cache review unavailable: {error:#}"),
        }
    }

    pub(in crate::review::desktop) fn set_cache_receipt(
        &mut self,
        receipt: storage::CacheCleanupResult,
    ) {
        let parsed = receipt
            .result
            .as_deref()
            .map(parse_cache_output)
            .unwrap_or_default();
        self.cache_receipt = Some(CacheReceipt {
            raw: receipt,
            parsed,
        });
        self.refresh_pending = true;
    }
    pub(in crate::review::desktop) fn set_review(&mut self, result: Result<storage::Review>) {
        match result {
            Ok(review) => { self.review = Some(review); self.docker_error.clear(); }
            Err(error) => self.docker_error = format!("選択内容を確認できませんでした。再スキャンして確認してください。 / Could not verify selection. {error:#}"),
        }
    }

    pub(in crate::review::desktop) fn set_receipt(&mut self, receipt: storage::Receipt) {
        self.receipt = Some(receipt);
        self.marked.clear();
        self.selected = None;
        self.docker = None;
        self.refresh_pending = true;
    }

    fn docker_view(&mut self, ui: &mut egui::Ui, japanese: bool, global_busy: bool) -> Action {
        let busy = global_busy
            || self.docker_job.is_some()
            || self.review.is_some()
            || self.cache_review.is_some();
        let mut action = Action::None;
        ui.horizontal(|ui| {
            ui.heading("Docker");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if controls::button(ui, text(japanese, "スキャン", "Scan"), true, !busy).clicked()
                {
                    action = Action::Scan;
                }
            });
        });
        ui.label(
            RichText::new(text(
                japanese,
                "容量を確認し、コンテナや未使用ビルドキャッシュを整理します。",
                "Review storage and clean up containers or unused build cache.",
            ))
            .color(MUTED),
        );
        ui.add_space(12.0);
        let mut context_changed = false;
        ui.horizontal(|ui| {
            ui.label(text(japanese, "ローカル接続先", "Local context"));
            if self.contexts.is_none() && self.context_job.is_none() {
                self.load_contexts(ui.ctx());
            }
            let current = text(japanese, "現在の接続先", "Current context");
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("docker-context-selection")
                    .selected_text(if self.context.is_empty() {
                        current
                    } else {
                        &self.context
                    })
                    .show_ui(ui, |ui| {
                        context_changed |= ui
                            .selectable_value(&mut self.context, String::new(), current)
                            .changed();
                        if let Some(Ok(names)) = &self.contexts {
                            for name in names {
                                context_changed |= ui
                                    .selectable_value(&mut self.context, name.clone(), name)
                                    .changed();
                            }
                        }
                    });
                if ui
                    .add_enabled(
                        self.context_job.is_none(),
                        egui::Button::new(text(japanese, "接続先を更新", "Refresh contexts")),
                    )
                    .clicked()
                {
                    self.load_contexts(ui.ctx());
                }
            });
            if self.docker_job.is_some() {
                ui.spinner();
                ui.label(text(japanese, "調査中…", "Inspecting…"));
            }
        });
        if let Some(Err(error)) = &self.contexts {
            ui.small(format!(
                "{} · {error:#}",
                text(
                    japanese,
                    "接続先を取得できません",
                    "Could not list contexts"
                )
            ));
        }
        if context_changed {
            self.docker = None;
            self.marked.clear();
            self.selected = None;
            self.receipt = None;
            self.cache_receipt = None;
            self.docker_error.clear();
        }
        let mut dismiss_cache_receipt = false;
        if let Some(result) = &self.cache_receipt {
            let receipt = &result.raw;
            ui.group(|ui| {
                if let Some(error) = &receipt.history_error {
                    ui.label(format!(
                        "{} · {error}",
                        text(
                            japanese,
                            "整理履歴を保存できませんでした",
                            "Could not save cleanup history"
                        )
                    ));
                }
                ui.horizontal(|ui| {
                    ui.strong(text(
                        japanese,
                        "ビルドキャッシュ整理の結果",
                        "Build cache cleanup result",
                    ));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        dismiss_cache_receipt =
                            ui.button(text(japanese, "閉じる", "Close")).clicked();
                    });
                });
                ui.label(format!(
                    "{} · {} {}",
                    receipt.context,
                    receipt.days,
                    text(japanese, "日以上", "days or older")
                ));
                if let Some(remaining) = receipt.remaining {
                    ui.label(if japanese {
                        format!(
                            "選択 {} 件のうち、再確認時に残っていた項目: {} 件",
                            receipt.selected, remaining
                        )
                    } else {
                        format!(
                            "Still listed after recheck: {remaining} of {} selected",
                            receipt.selected
                        )
                    });
                } else if receipt.attempted {
                    ui.label(text(
                        japanese,
                        "削除後の一覧を確認できませんでした。",
                        "Could not verify the inventory after the deletion attempt.",
                    ));
                }
                match &receipt.result {
                    Ok(output) => cache_output_table(ui, output, &result.parsed, japanese),
                    Err(error) => {
                        let label = if receipt.attempted {
                            text(
                                japanese,
                                "Docker の削除処理が完了しませんでした",
                                "Docker did not complete the deletion",
                            )
                        } else {
                            text(
                                japanese,
                                "削除を開始できませんでした。対象を確認し直してください",
                                "Deletion did not start. Review the candidates again",
                            )
                        };
                        ui.label(format!("{label} · {error:#}"));
                    }
                }
                ui.small(vm_space_note(japanese));
                if let (Some(before), Some(after)) = (receipt.before, receipt.after) {
                    ui.label(format!(
                        "{} {}{}",
                        text(japanese, "Mac の空き容量変化", "Mac free-space change"),
                        if after >= before { "+" } else { "−" },
                        bytes(after.abs_diff(before))
                    ));
                }
            });
        }
        if dismiss_cache_receipt {
            self.cache_receipt = None;
        }
        if let Some(receipt) = &self.receipt {
            show_receipt(
                ui,
                receipt,
                &self.receipt_context,
                &self.receipt_names,
                japanese,
            );
            ui.add_space(12.0);
            if controls::button(
                ui,
                text(japanese, "最新の候補一覧へ", "Back to current candidates"),
                true,
                !busy,
            )
            .clicked()
            {
                self.receipt = None;
            }
            return action;
        }
        if !self.docker_error.is_empty() {
            ui.label(RichText::new(&self.docker_error).color(Color32::from_rgb(170, 50, 42)));
        }
        if self.observation_pending() && self.docker_job.is_none() {
            ui.small(text(
                japanese,
                "容量の前回比較を記録中…",
                "Recording storage comparison…",
            ));
        }
        if let Some(Err(error)) = &self.observation_result {
            ui.label(format!("{} · {error:#}", text(japanese,
                "前回比較を記録できませんでした。概要には以前の記録が残る場合があります",
                "Could not record the comparison. The overview may still show an older observation")));
        }
        let Some(result) = &self.docker else {
            if !busy {
                ui.add_space(28.0);
                ui.strong(text(
                    japanese,
                    "Docker の保存容量を調べる",
                    "Inspect Docker storage",
                ));
                ui.label(text(
                    japanese,
                    "Docker Desktop または Colima を起動し、スキャンを押してください。",
                    "Start Docker Desktop or Colima, then scan.",
                ));
            }
            return action;
        };
        let report = match result {
            Ok(report) => Arc::clone(report),
            Err(error) => {
                ui.add_space(12.0);
                ui.strong(text(
                    japanese,
                    "接続先を確認してください",
                    "Check the connection",
                ));
                ui.label(format!("{error:#}"));
                return action;
            }
        };
        ui.add_space(8.0);
        ui.small(format!(
            "{} · {} · {}",
            text(japanese, "スキャンした接続先", "Inspected context"),
            report.context,
            report.observed_at.format("%Y/%m/%d %H:%M")
        ));
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for (index, (kind, items)) in report.groups.iter().enumerate() {
                if ui
                    .selectable_value(
                        &mut self.kind,
                        index,
                        format!("{} {} / {}", super::kind_label(*kind, japanese), items.len(), group_size(items, japanese)),
                    )
                    .on_hover_text(text(japanese, "Docker の各行の報告容量の合計。共有レイヤーを含み、回収できる量とは異なります。", "Sum of reported sizes, including shared layers. Reclaimable space may differ."))
                    .changed()
                {
                    self.marked.clear();
                    self.selected = None;
                    self.filter = Filter::All;
                    self.sort = if *kind == storage::Kind::Container {
                        Order::Stopped
                    } else {
                        Order::Size
                    };
                }
            }
        });
        let (kind, items) = &report.groups[self.kind];
        let container = *kind == storage::Kind::Container;
        if *kind == storage::Kind::Cache {
            if self.cache_days == 0 {
                self.cache_days = 7;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(text(japanese, "未使用期間", "Unused for"));
                ui.add_enabled(
                    !busy,
                    egui::DragValue::new(&mut self.cache_days).range(1..=3650),
                );
                ui.label(text(japanese, "日以上", "days or more"));
                if controls::button(
                    ui,
                    text(japanese, "未使用キャッシュを確認…", "Review unused cache…"),
                    true,
                    !busy,
                )
                .clicked()
                {
                    action = Action::ReviewCache;
                }
            });
            ui.small(text(
                japanese,
                "既定ビルダーの削除候補を確認して、ID ごとに選べます。",
                "Review the default builder’s candidates and choose which IDs to delete.",
            ));
        }
        let previous = (self.query.clone(), self.filter, self.sort, self.age_days);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    controls::field(ui, &mut self.query)
                        .desired_width(250.0)
                        .hint_text(text(
                            japanese,
                            "名前・ID・状態で絞り込み",
                            "Filter name, ID or state",
                        )),
                );
                egui::ComboBox::from_id_salt("docker-sort")
                    .selected_text(match self.sort {
                        Order::Stopped => text(japanese, "最終停止が古い順", "Oldest last stop"),
                        Order::Created => text(japanese, "作成が古い順", "Oldest created"),
                        Order::ImageLastStart => {
                            text(japanese, "最終起動が新しい順", "Latest container start")
                        }
                        Order::Size => text(japanese, "容量が大きい順", "Largest size"),
                    })
                    .show_ui(ui, |ui| {
                        if container {
                            ui.selectable_value(
                                &mut self.sort,
                                Order::Stopped,
                                text(japanese, "最終停止が古い順", "Oldest last stop"),
                            );
                            ui.selectable_value(
                                &mut self.sort,
                                Order::Created,
                                text(japanese, "作成が古い順", "Oldest created"),
                            );
                        }
                        if *kind == storage::Kind::Image {
                            ui.selectable_value(
                                &mut self.sort,
                                Order::ImageLastStart,
                                text(japanese, "最終起動が新しい順", "Latest container start"),
                            );
                            ui.selectable_value(
                                &mut self.sort,
                                Order::Created,
                                text(japanese, "作成が古い順", "Oldest created"),
                            );
                        }
                        ui.selectable_value(
                            &mut self.sort,
                            Order::Size,
                            text(japanese, "容量が大きい順", "Largest size"),
                        );
                    });
                if container {
                    egui::ComboBox::from_id_salt("docker-status")
                        .selected_text(match self.filter {
                            Filter::All => text(japanese, "すべて", "All"),
                            Filter::Reviewable => text(japanese, "確認可能な候補", "Reviewable"),
                            Filter::Stopped => {
                                text(japanese, "停止・作成済み", "Stopped / created")
                            }
                            Filter::Running => text(japanese, "実行中など", "Active"),
                        })
                        .show_ui(ui, |ui| {
                            for (value, ja, en) in [
                                (Filter::All, "すべて", "All"),
                                (Filter::Reviewable, "確認可能な候補", "Reviewable"),
                                (Filter::Stopped, "停止・作成済み", "Stopped / created"),
                                (Filter::Running, "実行中など", "Active"),
                            ] {
                                ui.selectable_value(
                                    &mut self.filter,
                                    value,
                                    text(japanese, ja, en),
                                );
                            }
                        });
                }
            });
            if container {
                ui.horizontal(|ui| {
                    ui.label(text(
                        japanese,
                        "最終停止／作成から",
                        "Stopped / created at least",
                    ));
                    ui.add(egui::DragValue::new(&mut self.age_days).range(0..=3650));
                    ui.label(text(
                        japanese,
                        "日以上（0 は指定なし）",
                        "days ago (0 = any)",
                    ));
                });
            }
        });
        if previous != (self.query.clone(), self.filter, self.sort, self.age_days) {
            self.marked.clear();
        }
        let now = Utc::now();
        let mut visible: Vec<_> = items
            .iter()
            .filter(|item| {
                matches(
                    item,
                    &self.query,
                    if container { self.filter } else { Filter::All },
                    if container { self.age_days } else { 0 },
                    now,
                )
            })
            .collect();
        visible.sort_by(|a, b| ordered(a, b, self.sort));
        ui.horizontal(|ui| {
            ui.small(if japanese {
                format!("表示 {} 件 / {} 件", visible.len(), items.len())
            } else {
                format!("{} of {} shown", visible.len(), items.len())
            });
            if container {
                if controls::button(
                    ui,
                    text(japanese, "表示中の候補を選択", "Select visible candidates"),
                    false,
                    !busy && visible.iter().any(|i| i.eligible),
                )
                .clicked()
                {
                    self.marked = visible
                        .iter()
                        .filter(|i| i.eligible)
                        .map(|i| i.id.clone())
                        .collect();
                }
                if controls::button(
                    ui,
                    text(japanese, "選択解除", "Clear selection"),
                    false,
                    !busy && !self.marked.is_empty(),
                )
                .clicked()
                {
                    self.marked.clear();
                }
            }
        });
        ui.add_space(8.0);
        if container {
            egui::Panel::bottom("docker-selection-footer").resizable(false).show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0,12))).show_inside(ui, |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(if japanese { format!("{} 件を選択 · 書込層の見込み {}", self.marked.len(), selection_size(items,&self.marked,japanese)) } else { format!("{} selected · estimated writable layer {}",self.marked.len(),selection_size(items,&self.marked,japanese)) });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if controls::button(ui,text(japanese,"選択した項目を確認","Review selected"),true,container && !busy && !self.marked.is_empty()).clicked() { action = Action::Review; }
                    });
                });
                ui.small(text(japanese,"Mac の空き容量とは異なる見込みです。実行前に対象を再確認します。","Estimates differ from Mac free space. Selected resources are verified again before removal."));
            });
        }
        let wide = ui.available_width() >= 900.0;
        let focused = self
            .selected
            .as_ref()
            .and_then(|key| {
                visible
                    .iter()
                    .find(|i| key == &(i.id.clone(), i.name.clone()))
            })
            .copied()
            .or_else(|| visible.first().copied());
        if *kind == storage::Kind::Image {
            self.selected = focused.map(|item| (item.id.clone(), item.name.clone()));
            if let Some(item) = focused {
                ui.horizontal_wrapped(|ui| {
                    if controls::button(
                        ui,
                        text(
                            japanese,
                            "選んだイメージの削除を確認…",
                            "Review selected image for removal…",
                        ),
                        true,
                        !busy,
                    )
                    .clicked()
                    {
                        action = Action::ReviewImage(item.id.clone());
                    }
                    ui.label(name(item)).on_hover_text(&item.id);
                });
            }
        }
        if wide {
            egui::Panel::right("docker-inspector")
                .exact_size(280.0)
                .resizable(false)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("docker-inspector-scroll")
                        .show(ui, |ui| inspector(ui, *kind, focused, japanese));
                });
        }
        let reserve = if !wide && focused.is_some() {
            105.0
        } else {
            0.0
        };
        let table_height = (ui.available_height() - reserve).max(90.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), table_height),
            egui::Layout::top_down(Align::Min),
            |ui| {
                let row_action = rows(
                    ui,
                    *kind,
                    &visible,
                    &mut self.marked,
                    &mut self.selected,
                    japanese,
                    !busy,
                );
                if !matches!(row_action, Action::None) {
                    action = row_action;
                }
            },
        );
        if !wide {
            ui.collapsing(
                text(
                    japanese,
                    "選んだ項目の理由・日時・保存先",
                    "Selected item details",
                ),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("docker-compact-inspector")
                        .max_height(110.0)
                        .show(ui, |ui| inspector(ui, *kind, focused, japanese));
                },
            );
        }
        action
    }
}

fn rows(
    ui: &mut egui::Ui,
    kind: storage::Kind,
    items: &[&storage::Resource],
    marked: &mut BTreeSet<String>,
    selected: &mut Option<(String, String)>,
    japanese: bool,
    enabled: bool,
) -> Action {
    let mut action = Action::None;
    let height = ui.available_height();
    let select_width = if kind == storage::Kind::Container {
        36.0
    } else {
        0.0
    };
    let widths = [
        select_width,
        (ui.available_width() - 16.0 - select_width - 90.0 - 148.0 - 72.0 - 32.0).max(80.0),
        90.0,
        148.0,
        72.0,
    ];
    let (header, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::hover());
    let mut header = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(header.shrink2(egui::vec2(8.0, 0.0)))
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    for (index, label) in [
        "",
        text(japanese, "名前", "Name"),
        if kind == storage::Kind::Container {
            text(japanese, "書込層", "Writable")
        } else {
            text(japanese, "容量", "Size")
        },
        if kind == storage::Kind::Container {
            text(japanese, "最終停止／作成", "Stopped / created")
        } else if kind == storage::Kind::Cache {
            text(japanese, "最終使用", "Last used")
        } else if kind == storage::Kind::Image {
            text(japanese, "最終起動／作成", "Last start / created")
        } else {
            text(japanese, "参照・共有", "References / shared")
        },
        text(japanese, "状態", "State"),
    ]
    .iter()
    .enumerate()
    {
        if widths[index] == 0.0 {
            continue;
        }
        controls::cell(
            &mut header,
            widths[index],
            28.0,
            if index == 2 { Align::Max } else { Align::Min },
            |ui| {
                controls::label(
                    ui,
                    label,
                    12.0,
                    MUTED,
                    if index == 2 { Align::Max } else { Align::Min },
                    Sense::hover(),
                );
            },
        );
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("docker-resource-rows")
        .auto_shrink([false, false])
        .max_height((height - 28.0).max(58.0))
        .show_rows(ui, 58.0, items.len(), |ui, range| {
            for index in range {
                let item = items[index];
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 58.0), Sense::click());
                let focused = selected
                    .as_ref()
                    .is_some_and(|key| key == &(item.id.clone(), item.name.clone()));
                let amount = ui.ctx().animate_bool_with_time(
                    egui::Id::new(("docker-row", &item.id, &item.name)),
                    focused || marked.contains(&item.id) || response.hovered(),
                    ui.style().animation_time.min(0.12),
                );
                ui.painter().rect_filled(
                    rect,
                    5,
                    Color32::from_rgba_unmultiplied(
                        ACCENT.r(),
                        ACCENT.g(),
                        ACCENT.b(),
                        (amount * 25.0) as u8,
                    ),
                );
                ui.painter().hline(
                    rect.x_range(),
                    rect.bottom() - 0.5,
                    Stroke::new(1.0_f32, LINE),
                );
                let mut row = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt((&item.id, &item.name, "docker-cells"))
                        .max_rect(rect.shrink2(egui::vec2(8.0, 6.0)))
                        .layout(egui::Layout::left_to_right(Align::Center)),
                );
                row.set_clip_rect(row.clip_rect().intersect(rect));
                if kind == storage::Kind::Container {
                    controls::cell(&mut row, widths[0], 36.0, Align::Min, |ui| {
                        if kind != storage::Kind::Container {
                            return;
                        }
                        let mut checked = marked.contains(&item.id);
                        let checkbox = ui.add_enabled(
                            enabled && kind == storage::Kind::Container && item.eligible,
                            egui::Checkbox::without_text(&mut checked),
                        );
                        checkbox.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::Checkbox,
                                enabled && item.eligible,
                                checked,
                                format!(
                                    "{} {}",
                                    text(japanese, "削除確認に含める", "Include in removal review"),
                                    name(item)
                                ),
                            )
                        });
                        if checkbox.changed() {
                            if checked {
                                marked.insert(item.id.clone());
                            } else {
                                marked.remove(&item.id);
                            }
                        }
                    });
                }
                let name_response = controls::cell(&mut row, widths[1], 44.0, Align::Min, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(widths[1]);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let title = if kind == storage::Kind::Cache {
                            cache_label(item, japanese).to_owned()
                        } else {
                            name(item)
                        };
                        let response = controls::label(
                            ui,
                            &title,
                            15.0,
                            super::super::INK,
                            Align::Min,
                            Sense::click(),
                        );
                        if response.clicked() {
                            *selected = Some((item.id.clone(), item.name.clone()));
                        }
                        controls::label(
                            ui,
                            if matches!(kind, storage::Kind::Cache | storage::Kind::Image) {
                                &item.id
                            } else {
                                reason(kind, item, japanese)
                            },
                            12.0,
                            MUTED,
                            Align::Min,
                            Sense::hover(),
                        );
                        response
                    })
                    .inner
                });
                controls::cell(&mut row, widths[2], 36.0, Align::Max, |ui| {
                    controls::label(
                        ui,
                        &item
                            .estimated_bytes
                            .map(bytes)
                            .unwrap_or_else(|| docker_size(&item.size)),
                        15.0,
                        super::super::INK,
                        Align::Max,
                        Sense::hover(),
                    );
                });
                controls::cell(&mut row, widths[3], 36.0, Align::Min, |ui| {
                    controls::label(
                        ui,
                        &if kind == storage::Kind::Container {
                            date(activity(item), japanese)
                        } else if kind == storage::Kind::Cache {
                            if item.last_used.is_empty() {
                                text(japanese, "不明", "Unknown").to_owned()
                            } else {
                                relative_time(&item.last_used, japanese)
                            }
                        } else if kind == storage::Kind::Image {
                            if let Some(last) = item.image_last_start {
                                date(Some(last), japanese)
                            } else if let Some(created) = item.image_created {
                                format!(
                                    "{} {}",
                                    text(japanese, "作成", "Created"),
                                    date(Some(created), japanese)
                                )
                            } else {
                                date(None, japanese)
                            }
                        } else if !item.mounts.is_empty() {
                            item.mounts.clone()
                        } else {
                            docker_size(&item.shared_size)
                        },
                        13.0,
                        MUTED,
                        Align::Min,
                        Sense::hover(),
                    );
                });
                controls::cell(&mut row, widths[4], 36.0, Align::Min, |ui| {
                    controls::label(
                        ui,
                        &super::resource_state(kind, item, japanese),
                        12.0,
                        MUTED,
                        Align::Min,
                        Sense::hover(),
                    );
                });
                if response.clicked()
                    || response.secondary_clicked()
                    || name_response.secondary_clicked()
                {
                    *selected = Some((item.id.clone(), item.name.clone()));
                }
                let menu = response.union(name_response);
                let may_remove = match kind {
                    storage::Kind::Image => true,
                    storage::Kind::Container => item.eligible,
                    storage::Kind::Cache => item.name == "regular" && item.status == "false",
                    storage::Kind::Volume => false,
                };
                if let Some(choice) =
                    super::super::item_menu::show(&menu, japanese, false, may_remove, !enabled)
                {
                    *selected = Some((item.id.clone(), item.name.clone()));
                    action = match choice {
                        super::super::item_menu::Action::Remove => match kind {
                            storage::Kind::Image => Action::ReviewImage(item.id.clone()),
                            storage::Kind::Container => Action::ReviewContainer(item.id.clone()),
                            storage::Kind::Cache => Action::ReviewCacheItem(item.id.clone()),
                            storage::Kind::Volume => Action::None,
                        },
                        super::super::item_menu::Action::Check => {
                            Action::CheckResource(kind, item.id.clone(), item.name.clone())
                        }
                        super::super::item_menu::Action::Finder => Action::None,
                    };
                }
            }
        });
    if items.is_empty() {
        ui.label(text(
            japanese,
            "条件に合う項目はありません。",
            "No matching resources.",
        ));
    }
    action
}

fn inspector(
    ui: &mut egui::Ui,
    kind: storage::Kind,
    item: Option<&storage::Resource>,
    japanese: bool,
) {
    let Some(item) = item else {
        ui.label(text(
            japanese,
            "行を選ぶと理由と日時を確認できます。",
            "Select a row to see its details.",
        ));
        return;
    };
    ui.add_space(8.0);
    let title = if kind == storage::Kind::Cache {
        cache_label(item, japanese).to_owned()
    } else {
        name(item)
    };
    ui.label(RichText::new(title).size(19.0).strong());
    ui.label(RichText::new(docker_size(&item.size)).size(24.0));
    ui.small(if kind == storage::Kind::Container {
        text(japanese, "コンテナの書込層", "Container writable layer")
    } else {
        text(japanese, "Docker 内の報告容量", "Reported Docker storage")
    });
    ui.add_space(12.0);
    ui.separator();
    ui.strong(text(japanese, "判定の理由", "Review basis"));
    ui.label(reason(kind, item, japanese));
    if kind == storage::Kind::Container {
        ui.add_space(12.0);
        for (title, time) in [
            (text(japanese, "作成", "Created"), item.created),
            (text(japanese, "直近の起動", "Last started"), item.started),
            (text(japanese, "直近の停止", "Last stopped"), item.finished),
        ] {
            ui.small(title);
            ui.label(date(time, japanese));
        }
        ui.label(format!(
            "{} {}",
            text(japanese, "Docker の再起動回数", "Docker restart count"),
            item.restart_count
        ));
        ui.small(text(
            japanese,
            "全起動履歴やアプリ内の最終利用は分かりません。",
            "Complete lifetime history and application usage are unavailable.",
        ));
        ui.add_space(10.0);
        ui.strong(text(japanese, "保存先とイメージ", "Mounts & image"));
        ui.add(egui::Label::new(&item.detail).wrap());
        ui.add(
            egui::Label::new(if item.mounts.trim().is_empty() {
                text(japanese, "マウント情報なし", "No mount information")
            } else {
                &item.mounts
            })
            .wrap(),
        );
        ui.small(text(japanese,"削除すると書込層と Docker 管理ログを失います。マウント先・ボリューム・イメージは残します。","Removal loses writable data and Docker-managed logs. Mounts, volumes and images remain."));
    } else if kind == storage::Kind::Image {
        ui.label(format!(
            "{} {}",
            text(japanese, "作成日時", "Created"),
            date(item.image_created, japanese),
        ));
        ui.label(format!(
            "{} {}",
            text(japanese, "最終起動（観測）", "Last observed start"),
            date(item.image_last_start, japanese),
        ));
        ui.small(text(
            japanese,
            "現在残っているコンテナの起動日時から集計しています。",
            "Based on start times of containers still present.",
        ));
        ui.label(format!(
            "{} {} · {} {}",
            text(japanese, "共有", "Shared"),
            docker_size(&item.shared_size),
            text(japanese, "固有", "Unique"),
            docker_size(&item.unique_size)
        ));
    }
    ui.add_space(12.0);
    ui.small("ID");
    ui.add(egui::Label::new(&item.id).wrap());
    if controls::button(ui, text(japanese, "ID をコピー", "Copy ID"), false, true).clicked() {
        ui.ctx().copy_text(item.id.clone());
    }
}

fn vm_space_note(japanese: bool) -> &'static str {
    text(japanese, "Colima や Docker Desktop では、VM 内で解放した容量が Mac の空き容量に反映されるまで時間がかかる場合があります。仮想ディスクの縮小は VM の設定に依存します。", "Colima and Docker Desktop release space inside a VM. Mac free space can change later, depending on the VM’s disk reclamation settings.")
}

fn cache_label(item: &storage::Resource, japanese: bool) -> &str {
    // Docker descriptions may contain a literal RUN command or build arguments.
    // The projection returns only a recognized operation, never that raw text.
    match item.operation.as_str() {
        "pull" => return text(japanese, "取得済みイメージのレイヤー", "Pulled image layer"),
        "run" => return text(japanese, "ビルドステップ (RUN)", "Build step (RUN)"),
        "copy" | "add" => return text(japanese, "ビルド入力のコピー", "Copied build input"),
        _ => {}
    }
    match item.name.as_str() {
        "regular" => text(japanese, "ビルドレイヤー", "Build layer"),
        "source.local" => text(japanese, "ローカルのビルド入力", "Local build input"),
        "exec.cachemount" => text(japanese, "ビルド用キャッシュマウント", "Build cache mount"),
        "source.git.checkout" => text(japanese, "Git チェックアウト", "Git checkout"),
        "frontend" => text(japanese, "ビルドフロントエンド", "Build frontend"),
        other => other,
    }
}

fn group_size(items: &[storage::Resource], japanese: bool) -> String {
    items
        .iter()
        .try_fold(0u64, |sum, row| {
            sum.checked_add(storage::size_bytes(&row.size)?)
        })
        .map(bytes)
        .unwrap_or_else(|| text(japanese, "容量不明", "Size unknown").to_owned())
}

fn show_receipt(
    ui: &mut egui::Ui,
    receipt: &storage::Receipt,
    context: &str,
    names: &std::collections::BTreeMap<String, String>,
    japanese: bool,
) {
    if let Some(error) = &receipt.history_error {
        ui.label(format!(
            "{} · {error}",
            text(
                japanese,
                "整理履歴を保存できませんでした",
                "Could not save cleanup history"
            )
        ));
    }
    egui::Frame::NONE
        .stroke(Stroke::new(1.0_f32, LINE))
        .corner_radius(9)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(text(japanese, "Docker の掃除結果", "Docker cleanup result"));
            let removed = receipt.outcomes.iter().filter(|(_, r)| r.is_ok()).count();
            ui.label(format!(
                "{} · {} {} · {} {}",
                context,
                text(japanese, "削除", "Removed"),
                removed,
                text(japanese, "保留・未確認", "Held / unverified"),
                receipt.outcomes.len() - removed
            ));
            let change = match (receipt.before, receipt.after) {
                (Some(a), Some(b)) => {
                    format!("{}{}", if b >= a { "+" } else { "−" }, bytes(b.abs_diff(a)))
                }
                _ => text(japanese, "測定できませんでした", "Unavailable").into(),
            };
            ui.label(RichText::new(change).size(26.0).color(ACCENT));
            ui.small(text(
                japanese,
                "この Mac の空き容量の実測変化（同時に動く他の処理を含みます）",
                "Measured Mac free-space change, including other concurrent activity",
            ));
            ui.label(format!(
                "{} {}",
                text(
                    japanese,
                    "選択時の書込層見込み",
                    "Estimated selected writable data"
                ),
                receipt
                    .estimated_bytes
                    .map(bytes)
                    .unwrap_or_else(|| text(japanese, "不明", "Unknown").into())
            ));
            ui.small(vm_space_note(japanese));
            ui.collapsing(
                text(japanese, "項目ごとの結果", "Individual results"),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("docker-receipt-items")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (id, result) in &receipt.outcomes {
                                ui.strong(names.get(id).map_or(id.as_str(), String::as_str));
                                ui.small(id);
                                match result {
                                    Ok(()) => {
                                        ui.label(text(japanese, "削除を確認", "Removal verified"));
                                    }
                                    Err(error) => {
                                        ui.label(format!(
                                            "{} · {error:#}",
                                            text(
                                                japanese,
                                                "削除を確認できませんでした",
                                                "Removal not confirmed"
                                            )
                                        ));
                                    }
                                }
                                ui.separator();
                            }
                        });
                },
            );
        });
    ui.add_space(12.0);
}

impl App {
    pub(super) fn docker_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.job.is_none() && self.monitor.refresh_pending {
            self.monitor.inspect_docker(&ctx);
        }
        match self
            .monitor
            .docker_view(ui, self.japanese_reasons, self.job.is_some())
        {
            Action::Scan if self.job.is_none() => self.monitor.inspect_docker(&ctx),
            Action::Review if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    let report = Arc::clone(report);
                    let marked = self.monitor.marked.clone();
                    self.spawn(
                        &ctx,
                        text(
                            self.japanese_reasons,
                            "選択したコンテナを再確認中…",
                            "Verifying selected containers…",
                        ),
                        false,
                        move || Outcome::DockerReview(storage::Review::prepare(&report, &marked)),
                    );
                }
            }
            Action::ReviewImage(id) if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    self.review_docker_image(&ctx, report.context.clone(), id);
                }
            }
            Action::ReviewContainer(id) if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    let report = Arc::clone(report);
                    self.spawn(
                        &ctx,
                        text(
                            self.japanese_reasons,
                            "コンテナを再確認中…",
                            "Verifying container…",
                        ),
                        false,
                        move || {
                            Outcome::DockerReview(storage::Review::prepare(
                                &report,
                                &BTreeSet::from([id]),
                            ))
                        },
                    );
                }
            }
            Action::ReviewCacheItem(id) if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    let context = report.context.clone();
                    let days = self.monitor.cache_days;
                    self.spawn(
                        &ctx,
                        text(
                            self.japanese_reasons,
                            "キャッシュを再確認中…",
                            "Verifying cache…",
                        ),
                        false,
                        move || {
                            Outcome::DockerCacheReview(storage::CacheCleanup::prepare_one(
                                &context, days, &id,
                            ))
                        },
                    );
                }
            }
            Action::CheckResource(kind, id, label) if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    let context = report.context.clone();
                    let ja = self.japanese_reasons;
                    self.spawn(
                        &ctx,
                        text(ja, "Docker の状態を確認中…", "Checking Docker state…"),
                        false,
                        move || {
                            let current = storage::inspect(&context).and_then(|report| {
                                report
                                    .groups
                                    .into_iter()
                                    .find(|(group, _)| *group == kind)
                                    .and_then(|(_, rows)| {
                                        rows.into_iter()
                                            .find(|item| item.id == id && item.name == label)
                                    })
                                    .with_context(|| {
                                        text(
                                            ja,
                                            "現在の一覧にこの項目がありません。",
                                            "The item is absent from the current inventory.",
                                        )
                                    })
                            });
                            let mut lines = vec![
                                context,
                                if label.is_empty() { id.clone() } else { label },
                                id,
                            ];
                            match current {
                                Ok(item) => {
                                    lines.push(format!(
                                        "{} · {}",
                                        docker_size(&item.size),
                                        super::resource_state(kind, &item, ja)
                                    ));
                                    lines.push(reason(kind, &item, ja).into());
                                }
                                Err(error) => lines.push(format!("{error:#}")),
                            }
                            Outcome::Inspection(lines)
                        },
                    );
                }
            }
            Action::ReviewCache if self.job.is_none() => {
                if let Some(Ok(report)) = &self.monitor.docker {
                    let context = report.context.clone();
                    let days = self.monitor.cache_days;
                    self.spawn(
                        &ctx,
                        text(
                            self.japanese_reasons,
                            "Docker の接続先を確認中…",
                            "Checking Docker connection…",
                        ),
                        false,
                        move || {
                            Outcome::DockerCacheReview(storage::CacheCleanup::prepare(
                                &context, days,
                            ))
                        },
                    );
                }
            }
            _ => {}
        }
    }

    pub(in crate::review::desktop) fn docker_confirmation(&mut self, ctx: &egui::Context) {
        if let Some(review) = &mut self.monitor.cache_review {
            let japanese = self.japanese_reasons;
            let mut cancel = false;
            let mut remove = false;
            let dialog = egui::Modal::new(egui::Id::new("docker-cache-confirmation")).frame(modal_frame(ctx)).show(ctx, |ui| {
                ui.set_width(520.0);
                ui.heading(text(japanese, "削除するビルドキャッシュを確認", "Review build cache to delete"));
                ui.strong(format!("{}: {} · {} {}", text(japanese, "接続先", "Context"), review.context, review.days, text(japanese, "日以上未使用", "days unused or more")));
                ui.label(text(japanese, "既定ビルダーの通常キャッシュが対象です。使用中・共有・変更可能なキャッシュとキャッシュマウントは対象外です。", "Only regular cache on the default builder is eligible. In-use, shared and mutable cache and cache mounts are excluded."));
                if review.referenced_count > 0 {
                    ui.label(if japanese {
                        format!("別のキャッシュから参照されているため除外: {} 件", review.referenced_count)
                    } else {
                        format!("Excluded because another cache record depends on them: {}", review.referenced_count)
                    });
                }
                if review.targets.is_empty() {
                    ui.label(text(japanese, "条件を満たすキャッシュはありません。", "No cache records meet these conditions."));
                } else {
                    if review.eligible_count > review.targets.len() {
                        ui.label(if japanese {
                            format!("候補 {} 件のうち、容量が大きい {} 件を表示しています。", review.eligible_count, review.targets.len())
                        } else {
                            format!("Showing the {} largest of {} candidates.", review.targets.len(), review.eligible_count)
                        });
                    }
                    ui.label(text(japanese, "ID ／ Docker 内の容量 ／ 最終使用", "ID / Docker size / Last used"));
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for item in &mut review.targets {
                            ui.checkbox(&mut item.selected, format!("{}  ·  {}  ·  {}", item.id, docker_size(&item.size), date(Some(item.last_used), japanese)));
                        }
                    });
                    ui.small(text(japanese, "削除後はビルドが遅くなることがあります。表示容量は Mac で回収できる容量とは異なります。", "Later builds may be slower. Listed sizes differ from the space recoverable on this Mac."));
                }
                let selected = review.targets.iter().filter(|item| item.selected).count();
                ui.horizontal(|ui| {
                    cancel = controls::button(ui, text(japanese, "閉じる", "Close"), false, self.job.is_none()).clicked();
                    if selected > 0 {
                        let label = if japanese { format!("選択した {selected} 件を削除") } else { format!("Delete {selected} selected records") };
                        remove = controls::destructive_button(ui, &label, self.job.is_none()).clicked();
                    }
                });
            });
            if cancel || dialog.should_close() {
                self.monitor.cache_review = None;
            } else if remove && self.job.is_none() {
                let review = self
                    .monitor
                    .cache_review
                    .take()
                    .expect("cache review exists");
                self.monitor.cache_receipt = None;
                self.spawn(
                    ctx,
                    text(
                        japanese,
                        "ビルドキャッシュを整理中…",
                        "Cleaning build cache…",
                    ),
                    true,
                    move || Outcome::DockerCacheRemoval(review.apply()),
                );
            }
            return;
        }
        let Some(review) = &self.monitor.review else {
            return;
        };
        let japanese = self.japanese_reasons;
        let mut cancel = false;
        let mut remove = false;
        let dialog=egui::Modal::new(egui::Id::new("docker-remove-confirmation")).frame(modal_frame(ctx)).show(ctx,|ui| {
            ui.set_width((ctx.content_rect().width()-100.0).clamp(440.0,700.0));
            ui.heading(if japanese {format!("{} 件のコンテナを削除しますか？",review.items.len())}else{format!("Remove {} containers?",review.items.len())});
            ui.strong(format!("{} {}",text(japanese,"接続先","Context"),review.context));
            ui.label(text(japanese,"コンテナ内の書込データと Docker 管理ログが失われます。マウント先・ボリューム・イメージは残します。","Container writable data and Docker-managed logs will be lost. Mounts, volumes and images remain."));
            ui.add_space(12.0);
            egui::ScrollArea::vertical().max_height(280.0).show(ui,|ui| {
                for item in &review.items { ui.strong(name(item)); ui.small(&item.id); ui.label(format!("{} · {} {}",docker_size(&item.size),text(japanese,"最終停止／作成","Stopped / created"),date(activity(item),japanese))); ui.label(item.reason.text(japanese)); ui.add_space(6.0); ui.separator(); }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                cancel=controls::button(ui,text(japanese,"キャンセル","Cancel"),false,self.job.is_none()).clicked();
                ui.with_layout(egui::Layout::right_to_left(Align::Center),|ui| { remove=controls::destructive_button(ui,&if japanese{format!("確認した {} 件を削除",review.items.len())}else{format!("Remove {} confirmed",review.items.len())},self.job.is_none()).clicked(); });
            });
        });
        if cancel || dialog.should_close() {
            self.monitor.review = None;
        } else if remove && self.job.is_none() {
            let review = self.monitor.review.take().expect("confirmation exists");
            self.monitor.receipt_context = review.context.clone();
            self.monitor.receipt_names = review
                .items
                .iter()
                .map(|i| (i.id.clone(), name(i)))
                .collect();
            self.monitor.receipt = None;
            self.spawn(
                ctx,
                text(
                    japanese,
                    "確認したコンテナを削除中…",
                    "Removing confirmed containers…",
                ),
                true,
                move || Outcome::DockerRemoval(review.apply()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_display_converts_known_values_and_preserves_unknown_values() {
        assert_eq!(docker_size("2GB"), bytes(2_000_000_000));
        assert_eq!(docker_size("unknown"), "unknown");
        assert_eq!(relative_time("2 months ago", true), "2か月前");
        assert_eq!(relative_time("1 hour ago", true), "1時間前");
        assert_eq!(relative_time("About an hour ago", true), "約1時間前");
        assert_eq!(relative_time("2 months ago", false), "2 months ago");
        assert_eq!(relative_time("unknown format", true), "unknown format");
    }

    #[test]
    fn cache_cleanup_output_preserves_rows_and_unknown_details() {
        let text = "ID RECLAIMABLE SIZE LAST ACCESSED\nabc123* true 4.227MB 2 months ago\ndef456 false 0B 9 days ago\nTotal: 4.227MB\n";
        let (rows, total, unknown) = parse_cache_output(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "abc123*");
        assert_eq!(rows[0].last_accessed, "2 months ago");
        assert_eq!(rows[1].reclaimable, "false");
        assert_eq!(total.as_deref(), Some("4.227MB"));
        assert!(!unknown);
        let (_, _, unknown) = parse_cache_output("unexpected Docker format\n");
        assert!(unknown);
        let (rows, total, _) = parse_cache_output("Total: 0B\n");
        assert!(rows.is_empty());
        assert_eq!(total.as_deref(), Some("0B"));
    }

    fn resource(id: &str, status: &str, days: i64, eligible: bool) -> storage::Resource {
        let time = Utc::now() - TimeDelta::days(days);
        storage::Resource {
            id: id.into(),
            name: id.into(),
            status: status.into(),
            created: Some(time),
            finished: Some(time),
            eligible,
            estimated_bytes: Some(100),
            ..Default::default()
        }
    }

    #[test]
    fn old_date_is_not_evidence_of_inactivity_for_running_or_unknown() {
        let now = Utc::now();
        let stopped = resource("old", "exited", 60, true);
        let running = resource("running", "running", 60, false);
        let mut unknown = resource("unknown", "exited", 60, false);
        unknown.finished = None;
        assert!(matches(&stopped, "OLD", Filter::Reviewable, 30, now));
        assert!(!matches(&running, "", Filter::All, 30, now));
        assert!(!matches(&unknown, "", Filter::All, 30, now));
        assert!(ordered(&stopped, &unknown, Order::Stopped).is_lt());
        assert!(matches(&running, "", Filter::Running, 0, now));
    }

    #[test]
    fn visible_bulk_selection_excludes_protected_and_filtered_rows() {
        let items = [
            resource("old", "exited", 60, true),
            resource("new", "exited", 1, true),
            resource("keep", "exited", 60, false),
        ];
        let selected: BTreeSet<_> = items
            .iter()
            .filter(|item| matches(item, "", Filter::All, 30, Utc::now()))
            .filter(|item| item.eligible)
            .map(|item| item.id.clone())
            .collect();
        assert_eq!(selected, BTreeSet::from(["old".into()]));
        assert_eq!(selection_size(&items, &selected, false), bytes(100));
        let mut sorted = items.to_vec();
        sorted[1].estimated_bytes = Some(500);
        sorted.sort_by(|a, b| ordered(a, b, Order::Size));
        assert_eq!(sorted[0].id, "new");
    }
}
