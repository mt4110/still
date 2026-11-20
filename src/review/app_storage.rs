#[cfg(all(target_os = "macos", feature = "desktop"))]
pub(super) mod known_cache;
#[cfg(target_os = "macos")]
mod library;
#[cfg(all(target_os = "macos", any(test, feature = "desktop")))]
mod observations;

#[cfg(all(target_os = "macos", feature = "desktop"))]
pub(super) fn desktop_scan(xcode: bool, include_library: bool) -> anyhow::Result<Report> {
    let mut report = scan(xcode, include_library)?;
    if include_library {
        if let Err(err) = observations::record(&mut report) {
            report.notes.push(format!(
                "Library の履歴を保存できませんでした / Library history unavailable: {err:#}"
            ));
        }
    }
    Ok(report)
}

#[cfg(all(target_os = "windows", feature = "desktop"))]
pub(super) use scan as desktop_scan;

use super::{bytes, message::Message};
use crate::cli::{AppStorageArgs, ListFormat};
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Row {
    pub label: String,
    pub path: Option<String>,
    #[cfg(all(target_os = "macos", feature = "desktop"))]
    #[serde(skip)]
    pub native_path: Option<std::path::PathBuf>,
    pub size_bytes: Option<u64>,
    pub size_basis: &'static str,
    pub partial: bool,
    pub reason: Message,
    #[cfg(target_os = "macos")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<library::Evidence>,
}

#[derive(Default, Serialize)]
pub(super) struct Report {
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
}

