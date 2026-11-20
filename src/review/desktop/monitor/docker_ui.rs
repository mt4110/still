use super::*;
use crate::review::desktop::{controls, modal_frame, Outcome};
use chrono::{DateTime, Local, TimeDelta, Utc};
use egui::{Align, Color32, RichText, Sense, Stroke};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Order {
    #[default]
    Stopped,
    Created,
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
            "参照と共有レイヤーを確認する対象です。個別削除は CLI で確認できます。",
            "Review references and shared layers. Individual removal is available through the CLI.",
        ),
        storage::Kind::Volume => text(
            japanese,
            "永続データを保持する可能性があるため保護しています。",
            "Protected because it may contain persistent application data.",
        ),
        storage::Kind::Cache => text(
            japanese,
            "容量を確認できます。上の操作では条件に合う未使用キャッシュをまとめて整理します。",
            "Review usage here. The action above prunes unused cache matching the retention period.",
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
    let haystack =
        format!("{} {} {} {}", item.name, item.id, item.status, item.detail).to_lowercase();
    let status = match filter {
        Filter::All => true,
        Filter::Reviewable => item.eligible,
        Filter::Stopped => matches!(item.status.as_str(), "exited" | "created"),
        Filter::Running => matches!(item.status.as_str(), "running" | "paused" | "restarting"),
    };
    status
        && haystack.contains(&query.to_lowercase())
        && (days == 0
            || activity(item).is_some_and(|time| time <= now - TimeDelta::days(i64::from(days))))
}

