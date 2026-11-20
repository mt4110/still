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
    path: PathBuf,
    running: bool,
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
        let json = command_output(
            Command::new("/usr/bin/osascript")
                .args(["-l", "JavaScript", "-e", include_str!("bundles.js"), "--"])
                .args(group),
            Duration::from_secs(8),
        )?;
        let matches: BTreeMap<String, Bundle> =
            serde_json::from_str(&json).context("Invalid bundle inventory")?;
        bundles.extend(matches);
    }
    apply_matches(rows, &bundles);
    Ok(())
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
    use std::os::unix::fs::symlink;

    fn row(area: Area, directory: &str) -> Row {
        let evidence = Evidence::new(area, OsStr::new(directory), Activity::default());
        Row {
            label: directory.into(),
            path: None,
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
}