pub fn run(args: &AppStorageArgs) -> Result<()> {
    let result = scan(args.xcode_runtimes, args.library)?;
    match args.format {
        ListFormat::Json => println!("{}", serde_json::to_string(&result)?),
        ListFormat::Text => {
            println!("App storage · review only");
            for row in result.rows {
                let size = row.size_bytes.map_or_else(|| "—".into(), bytes);
                let partial = if row.partial { " · partial read" } else { "" };
                println!("{}  {size} {}{partial}", row.label, row.size_basis);
                if let Some(path) = row.path {
                    println!("  {path}");
                }
                println!("  {}", row.reason);
                #[cfg(target_os = "macos")]
                if let Some(evidence) = row.library {
                    println!("  {}", evidence.activity_text(false));
                }
            }
            for note in result.notes {
                println!("{note}");
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(super) fn scan(_: bool, _: bool) -> Result<Report> {
    anyhow::bail!("App storage review is currently available on macOS")
}

#[cfg(target_os = "macos")]
pub(super) use mac::scan;
#[cfg(all(target_os = "macos", feature = "desktop"))]
pub(super) use mac::{command_output, runtimes, simctl_json, Runtime};

#[cfg(target_os = "macos")]
mod mac {
    use super::{library, Message, Report, Row};
    use crate::review::inventory;
    use anyhow::{bail, Context, Result};
    use serde::Deserialize;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const BROWSER: (&str, &str) = (
        "Browser cache directory. Clear cached files from browser settings when needed.",
        "ブラウザのキャッシュです。必要に応じてブラウザの設定からキャッシュを消去できます。",
    );
    const EVIDENCE: (&str, &str) = (
        "Protected diagnostic data. It may be needed as evidence or for troubleshooting.",
        "証跡や障害調査に必要な場合があるため、ログを保護しています。",
    );
    const SOURCES: &[(&str, &str, (&str, &str))] = &[
        ("Chrome cache", "Library/Caches/Google/Chrome", BROWSER),
        ("Safari cache", "Library/Caches/com.apple.Safari", BROWSER),
        ("Firefox cache", "Library/Caches/Firefox/Profiles", BROWSER),
        ("Edge cache", "Library/Caches/Microsoft Edge", BROWSER),
        ("Slack logs", "Library/Application Support/Slack/logs", EVIDENCE),
        ("Slack logs", "Library/Logs/Slack", EVIDENCE),
        ("Teams logs", "Library/Logs/Microsoft Teams", EVIDENCE),
        ("Teams logs", "Library/Application Support/Microsoft/Teams/logs", EVIDENCE),
        ("Teams logs", "Library/Group Containers/UBF8T346G9.com.microsoft.teams/Library/Logs", EVIDENCE),
        ("Teams logs", "Library/Containers/com.microsoft.teams2/Data/Library/Logs", EVIDENCE),
        ("Notion local data", "Library/Application Support/Notion", (
            "Protected application data. It may include offline pages, local databases, and changes awaiting sync.",
            "オフラインページ、ローカルDB、同期待ちの変更を含む可能性があるため保護しています。",
        )),
        ("Xcode simulator device data", "Library/Developer/CoreSimulator/Devices", (
            "Protected device data. Use Xcode’s Devices and Simulators window to review individual devices.",
            "シミュレータ内のデータを保護しています。端末ごとの確認には Xcode の Devices and Simulators を使ってください。",
        )),
        ("Unity Asset Store downloads", "Library/Unity/Asset Store", (
            "Downloaded packages. Confirm that you can obtain the exact versions again before removing them.",
            "ダウンロード済みパッケージです。整理前に同じ版を再取得できるか確認してください。",
        )),
        ("Unity Asset Store downloads", "Library/Unity/Asset Store-5.x", (
            "Downloaded packages. Confirm that you can obtain the exact versions again before removing them.",
            "ダウンロード済みパッケージです。整理前に同じ版を再取得できるか確認してください。",
        )),
        ("Unreal Engine local DDC", "Library/Application Support/Epic/UnrealEngine/Common/DerivedDataCache", (
            "Derived asset cache. Rebuilding it may take time.",
            "アセットから生成したキャッシュです。再生成には時間がかかる場合があります。",
        )),
    ];

    pub(in crate::review) fn scan(include_xcode: bool, include_library: bool) -> Result<Report> {
        let dirs = directories::BaseDirs::new().context("Could not locate your home directory")?;
        let home = dirs.home_dir().canonicalize()?;
        let mut report = scan_home(&home);
        if include_library {
            scan_library(&home, &mut report);
            if let Err(err) = library::match_bundles(&mut report.rows) {
                report
                    .notes
                    .push(format!("App registration lookup: {err:#}"));
            }
            report.notes.push("Library inventory can overlap known locations; do not add the displayed sizes together.".into());
        }
        if include_xcode {
            match xcode_runtimes() {
                Ok(mut rows) => report.rows.append(&mut rows),
                Err(error) => report.notes.push(format!("Xcode runtimes: {error}")),
            }
        }
        report
            .rows
            .sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then(a.label.cmp(&b.label)));
        Ok(report)
    }

    fn scan_home(home: &Path) -> Report {
        let mut report = Report::default();
        for &(label, relative, reason) in SOURCES {
            measure(
                home,
                &home.join(relative),
                label.into(),
                reason,
                &mut report,
            );
        }
        let ndk = home.join("Library/Android/sdk/ndk");
        if let Some(root) = known_directory(home, &ndk, &mut report.notes) {
            match fs::read_dir(&root) {
                Ok(entries) => {
                    for entry in entries {
                        let Ok(entry) = entry else {
                            report
                                .notes
                                .push("Could not read an Android NDK directory entry.".into());
                            continue;
                        };
                        let version = entry.file_name();
                        let Some(version) = version.to_str().filter(|v| {
                            v.split('.').count() >= 3
                                && v.split('.').all(|part| {
                                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                                })
                        }) else {
                            continue;
                        };
                        let Some(path) = known_directory(home, &entry.path(), &mut report.notes)
                        else {
                            continue;
                        };
                        if fs::symlink_metadata(path.join("source.properties"))
                            .is_ok_and(|m| m.is_file() && !inventory::is_link(&m))
                        {
                            measure(home, &path, format!("Android NDK {version}"), (
                                "A project may pin this version. Check project requirements and use Android Studio’s SDK Manager.",
                                "プロジェクトがこの版を指定している場合があります。利用状況を確認し、Android Studio の SDK Manager で管理してください。",
                            ), &mut report);
                        }
                    }
                }
                Err(error) => report.notes.push(format!("Android NDK: {error}")),
            }
        }
        report
    }

    fn scan_library(home: &Path, report: &mut Report) {
        let home_device = match fs::metadata(home) {
            Ok(meta) => meta.dev(),
            Err(err) => {
                report.notes.push(format!("Library: {err}"));
                return;
            }
        };
        for (kind, area) in library::Area::ALL {
            let base = home.join("Library").join(area);
            let Some(base) = known_directory(home, &base, &mut report.notes) else {
                continue;
            };
            let entries = match fs::read_dir(&base) {
                Ok(entries) => entries,
                Err(err) => {
                    report
                        .notes
                        .push(format!("{} · {err}", inventory::display(&base)));
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        report
                            .notes
                            .push(format!("{} · {err}", inventory::display(&base)));
                        continue;
                    }
                };
                let path = entry.path();
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() && !inventory::is_link(&meta) => {
                        let label =
                            format!("Library/{area}/{}", entry.file_name().to_string_lossy());
                        let Some(path) = known_directory(home, &path, &mut report.notes) else {
                            continue;
                        };
                        match library::measure(&path, home_device) {
                            Ok((size, activity)) => {
                                let partial = activity.skipped > 0;
                                let evidence =
                                    library::Evidence::new(kind, &entry.file_name(), activity);
                                report.rows.push(Row {
                                    label,
                                    path: Some(inventory::display(&path)),
                                    #[cfg(feature = "desktop")]
                                    native_path: Some(path.clone()),
                                    size_bytes: Some(size),
                                    size_basis: "allocated",
                                    partial,
                                    reason: evidence.describe(),
                                    library: Some(evidence),
                                });
                            }
                            Err(err) => report.notes.push(format!("{label} · {err}")),
                        }
                    }
                    Ok(meta) if inventory::is_link(&meta) => report
                        .notes
                        .push(format!("Skipped link: {}", inventory::display(&path))),
                    Ok(_) => {}
                    Err(err) => report
                        .notes
                        .push(format!("{} · {err}", inventory::display(&path))),
                }
            }
        }
    }

    fn known_directory(home: &Path, path: &Path, notes: &mut Vec<String>) -> Option<PathBuf> {
        match inventory::home_scan_dir(home, path) {
            Ok(path) => Some(path),
            Err(err)
                if err.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                }) =>
            {
                None
            }
            Err(err) => {
                notes.push(format!("{} · {err}", inventory::display(path)));
                None
            }
        }
    }

    fn measure(home: &Path, path: &Path, label: String, reason: (&str, &str), report: &mut Report) {
        let Some(path) = known_directory(home, path, &mut report.notes) else {
            return;
        };
        match inventory::scan(&path, 4, None) {
            Ok(scan) => {
                if let Some(root) = scan.directories.first() {
                    report.rows.push(Row {
                        label,
                        path: Some(inventory::display(&path)),
                        #[cfg(feature = "desktop")]
                        native_path: Some(path.clone()),
                        size_bytes: Some(root.usage.size()),
                        size_basis: if root.usage.allocated_bytes.is_some() {
                            "allocated"
                        } else {
                            "logical"
                        },
                        library: None,
                        partial: root.usage.skipped > 0,
                        reason: Message::new(reason.0, reason.1),
                    });
                }
            }
            Err(err) => report.notes.push(format!("{label} · {err}")),
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(in crate::review) struct Runtime {
        pub platform_identifier: String,
        pub version: String,
        pub build: String,
        pub size_bytes: Option<u64>,
        pub deletable: Option<bool>,
        #[cfg(feature = "desktop")]
        pub runtime_identifier: String,
        #[cfg(feature = "desktop")]
        pub state: String,
        #[cfg(feature = "desktop")]
        pub path: Option<PathBuf>,
    }

    pub(in crate::review) fn command_output(
        command: &mut Command,
        timeout: Duration,
    ) -> Result<String> {
        let mut output = tempfile::tempfile()?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(Stdio::null())
            .spawn()
            .context("Could not launch the management tool")?;
        let deadline = Instant::now() + timeout;
        loop {
            if output.metadata()?.len() > 4 * 1024 * 1024 {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Management tool output exceeded 4 MiB");
            }
            if let Some(status) = child.try_wait()? {
                if status.success() {
                    break;
                }
                bail!("Management tool exited unsuccessfully ({status})");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Management tool timed out");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        output.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        output.take(4 * 1024 * 1024 + 1).read_to_string(&mut text)?;
        if text.len() > 4 * 1024 * 1024 {
            bail!("Management tool output exceeded 4 MiB");
        }
        Ok(text)
    }

    pub(in crate::review) fn simctl_json<T: serde::de::DeserializeOwned>(
        args: &[&str],
    ) -> Result<T> {
        let output = command_output(
            Command::new("/usr/bin/xcrun").arg("simctl").args(args),
            Duration::from_secs(12),
        )?;
        serde_json::from_str(&output).context("Could not read the Xcode simulator inventory")
    }

    pub(in crate::review) fn runtimes() -> Result<BTreeMap<String, Runtime>> {
        simctl_json(&["runtime", "list", "-j"])
    }

    impl Runtime {
        pub fn platform(&self) -> &str {
            match self.platform_identifier.as_str() {
                "com.apple.platform.iphonesimulator" => "iOS",
                "com.apple.platform.watchsimulator" => "watchOS",
                "com.apple.platform.appletvsimulator" => "tvOS",
                "com.apple.platform.xrsimulator" => "visionOS",
                _ => "Xcode",
            }
        }

        pub fn label(&self) -> String {
            format!(
                "{} {} runtime ({})",
                self.platform(),
                self.version,
                self.build
            )
        }
    }

    fn xcode_runtimes() -> Result<Vec<Row>> {
        // Ask Xcode for metadata. Never use the system paths returned in its output.
        Ok(runtimes()?.into_values().map(|runtime| {
            Row {
                label: runtime.label(),
                path: None,
                #[cfg(feature = "desktop")]
                native_path: None,
                size_bytes: runtime.size_bytes,
                size_basis: "reported by Xcode",
                partial: false,
                library: None,
                reason: if runtime.deletable == Some(true) {
                    Message::new(
                        "Xcode allows removal in Settings → Components. Check the versions you still test.",
                        "Xcode の Settings → Components で削除できます。テストに必要な版を先に確認してください。",
                    )
                } else {
                    Message::new(
                        "Check availability and removal options in Xcode Settings → Components.",
                        "Xcode の Settings → Components で利用状況と削除の可否を確認してください。",
                    )
                },
            }
        }).collect())
    }

    #[cfg(test)]
    #[test]
    fn library_inventory_reports_unknown_owners_and_skips_links() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let base = fixture.path().canonicalize()?;
        let home = base.join("home");
        let cache = home.join("Library/Caches/unknown.application");
        let outside = base.join("outside");
        fs::create_dir_all(&cache)?;
        fs::create_dir_all(&outside)?;
        fs::write(cache.join("data"), b"local data")?;
        std::os::unix::fs::symlink(&outside, home.join("Library/Caches/link"))?;
        let mut report = Report::default();
        scan_library(&home, &mut report);
        assert_eq!(report.rows.len(), 1);
        assert!(report.rows[0].reason.to_string().contains("not verified"));
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("Skipped link")));
        assert!(cache.join("data").exists());
        Ok(())
    }

    #[cfg(test)]
    #[test]
    fn reports_known_home_data_without_following_outside_links() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let base = fixture.path().canonicalize()?;
        let home = base.join("home");
        let outside = base.join("outside");
        for path in [
            home.join("Library/Application Support/Notion"),
            home.join("Library/Logs/Slack"),
            home.join("Library/Android/sdk/ndk/27.1.12297006"),
            home.join("Library/Caches/Google"),
            outside.clone(),
        ] {
            fs::create_dir_all(path)?;
        }
        fs::write(
            home.join("Library/Application Support/Notion/local.db"),
            b"not parsed",
        )?;
        fs::write(home.join("Library/Logs/Slack/work.log"), b"not parsed")?;
        fs::write(
            home.join("Library/Android/sdk/ndk/27.1.12297006/source.properties"),
            b"marker",
        )?;
        std::os::unix::fs::symlink(outside, home.join("Library/Caches/Google/Chrome"))?;
        let report = scan_home(&home);
        assert_eq!(report.rows.len(), 3);
        assert!(report
            .rows
            .iter()
            .any(|row| row.label == "Notion local data"
                && row
                    .reason
                    .to_string()
                    .contains("Protected application data")));
        assert!(report.rows.iter().any(|row| row.label == "Slack logs"
            && row.reason.to_string().contains("Protected diagnostic data")));
        assert!(report
            .rows
            .iter()
            .any(|row| row.label == "Android NDK 27.1.12297006"));
        assert!(report.notes.iter().any(|note| note.contains("Chrome")));
        Ok(())
    }
}
