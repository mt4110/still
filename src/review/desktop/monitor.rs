use super::{bytes, egui, text, App, ACCENT, LINE, MUTED};
use crate::review::{app_storage::command_output, docker::storage};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::Write;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Disk,
    Docker,
}

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    pub tab: Tab,
    pub context: String,
    paused: bool,
    disk_job: Option<Receiver<Result<DiskSample>>>,
    disk: Option<DiskSample>,
    disk_error: String,
    history: VecDeque<Rates>,
    next_sample: Option<Instant>,
    docker_job: Option<Receiver<Result<storage::Report>>>,
    docker: Option<Result<storage::Report>>,
    kind: usize,
    query: String,
    selected: Option<(String, String)>,
}

struct DiskSample {
    at: Instant,
    clock: chrono::DateTime<chrono::Local>,
    devices: Vec<String>,
    total: u64,
    free: u64,
    read: u64,
    written: u64,
}

#[derive(Clone, Copy)]
struct Rates {
    read: f64,
    written: f64,
}

impl Rates {
    fn between(previous: &DiskSample, next: &DiskSample) -> Option<Self> {
        if previous.devices != next.devices {
            return None;
        }
        let seconds = next.at.duration_since(previous.at).as_secs_f64();
        (seconds > 0.0 && seconds <= 10.0).then_some(Self {
            read: next.read.checked_sub(previous.read)? as f64 / seconds,
            written: next.written.checked_sub(previous.written)? as f64 / seconds,
        })
    }
}

fn plist(command: &mut Command) -> Result<Value> {
    let xml = command_output(command, Duration::from_secs(5))?;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(xml.as_bytes())?;
    let json = command_output(
        Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(file.path()),
        Duration::from_secs(5),
    )?;
    Ok(serde_json::from_str(&json)?)
}

fn disk_sample(mut devices: Vec<String>) -> Result<DiskSample> {
    let dirs = directories::BaseDirs::new().context("ホームフォルダを取得できません")?;
    let home = dirs.home_dir();
    if devices.is_empty() {
        let volume = command_output(
            Command::new("/bin/df")
                .env("LC_ALL", "C")
                .arg("-P")
                .arg(home),
            Duration::from_secs(5),
        )
        .context("ホームフォルダのボリュームを確認できません")?;
        let device = volume
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().next())
            .context("ボリュームのデバイスを特定できません")?;
        ensure!(
            device.starts_with("/dev/disk"),
            "読み書き計測はローカルディスクに対応しています"
        );
        let info = plist(
            Command::new("/usr/sbin/diskutil")
                .args(["info", "-plist"])
                .arg(device),
        )
        .context("物理ディスクを特定できません")?;
        for store in info["APFSPhysicalStores"].as_array().into_iter().flatten() {
            if let Some(store) = store["APFSPhysicalStore"].as_str() {
                let disk = store.trim_start_matches("/dev/");
                let digits: String = disk
                    .strip_prefix("disk")
                    .unwrap_or_default()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if !digits.is_empty() {
                    devices.push(format!("disk{digits}"));
                }
            }
        }
        if devices.is_empty() {
            if let Some(disk) = info["ParentWholeDisk"].as_str() {
                devices.push(disk.to_owned());
            }
        }
        devices.sort();
        devices.dedup();
    }
    ensure!(
        !devices.is_empty(),
        "このボリュームの物理ディスクを特定できません"
    );
    let registry = plist(Command::new("/usr/sbin/ioreg").args([
        "-r",
        "-c",
        "IOBlockStorageDriver",
        "-l",
        "-a",
    ]))
    .context("ディスクの読み書き統計を取得できません")?;
    let (read, written) = counters(&registry, &devices)?;
    let stat = rustix::fs::statvfs(home)?;
    let total = stat
        .f_blocks
        .checked_mul(stat.f_frsize)
        .context("容量が範囲外です")?;
    let free = stat
        .f_bavail
        .checked_mul(stat.f_frsize)
        .context("空き容量が範囲外です")?;
    ensure!(total > 0 && free <= total, "ボリューム容量を確認できません");
    Ok(DiskSample {
        at: Instant::now(),
        clock: chrono::Local::now(),
        devices,
        total,
        free,
        read,
        written,
    })
}

