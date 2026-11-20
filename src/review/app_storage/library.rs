//! Evidence for read-only Library review. A bundle ID match says nothing
//! about whether files can currently be removed or restored.
use super::mac::command_output;
use super::Message;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct Bundle {
    pub(super) path: PathBuf,
    pub(super) running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Area {
    Cache,
    Log,
    Support,
    Container,
    SharedContainer,
}

impl Area {
    pub(super) const ALL: [(Self, &'static str); 5] = [
        (Self::Cache, "Caches"),
        (Self::Log, "Logs"),
        (Self::Support, "Application Support"),
        (Self::Container, "Containers"),
        (Self::SharedContainer, "Group Containers"),
    ];

    fn reason(self) -> Message {
        match self {
            Self::Cache => Message::new(
                "Cache location. Check the app's cache controls and the cost of rebuilding.",
                "キャッシュの保存先です。再生成の負担とアプリ内の設定を確認してください。",
            ),
            Self::Log => Message::new(
                "Logs may be needed for diagnostics or record keeping.",
                "診断や証跡に使うログが含まれる可能性があります。",
            ),
            Self::Support | Self::Container => Message::new(
                "May contain databases, preferences, or work awaiting sync.",
                "データベース、設定、同期待ちの作業を含む可能性があります。",
            ),
            Self::SharedContainer => Message::new(
                "Shared storage. Multiple apps or extensions may depend on it.",
                "共有の保存先です。複数のアプリや拡張機能が利用する可能性があります。",
            ),
        }
    }
}

#[derive(Default, Debug, Serialize)]
pub(super) struct Activity {
    pub directory_modified: Option<DateTime<Utc>>,
    pub newest_file_modified: Option<DateTime<Utc>>,
    pub newest_file_created: Option<DateTime<Utc>>,
    pub newest_file_accessed: Option<DateTime<Utc>>,
    pub files: u64,
    pub skipped: u64,
    name_hints: NameHints,
}

#[derive(Default, Debug, Serialize)]
struct NameHints {
    database: u64,
    log: u64,
    archive: u64,
    backup: u64,
    undecodable: u64,
}

impl NameHints {
    fn observe(&mut self, name: &OsStr) {
        let Some(name) = name.to_str().map(str::to_ascii_lowercase) else {
            self.undecodable += 1;
            return;
        };
        let database = [
            ".db",
            ".db3",
            ".sqlite",
            ".sqlite3",
            ".realm",
            ".db-wal",
            ".db-shm",
            ".sqlite-wal",
            ".sqlite-shm",
            ".sqlite3-wal",
            ".sqlite3-shm",
        ]
        .iter()
        .any(|ext| name.ends_with(ext));
        let log = name.ends_with(".log")
            || name.ends_with(".log.gz")
            || name.ends_with(".log.old")
            || name.rsplit_once(".log.").is_some_and(|(_, suffix)| {
                let digits = suffix.strip_suffix(".gz").unwrap_or(suffix);
                let digits = digits.strip_prefix("old.").unwrap_or(digits);
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            });
        let archive = [
            ".zip", ".tar", ".tar.gz", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar",
        ]
        .iter()
        .any(|ext| name.ends_with(ext));
        let backup = [".bak", ".backup", ".old"]
            .iter()
            .any(|ext| name.ends_with(ext))
            || name.rsplit_once(".old.").is_some_and(|(_, digits)| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            });
        self.database += u64::from(database);
        self.log += u64::from(log);
        self.archive += u64::from(archive);
        self.backup += u64::from(backup);
    }
}

/// File observations are maxima of recorded filesystem timestamps. Access times
/// may not be updated on every read and never establish that an app is unused.
pub(super) fn measure(path: &Path, home_device: u64) -> Result<(u64, Activity)> {
    let root = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        root.is_dir() && root.dev() == home_device,
        "Unavailable or mounted Library directory"
    );
    let mut activity = Activity {
        directory_modified: root.modified().ok().map(Into::into),
        ..Activity::default()
    };
    let mut allocated = 0u64;
    let mut hardlinks = HashSet::new();
    for entry in ignore::WalkBuilder::new(path)
        .standard_filters(false)
        .follow_links(false)
        .same_file_system(true)
        .build()
    {
        let Ok(entry) = entry else {
            activity.skipped += 1;
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            activity.skipped += 1;
            continue;
        };
        if metadata.dev() != home_device || metadata.is_symlink() {
            activity.skipped += 1;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        activity.files += 1;
        activity.name_hints.observe(entry.file_name());
        if metadata.nlink() == 1 || hardlinks.insert(metadata.ino()) {
            allocated = allocated.saturating_add(metadata.blocks().saturating_mul(512));
        }
        for (latest, stamp) in [
            (&mut activity.newest_file_modified, metadata.modified()),
            (&mut activity.newest_file_created, metadata.created()),
            (&mut activity.newest_file_accessed, metadata.accessed()),
        ] {
            if let Ok(time) = stamp {
                *latest = (*latest).max(Some(time.into()));
            }
        }
    }
    Ok((allocated, activity))
}

#[derive(Debug, Serialize)]
pub(in crate::review) struct Evidence {
    pub(super) previous: Option<(DateTime<Utc>, i128)>,
    area: Area,
    bundle_id: Option<String>,
    installed_bundle: Option<Bundle>,
    activity: Activity,
}

impl Evidence {
    #[cfg(feature = "desktop")]
    pub(in crate::review) fn bundle_id(&self) -> Option<&str> {
        self.bundle_id.as_deref()
    }

    #[cfg(feature = "desktop")]
    pub(in crate::review) fn registered_bundle_observed(&self) -> bool {
        self.installed_bundle.is_some()
    }

    pub(super) fn new(area: Area, name: &OsStr, activity: Activity) -> Self {
        let bundle_id = name
            .to_str()
            .filter(|id| {
                area != Area::SharedContainer
                    && id.len() <= 255
                    && id.split('.').count() >= 2
                    && id.split('.').all(|part| {
                        !part.is_empty()
                            && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    })
            })
            .map(str::to_owned);
        Self {
            previous: None,
            area,
            bundle_id,
            installed_bundle: None,
            activity,
        }
    }

    pub(super) fn describe(&self) -> Message {
        let category = self.area.reason();
        let (en, ja) = match (&self.installed_bundle, &self.bundle_id) {
            (Some(bundle), Some(id)) => {
                let path = bundle.path.display();
                if bundle.running {
                    (
                        format!("Installed bundle ID matches {id} ({path}); app running."),
                        format!(
                            "インストール済みアプリの ID が {id} と一致し、起動中です（{path}）。"
                        ),
                    )
                } else {
                    (
                        format!("Installed bundle ID matches {id} ({path})."),
                        format!("インストール済みアプリの ID が {id} と一致します（{path}）。"),
                    )
                }
            }
            _ => (
                "Owner not verified.".to_owned(),
                "所有元は未確認です。".to_owned(),
            ),
        };
        Message::new(
            format!("{en} {category}"),
            format!("{ja}{}", category.japanese()),
        )
    }

    pub(in crate::review) fn activity_text(&self, japanese: bool) -> String {
        let observed = |t: Option<DateTime<Utc>>| {
            t.map_or_else(
                || "—".to_owned(),
                |t| {
                    t.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d")
                        .to_string()
                },
            )
        };
        let a = &self.activity;
        let (folder, files, modified, created, accessed) = if japanese {
            (
                "フォルダ更新",
                "配下のファイル",
                "更新",
                "作成",
                "アクセス記録",
            )
        } else {
            (
                "Folder modified",
                "Files observed",
                "modified",
                "created",
                "access recorded",
            )
        };
        let delta = self.previous.map_or_else(String::new, |(prior, diff)| {
            let date = prior.with_timezone(&chrono::Local).format("%Y-%m-%d");
            let sign = if diff < 0 { "−" } else { "+" };
            let bytes = crate::review::bytes(diff.unsigned_abs() as u64);
            if japanese {
                format!(" · {date} 比 {sign}{bytes}")
            } else {
                format!(" · {sign}{bytes} since {date}")
            }
        });
        format!(
            "{folder} {} · {files} {} · {modified} {} · {created} {} · {accessed} {}{delta}",
            observed(a.directory_modified),
            a.files,
            observed(a.newest_file_modified),
            observed(a.newest_file_created),
            observed(a.newest_file_accessed)
        )
    }

    pub(in crate::review) fn name_hints_text(&self, japanese: bool) -> String {
        let a = &self.activity;
        let hints = &a.name_hints;
        let mut labels = Vec::new();
        for (count, ja, en) in [
            (hints.database, "DB形式", "database-like"),
            (hints.log, "ログ形式", "log-like"),
            (hints.archive, "アーカイブ形式", "archive-like"),
            (hints.backup, "バックアップ形式", "backup-like"),
        ] {
            if count > 0 {
                labels.push(if japanese {
                    format!("{ja}の可能性 {count}件")
                } else {
                    format!("{count} {en}")
                });
            }
        }
        let detail = if labels.is_empty() {
            if japanese {
                "該当する名前は見つかりませんでした".to_owned()
            } else {
                "No matching names observed".to_owned()
            }
        } else {
            labels.join(" · ")
        };
        if japanese {
            format!("名前からの手がかり: {detail}。名前だけの推定で、内容・用途は未確認です。種類は重複します。{}{}", if hints.undecodable > 0 { format!("文字として判別できない名前が{}件あります。", hints.undecodable) } else { String::new() }, if a.skipped > 0 { "一部を確認できていません。" } else { "" })
        } else {
            format!("Filename clues: {detail}. Names alone do not establish content or purpose; categories overlap.{}{}", if hints.undecodable > 0 { format!(" Could not decode {} filename(s).", hints.undecodable) } else { String::new() }, if a.skipped > 0 { " Some entries were not inspected." } else { "" })
        }
    }
}

pub(super) fn match_bundles(rows: &mut [super::Row]) -> Result<()> {
    let ids: Vec<_> = rows
        .iter()
        .filter_map(|row| row.library.as_ref()?.bundle_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut bundles = BTreeMap::new();
    for group in ids.chunks(128) {
        let matches = lookup_bundles(group)?;
        bundles.extend(matches);
    }
    apply_matches(rows, &bundles);
    Ok(())
}

pub(super) fn lookup_bundles(ids: &[String]) -> Result<BTreeMap<String, Bundle>> {
    let json = command_output(
        Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", include_str!("bundles.js"), "--"])
            .args(ids),
        Duration::from_secs(8),
    )?;
    serde_json::from_str(&json).context("Invalid bundle inventory")
}

fn apply_matches(rows: &mut [super::Row], bundles: &BTreeMap<String, Bundle>) {
    for row in rows {
        if let Some(evidence) = &mut row.library {
            evidence.installed_bundle = evidence
                .bundle_id
                .as_ref()
                .and_then(|id| bundles.get(id))
                .cloned();
            row.reason = evidence.describe();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::app_storage::Row;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    fn row(area: Area, directory: &str) -> Row {
        let evidence = Evidence::new(area, OsStr::new(directory), Activity::default());
        Row {
            label: directory.into(),
            path: None,
            #[cfg(feature = "desktop")]
            native_path: None,
            size_bytes: Some(0),
            size_basis: "allocated",
            partial: false,
            reason: evidence.describe(),
            library: Some(evidence),
        }
    }

    #[test]
    fn only_exact_identifiers_match_and_shared_locations_keep_their_warning() {
        let mut rows = [
            row(Area::Container, "com.example.editor"),
            row(Area::Log, "Editor"),
            row(Area::Cache, "com.example.editor.beta"),
            row(Area::SharedContainer, "com.example.editor"),
        ];
        let installed = BTreeMap::from([(
            "com.example.editor".into(),
            Bundle {
                path: "/Applications/Editor.app".into(),
                running: true,
            },
        )]);
        apply_matches(&mut rows, &installed);
        assert!(rows[0].reason.to_string().contains("app running"));
        for row in &rows[1..] {
            assert!(row.reason.to_string().contains("not verified"));
        }
        assert!(rows[3].reason.to_string().contains("Shared storage"));
        apply_matches(&mut rows, &BTreeMap::new());
        assert!(rows[0].reason.to_string().contains("not verified"));
        assert!(rows[0].library.as_ref().unwrap().installed_bundle.is_none());
    }

    #[test]
    fn observes_file_activity_skips_links_and_counts_hardlinked_blocks_once() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("Caches");
        fs::create_dir(&root)?;
        let first = root.join("first");
        fs::write(&first, [1; 8192])?;
        fs::hard_link(&first, root.join("second"))?;
        symlink(fixture.path(), root.join("outside"))?;
        let m = first.metadata()?;
        let (size, activity) = measure(&root, m.dev())?;
        assert_eq!(size, m.blocks() * 512);
        assert_eq!(activity.files, 2);
        assert_eq!(activity.skipped, 1);
        assert_eq!(activity.newest_file_modified, Some(m.modified()?.into()));
        assert!(measure(&root, m.dev().wrapping_add(1)).is_err());
        Ok(())
    }

    #[test]
    fn filenames_are_only_overlapping_clues_and_partial_and_undecodable_are_shown() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("Caches");
        fs::create_dir(&root)?;
        for name in [
            "local.sqlite-wal",
            "app.LOG.4.gz",
            "warp.log.old.3",
            "bundle.tar.gz",
            "copy.bak",
            "notes.txt",
            "invalid.log..gz",
        ] {
            fs::write(root.join(name), b"content is not examined")?;
        }
        symlink(fixture.path(), root.join("external"))?;
        let (_, mut activity) = measure(&root, root.metadata()?.dev())?;
        // Some macOS filesystems reject non-UTF-8 names, so exercise this
        // case on the classifier without relying on filesystem support.
        activity
            .name_hints
            .observe(&std::ffi::OsString::from_vec(vec![b'x', 0xff]));
        assert_eq!(activity.name_hints.database, 1);
        assert_eq!(activity.name_hints.log, 2);
        assert_eq!(activity.name_hints.archive, 3);
        assert_eq!(activity.name_hints.backup, 2);
        assert_eq!(activity.name_hints.undecodable, 1);
        assert_eq!(activity.skipped, 1);
        let evidence = Evidence::new(Area::Cache, OsStr::new("example"), activity);
        let ja = evidence.name_hints_text(true);
        assert!(ja.contains("内容・用途は未確認"));
        assert!(ja.contains("種類は重複"));
        assert!(ja.contains("判別できない名前が1件"));
        assert!(ja.contains("一部を確認できていません"));
        assert!(evidence
            .name_hints_text(false)
            .contains("Could not decode 1 filename(s)"));
        Ok(())
    }
}