fn ordered(a: &storage::Resource, b: &storage::Resource, order: Order) -> std::cmp::Ordering {
    let dates = |a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>| match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    match order {
        Order::Stopped => dates(activity(a), activity(b)),
        Order::Created => dates(a.created, b.created),
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

struct CacheOutputRow {
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

fn cache_output_table(ui: &mut egui::Ui, output: &str, japanese: bool) {
    let (rows, total, unrecognized) = parse_cache_output(output);
    if !rows.is_empty() {
        ui.label(if japanese {
            format!("Docker が報告した項目: {} 件", rows.len())
        } else {
            format!("{} records reported by Docker", rows.len())
        });
        egui::ScrollArea::both()
            .id_salt("docker-cache-result-table")
            .max_height(220.0)
            .show(ui, |ui| {
                egui::Grid::new("docker-cache-result-grid")
                    .striped(true)
                    .spacing([20.0, 5.0])
                    .show(ui, |ui| {
                        ui.strong("ID");
                        ui.strong(text(japanese, "回収可能", "Reclaimable"));
                        ui.strong(text(japanese, "容量", "Size"));
                        ui.strong(text(japanese, "最終使用", "Last accessed"));
                        ui.end_row();
                        for row in &rows {
                            ui.monospace(&row.id);
                            ui.label(if row.reclaimable == "true" {
                                text(japanese, "はい", "Yes")
                            } else {
                                text(japanese, "いいえ", "No")
                            });
                            ui.label(&row.size);
                            ui.label(&row.last_accessed);
                            ui.end_row();
                        }
                    });
            });
    }
    if let Some(total) = &total {
        ui.label(format!(
            "{}: {total}",
            text(japanese, "Docker の報告容量", "Docker reported")
        ));
    }
    if unrecognized || (rows.is_empty() && total.is_none() && !output.trim().is_empty()) {
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
        self.cache_receipt = Some(receipt);
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
            context_changed = ui
                .add_enabled(
                    !busy,
                    controls::field(ui, &mut self.context)
                        .desired_width(240.0)
                        .hint_text(text(
                            japanese,
                            "現在の接続先（例 colima）",
                            "Current context (e.g. colima)",
                        )),
                )
                .changed();
            if self.docker_job.is_some() {
                ui.spinner();
                ui.label(text(japanese, "調査中…", "Inspecting…"));
            }
        });
        if context_changed {
            self.docker = None;
            self.marked.clear();
            self.selected = None;
            self.receipt = None;
            self.cache_receipt = None;
            self.docker_error.clear();
        }
        let mut dismiss_cache_receipt = false;
        if let Some(receipt) = &self.cache_receipt {
            ui.group(|ui| {
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
                match &receipt.result {
                    Ok(output) => {
                        ui.label(text(
                            japanese,
                            "Docker の処理が完了しました。",
                            "Docker cleanup completed.",
                        ));
                        cache_output_table(ui, output, japanese);
                    }
                    Err(error) => {
                        ui.label(format!(
                            "{} · {error:#}",
                            text(
                                japanese,
                                "完了を確認できません。一部削除された可能性があります",
                                "Completion was not confirmed; some cache may have been removed"
                            )
                        ));
                    }
                }
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
                        format!("{} {}", super::kind_label(*kind, japanese), items.len()),
                    )
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
            ui.small(text(japanese, "Docker の接続先にある該当キャッシュをまとめて整理します。正確な対象と回収量は事前に確定できません。", "Docker selects matching cache on this connection. Exact items and reclaimed space cannot be determined in advance."));
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
        let mut visible: Vec<_> = items
            .iter()
            .filter(|item| {
                matches(
                    item,
                    &self.query,
                    if container { self.filter } else { Filter::All },
                    if container { self.age_days } else { 0 },
                    Utc::now(),
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
                rows(
                    ui,
                    *kind,
                    &visible,
                    &mut self.marked,
                    &mut self.selected,
                    japanese,
                    !busy,
                );
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
) {
    let height = ui.available_height();
    let widths = [
        36.0,
        (ui.available_width() - 16.0 - 36.0 - 90.0 - 148.0 - 72.0 - 32.0).max(80.0),
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
        } else {
            text(japanese, "参照・共有", "References / shared")
        },
        text(japanese, "状態", "State"),
    ]
    .iter()
    .enumerate()
    {
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
                    egui::Id::new(("docker-row", &item.id)),
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
                        .id_salt((&item.id, "docker-cells"))
                        .max_rect(rect.shrink2(egui::vec2(8.0, 6.0)))
                        .layout(egui::Layout::left_to_right(Align::Center)),
                );
                row.set_clip_rect(row.clip_rect().intersect(rect));
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
                controls::cell(&mut row, widths[1], 44.0, Align::Min, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(widths[1]);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if controls::label(
                            ui,
                            &name(item),
                            15.0,
                            super::super::INK,
                            Align::Min,
                            Sense::click(),
                        )
                        .clicked()
                        {
                            *selected = Some((item.id.clone(), item.name.clone()));
                        }
                        controls::label(
                            ui,
                            reason(kind, item, japanese),
                            12.0,
                            MUTED,
                            Align::Min,
                            Sense::hover(),
                        );
                    });
                });
                controls::cell(&mut row, widths[2], 36.0, Align::Max, |ui| {
                    controls::label(
                        ui,
                        &item
                            .estimated_bytes
                            .map(bytes)
                            .unwrap_or_else(|| item.size.clone()),
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
                        } else if !item.mounts.is_empty() {
                            item.mounts.clone()
                        } else {
                            item.shared_size.clone()
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
                if response.clicked() {
                    *selected = Some((item.id.clone(), item.name.clone()));
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
    ui.label(RichText::new(name(item)).size(19.0).strong());
    ui.label(RichText::new(&item.size).size(24.0));
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
            "{} {} · {} {}",
            text(japanese, "共有", "Shared"),
            item.shared_size,
            text(japanese, "固有", "Unique"),
            item.unique_size
        ));
    }
    ui.add_space(12.0);
    ui.small("ID");
    ui.add(egui::Label::new(&item.id).wrap());
    if controls::button(ui, text(japanese, "ID をコピー", "Copy ID"), false, true).clicked() {
        ui.ctx().copy_text(item.id.clone());
    }
}

fn show_receipt(
    ui: &mut egui::Ui,
    receipt: &storage::Receipt,
    context: &str,
    names: &std::collections::BTreeMap<String, String>,
    japanese: bool,
) {
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
            ui.small(text(
                japanese,
                "Docker VM 内の解放と Mac の SSD の空きは一致しない場合があります。",
                "Space released inside the Docker VM may not match Mac SSD free space.",
            ));
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
        if let Some(review) = &self.monitor.cache_review {
            let japanese = self.japanese_reasons;
            let mut cancel = false;
            let mut remove = false;
            let dialog = egui::Modal::new(egui::Id::new("docker-cache-confirmation")).frame(modal_frame(ctx)).show(ctx, |ui| {
                ui.set_width(520.0);
                ui.heading(text(japanese, "ビルドキャッシュを整理しますか？", "Clean up build cache?"));
                ui.strong(format!("{}: {}", text(japanese, "接続先", "Context"), review.context));
                ui.label(if japanese { format!("この Docker の既定ビルダーで、{} 日以上未使用のキャッシュを Docker の基準でまとめて削除します。", review.days) } else { format!("Docker will prune unused cache older than {} days from this daemon's default builder.", review.days) });
                ui.label(text(japanese, "対象の全件や回収量は事前に確定できません。次のビルドは遅くなり、キャッシュマウント内のデータが失われる場合があります。ゴミ箱には入りません。", "Exact records and reclaimed space are unknown in advance. Later builds may be slower and cache-mount data may be lost. This bypasses Trash."));
                ui.horizontal(|ui| {
                    cancel = controls::button(ui, text(japanese, "キャンセル", "Cancel"), false, self.job.is_none()).clicked();
                    remove = controls::destructive_button(ui, text(japanese, "確認して整理", "Confirm cleanup"), self.job.is_none()).clicked();
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
                for item in &review.items { ui.strong(name(item)); ui.small(&item.id); ui.label(format!("{} · {} {}",item.size,text(japanese,"最終停止／作成","Stopped / created"),date(activity(item),japanese))); ui.label(item.reason.text(japanese)); ui.add_space(6.0); ui.separator(); }
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