fn counters(registry: &Value, devices: &[String]) -> Result<(u64, u64)> {
    let drivers = registry
        .as_array()
        .context("ディスク統計の形式を確認できません")?;
    let mut read = 0u64;
    let mut written = 0u64;
    for disk in devices {
        let driver = drivers
            .iter()
            .find(|driver| {
                driver["IORegistryEntryChildren"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|child| child["BSD Name"].as_str() == Some(disk))
            })
            .with_context(|| format!("{disk} の読み書き統計を取得できません"))?;
        let stats = &driver["Statistics"];
        read = read
            .checked_add(
                stats["Bytes (Read)"]
                    .as_u64()
                    .context("読み込み量が不明です")?,
            )
            .context("読み込み量が範囲外です")?;
        written = written
            .checked_add(
                stats["Bytes (Write)"]
                    .as_u64()
                    .context("書き込み量が不明です")?,
            )
            .context("書き込み量が範囲外です")?;
    }
    Ok((read, written))
}

impl Panel {
    fn reset_disk(&mut self) {
        self.disk = None;
        self.disk_job = None;
        self.next_sample = None;
        self.history.clear();
        self.paused = false;
        self.disk_error.clear();
    }

    pub(super) fn open_tab(&mut self, tab: Tab) {
        if !self.open || self.tab != tab {
            self.reset_disk();
        }
        self.open = true;
        self.tab = tab;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.reset_disk();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.disk_job {
            match job.try_recv() {
                Ok(result) => {
                    self.disk_job = None;
                    match result {
                        Ok(sample) => {
                            self.disk_error.clear();
                            if let Some(previous) = &self.disk {
                                if let Some(rate) = Rates::between(previous, &sample) {
                                    self.history.push_back(rate);
                                    if self.history.len() > 60 {
                                        self.history.pop_front();
                                    }
                                }
                            }
                            self.disk = Some(sample);
                        }
                        Err(error) => self.disk_error = format!("{error:#}"),
                    }
                    self.next_sample = Some(Instant::now() + Duration::from_secs(2));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk_job = None;
                    self.disk_error = "読み書きの計測が中断されました".into();
                    self.next_sample = Some(Instant::now() + Duration::from_secs(2));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(job) = &self.docker_job {
            match job.try_recv() {
                Ok(report) => {
                    self.docker_job = None;
                    if let Ok(report) = &report {
                        if self.context.is_empty() {
                            self.context = report.context.clone();
                        }
                    }
                    self.docker = Some(report);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.docker_job = None;
                    self.docker = Some(Err(anyhow::anyhow!("Docker の調査が中断されました")));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.disk_job.is_some() || self.docker_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn sample(&mut self, ctx: &egui::Context) {
        if self.paused || self.disk_job.is_some() {
            return;
        }
        let deadline = self.next_sample.unwrap_or_else(Instant::now);
        ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        if deadline > Instant::now() {
            return;
        }
        let devices = self
            .disk
            .as_ref()
            .map_or_else(Vec::new, |sample| sample.devices.clone());
        let (tx, rx) = mpsc::channel();
        self.disk_job = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(disk_sample(devices));
            ctx.request_repaint();
        });
    }

    fn inspect_docker(&mut self, ctx: &egui::Context) {
        self.docker = None;
        self.selected = None;
        let context = self.context.clone();
        let (tx, rx) = mpsc::channel();
        self.docker_job = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(storage::inspect(&context));
            ctx.request_repaint();
        });
    }

    fn disk_view(&mut self, ui: &mut egui::Ui, japanese: bool) {
        self.sample(ui.ctx());
        ui.horizontal(|ui| {
            ui.heading(text(japanese, "ディスク", "Disk"));
            let clicked = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.button(text(
                        japanese,
                        if self.paused {
                            "再開"
                        } else {
                            "一時停止"
                        },
                        if self.paused { "Resume" } else { "Pause" },
                    ))
                    .clicked()
                })
                .inner;
            if clicked {
                self.paused = !self.paused;
                if !self.paused {
                    self.disk = None;
                    self.history.clear();
                    self.next_sample = None;
                }
            }
        });
        ui.label(
            egui::RichText::new(text(
                japanese,
                "ホームフォルダの使用量と、物理ディスクの読み書きを確認します。",
                "Inspect home-volume usage and physical-disk activity.",
            ))
            .color(MUTED),
        );
        ui.add_space(24.0);
        if let Some(sample) = &self.disk {
            let used = 1.0 - sample.free as f32 / sample.total as f32;
            ui.label(text(japanese, "ホームフォルダのボリューム", "Home volume"));
            ui.label(
                egui::RichText::new(format!(
                    "{} / {}",
                    bytes(sample.total - sample.free),
                    bytes(sample.total)
                ))
                .size(24.0)
                .strong(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::ProgressBar::new(used)
                    .desired_height(7.0)
                    .corner_radius(4),
            );
            ui.small(format!(
                "{:.1}% {}",
                used * 100.0,
                text(japanese, "使用中", "used")
            ));
            ui.add_space(6.0);
            ui.label(format!(
                "{} {} · {} · {}",
                text(japanese, "空き", "Free"),
                bytes(sample.free),
                sample.devices.join(" + "),
                sample.clock.format("%H:%M:%S")
            ));
        } else if !self.paused && self.disk_error.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(text(
                    japanese,
                    "ディスク情報を取得中…",
                    "Reading disk information…",
                ));
            });
        }
        if !self.disk_error.is_empty() {
            ui.add_space(12.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.strong(text(
                        japanese,
                        "計測できませんでした",
                        "Measurement unavailable",
                    ));
                    ui.label(&self.disk_error);
                });
        }
        ui.add_space(24.0);
        ui.strong(text(japanese, "読み書き速度", "Read and write activity"));
        ui.add_space(8.0);
        if let Some(rate) = self.history.back() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ACCENT,
                    format!(
                        "{} {}/s",
                        text(japanese, "読み込み", "Read"),
                        bytes(rate.read as u64)
                    ),
                );
                ui.colored_label(
                    MUTED,
                    format!(
                        "{} {}/s",
                        text(japanese, "書き込み", "Write"),
                        bytes(rate.written as u64)
                    ),
                );
            });
        } else {
            ui.label(text(
                japanese,
                "2回の計測から読み書き速度を求めます",
                "Waiting for two samples to measure activity",
            ));
        }
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 180.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(248, 249, 251));
        for fraction in [0.25, 0.5, 0.75] {
            let y = rect.top() + rect.height() * fraction;
            painter.hline(rect.x_range(), y, egui::Stroke::new(1.0_f32, LINE));
        }
        let ceiling = self
            .history
            .iter()
            .map(|rate| rate.read.max(rate.written))
            .fold(1024.0, f64::max);
        for (write, color) in [(false, ACCENT), (true, MUTED)] {
            let points: Vec<_> = self
                .history
                .iter()
                .enumerate()
                .map(|(i, rate)| {
                    let value = if write { rate.written } else { rate.read };
                    egui::pos2(
                        rect.left() + rect.width() * i as f32 / 59.0,
                        rect.bottom() - 4.0 - (rect.height() - 8.0) * (value / ceiling) as f32,
                    )
                })
                .collect();
            if points.len() > 1 {
                painter.add(egui::Shape::line(points, egui::Stroke::new(2.0_f32, color)));
            }
        }
        ui.small(format!(
            "{} · {}/s",
            text(
                japanese,
                "直近の最大 60 回。グラフ上端",
                "Up to 60 recent samples. Chart maximum"
            ),
            bytes(ceiling as u64)
        ));
        ui.add_space(12.0);
        ui.label(egui::RichText::new(text(japanese, "この画面を開いている間、約2秒間隔で計測します。読み書きは、このボリュームが使う物理ディスク全体の値です。", "Samples about every two seconds while this view is open. Activity covers the physical disks serving this volume.")).color(MUTED));
    }

    fn docker_view(&mut self, ui: &mut egui::Ui, japanese: bool) {
        ui.heading("Docker");
        ui.label(
            egui::RichText::new(text(
                japanese,
                "コンテナ・イメージ・ボリューム・ビルドキャッシュの容量を確認します。",
                "Inspect containers, images, volumes, and build-cache storage.",
            ))
            .color(MUTED),
        );
        ui.add_space(24.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(text(japanese, "ローカル接続先", "Local context"));
            ui.add_enabled(
                self.docker_job.is_none(),
                super::controls::field(ui, &mut self.context)
                    .desired_width(240.0)
                    .hint_text(text(
                        japanese,
                        "現在の Docker 接続先",
                        "Current Docker context",
                    )),
            );
            if ui
                .add_enabled(
                    self.docker_job.is_none(),
                    egui::Button::new(text(japanese, "容量を調べる", "Inspect storage")),
                )
                .clicked()
            {
                self.inspect_docker(ui.ctx());
            }
            if self.docker_job.is_some() {
                ui.spinner();
                ui.label(text(japanese, "調査中…", "Inspecting…"));
            }
        });
        ui.add_space(10.0);
        ui.label(egui::RichText::new(text(japanese, "Docker 内の容量です。共有レイヤーがあるため、SSD で回収できる容量とは異なる場合があります。", "Docker sizes include shared layers and may differ from SSD space recoverable.")).color(MUTED));
        ui.add_space(20.0);
        let Some(result) = &self.docker else {
            if self.docker_job.is_none() {
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.strong(text(
                            japanese,
                            "接続先を調べる",
                            "Inspect a local connection",
                        ));
                        ui.add_space(6.0);
                        ui.label(text(
                            japanese,
                            "Docker Desktop または Colima を起動し、接続先の容量を調べてください。",
                            "Start Docker Desktop or Colima, then inspect its storage.",
                        ));
                    });
            }
            return;
        };
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.strong(text(
                            japanese,
                            "Docker に接続できませんでした",
                            "Docker connection unavailable",
                        ));
                        ui.add_space(6.0);
                        ui.label(format!("{error:#}"));
                    });
                return;
            }
        };
        ui.label(format!(
            "{} · {} · {}",
            text(japanese, "調査した接続先", "Inspected context"),
            report.context,
            report.observed_at.format("%H:%M:%S")
        ));
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (index, (kind, items)) in report.groups.iter().enumerate() {
                if ui
                    .selectable_value(
                        &mut self.kind,
                        index,
                        format!("{} {}", kind_label(*kind, japanese), items.len()),
                    )
                    .clicked()
                {
                    self.selected = None;
                }
            }
        });
        ui.add_space(8.0);
        ui.add(super::controls::field(ui, &mut self.query).hint_text(text(
            japanese,
            "名前・ID・状態で検索",
            "Search name, ID or state",
        )));
        ui.add_space(12.0);
        let (kind, items) = &report.groups[self.kind];
        let needle = self.query.to_lowercase();
        let rows: Vec<_> = items
            .iter()
            .filter(|item| {
                needle.is_empty()
                    || format!(
                        "{} {} {} {}",
                        item.name,
                        item.id,
                        item.status,
                        resource_state(*kind, item, japanese)
                    )
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect();
        if rows.is_empty() {
            ui.label(text(
                japanese,
                "表示する項目はありません",
                "No matching resources",
            ));
        }
        let name_width = ui.available_width() - 220.0;
        ui.horizontal(|ui| {
            super::controls::cell(ui, name_width, 28.0, egui::Align::Min, |ui| {
                ui.add_space(super::controls::PADDING_X);
                ui.label(
                    egui::RichText::new(text(japanese, "名前", "Name"))
                        .size(12.0)
                        .color(MUTED),
                );
            });
            super::controls::cell(ui, 90.0, 28.0, egui::Align::Max, |ui| {
                ui.label(
                    egui::RichText::new(text(japanese, "容量", "Size"))
                        .size(12.0)
                        .color(MUTED),
                );
            });
            ui.label(
                egui::RichText::new(text(japanese, "状態・参照", "State / references"))
                    .size(12.0)
                    .color(MUTED),
            );
        });
        let table_height = (ui.available_height() - 160.0).clamp(180.0, 420.0);
        egui::ScrollArea::vertical()
            .id_salt("docker-resources")
            .max_height(table_height)
            .show_rows(ui, 34.0, rows.len(), |ui, range| {
                for index in range {
                    let item = rows[index];
                    ui.push_id((&item.id, &item.name), |ui| {
                        ui.horizontal(|ui| {
                            let selected = self
                                .selected
                                .as_ref()
                                .is_some_and(|key| key == &(item.id.clone(), item.name.clone()));
                            if ui
                                .add_sized(
                                    [ui.available_width() - 220.0, 28.0],
                                    egui::Button::new("")
                                        .left_text(egui::RichText::new(&item.name))
                                        .frame_when_inactive(false)
                                        .selected(selected)
                                        .truncate(),
                                )
                                .on_hover_text(&item.name)
                                .clicked()
                            {
                                self.selected = Some((item.id.clone(), item.name.clone()));
                            }
                            super::controls::cell(ui, 90.0, 28.0, egui::Align::Max, |ui| {
                                ui.label(&item.size);
                            });
                            ui.label(resource_state(*kind, item, japanese));
                        });
                    });
                }
            });
        if let Some(item) = items
            .iter()
            .find(|item| self.selected.as_ref() == Some(&(item.id.clone(), item.name.clone())))
        {
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.strong(&item.name);
            ui.horizontal_wrapped(|ui| {
                ui.label(&item.id);
                if ui
                    .small_button(text(japanese, "IDをコピー", "Copy ID"))
                    .clicked()
                {
                    ui.ctx().copy_text(item.id.clone());
                }
            });
            match kind {
                storage::Kind::Image => {
                    ui.label(format!(
                        "{} {} · {} {}",
                        text(japanese, "共有", "Shared"),
                        item.shared_size,
                        text(japanese, "固有", "Unique"),
                        item.unique_size
                    ));
                }
                storage::Kind::Container => {
                    ui.label(format!(
                        "{} {}",
                        text(japanese, "イメージ", "Image"),
                        item.detail
                    ));
                    ui.label(format!(
                        "{} {}",
                        text(japanese, "マウント", "Mounts"),
                        item.mounts
                    ));
                }
                storage::Kind::Volume => {
                    ui.label(text(japanese, "ボリュームは未接続でもアプリのデータを保持するため保護しています。", "Volumes are protected because they can retain app data even when unreferenced."));
                }
                storage::Kind::Cache => {
                    ui.label(format!(
                        "{} {}",
                        text(japanese, "共有", "Shared"),
                        text(
                            japanese,
                            if item.detail == "true" {
                                "あり"
                            } else {
                                "なし"
                            },
                            &item.detail
                        )
                    ));
                }
            }
        }
    }
}

fn kind_label(kind: storage::Kind, japanese: bool) -> &'static str {
    match kind {
        storage::Kind::Container => text(japanese, "コンテナ", "Containers"),
        storage::Kind::Image => text(japanese, "イメージ", "Images"),
        storage::Kind::Volume => text(japanese, "ボリューム", "Volumes"),
        storage::Kind::Cache => text(japanese, "ビルドキャッシュ", "Build cache"),
    }
}

fn resource_state(kind: storage::Kind, item: &storage::Resource, japanese: bool) -> String {
    match kind {
        storage::Kind::Container => match item.status.as_str() {
            "running" => text(japanese, "実行中", "Running").into(),
            "exited" => text(japanese, "停止中", "Stopped").into(),
            "created" => text(japanese, "作成済み", "Created").into(),
            "paused" => text(japanese, "一時停止", "Paused").into(),
            _ => item.status.clone(),
        },
        storage::Kind::Cache => match item.status.as_str() {
            "true" => text(japanese, "使用中", "In use").into(),
            "false" => text(japanese, "未使用", "Unused").into(),
            _ => text(japanese, "未確認", "Unknown").into(),
        },
        _ => format!("{} {}", text(japanese, "参照", "References"), item.status),
    }
}

impl App {
    pub(super) fn monitor_page(&mut self, ui: &mut egui::Ui, tab: Tab) {
        let japanese = self.japanese_reasons;
        let state = &mut self.monitor;
        state.poll(ui.ctx());
        egui::ScrollArea::vertical()
            .id_salt("storage-monitor-page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                match tab {
                    Tab::Disk => state.disk_view(ui, japanese),
                    Tab::Docker => state.docker_view(ui, japanese),
                }
                ui.add_space(20.0);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_volume_devices_and_refuses_missing_counters() -> Result<()> {
        let registry = serde_json::json!([
            {"IORegistryEntryChildren":[{"BSD Name":"disk0"}],"Statistics":{"Bytes (Read)":100,"Bytes (Write)":200}},
            {"IORegistryEntryChildren":[{"BSD Name":"disk7"}],"Statistics":{"Bytes (Read)":9999,"Bytes (Write)":9999}}
        ]);
        assert_eq!(counters(&registry, &["disk0".into()])?, (100, 200));
        assert!(counters(&registry, &["disk1".into()]).is_err());
        Ok(())
    }

    #[test]
    fn rates_rebase_after_counter_reset_long_gaps_and_tab_changes() {
        let sample = |at, read, written| DiskSample {
            at,
            clock: chrono::Local::now(),
            devices: vec!["disk0".into()],
            total: 1000,
            free: 100,
            read,
            written,
        };
        let now = Instant::now();
        let before = sample(now, 100, 100);
        let next = sample(now + Duration::from_secs(2), 300, 500);
        let rate = Rates::between(&before, &next).unwrap();
        assert_eq!((rate.read, rate.written), (100.0, 200.0));
        assert!(
            Rates::between(&before, &sample(now + Duration::from_secs(30), 300, 500)).is_none()
        );
        assert!(Rates::between(&before, &sample(now + Duration::from_secs(2), 0, 0)).is_none());
        let mut panel = Panel {
            disk: Some(next),
            ..Panel::default()
        };
        panel.open_tab(Tab::Docker);
        assert!(panel.disk.is_none());
        panel.close();
        assert!(!panel.open);
    }
}
