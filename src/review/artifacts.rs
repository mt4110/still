mod cargo_cache;
mod vendor;

use super::message::Message;
use super::{filesystem, git, inventory};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Producer {
    Cargo,
    CargoRelocated,
    CargoIncremental,
    CargoDependencies,
    NextJs,
    Vite,
    NodeModules,
    PythonBytecode,
    UnityLibrary,
    UnityAssets,
    UnrealCache,
    UnrealProjectData,
    UnrealEngine,
    DeclaredOutput,
}

impl Producer {
    pub fn reason(self) -> &'static str {
        self.explanation().0
    }

    pub fn explanation(self) -> (&'static str, &'static str) {
        match self {
            Self::Cargo => ("Cargo manifest and build metadata", "Cargo の設定とビルド情報を検出"),
            Self::CargoIncremental => ("Rust compiler incremental cache; rebuilt on the next build", "Rust の差分コンパイル用キャッシュ。次のビルド時に再生成します。"),
            Self::CargoDependencies => ("Cargo compiled artifacts and dependency metadata; rebuilt by Cargo", "Cargo のコンパイル済み成果物と依存情報。次回のビルドで再生成します。"),
            Self::CargoRelocated => ("Cargo metadata and compiled profile in a relocated output", "別の場所に置かれた Cargo のビルド出力を検出"),
            Self::NextJs => ("Next.js dependency and build output", "Next.js の依存関係とビルド出力を検出"),
            Self::Vite => ("Vite dependency and bundled assets", "Vite の依存関係とビルド出力を検出"),
            Self::NodeModules => ("Dependency directory beside package.json; review only because it may contain local changes or linked packages", "package.json の隣にある依存フォルダです。ローカル変更やリンクを含む場合があるため、削除せず確認のみ行います。"),
            Self::PythonBytecode => ("CPython bytecode with matching source files", "対応するソースがある CPython のキャッシュを検出"),
            Self::UnityLibrary => ("Unity asset cache; protected here because regeneration requires a full reimport", "Unity のキャッシュです。削除後に全アセットの再インポートが必要なため保護しています。"),
            Self::UnityAssets => ("Protected Unity source assets", "Unity プロジェクトの元アセットを保護しています。"),
            Self::UnrealCache => ("Unreal Engine derived cache; rebuilding can take time", "Unreal Engine の生成キャッシュです。再生成には時間がかかる場合があります。"),
            Self::UnrealProjectData => ("Protected Unreal project content, autosaves, or logs", "Unreal プロジェクトの元アセット、自動保存、ログを保護しています。"),
            Self::UnrealEngine => ("Unreal Engine files. Check project requirements and manage installed versions from Epic Games Launcher.", "Unreal Engine 本体です。プロジェクトが使う版を確認し、インストール済みの版は Epic Games Launcher で管理してください。"),
            Self::DeclaredOutput => ("Dedicated output declared with --output", "専用の出力先として登録されたフォルダ"),
        }
    }

    fn is_partial_cargo(self) -> bool {
        matches!(self, Self::CargoIncremental | Self::CargoDependencies)
    }

    fn is_review_only(self) -> bool {
        matches!(
            self,
            Self::UnityLibrary
                | Self::NodeModules
                | Self::UnityAssets
                | Self::UnrealCache
                | Self::UnrealProjectData
                | Self::UnrealEngine
        )
    }
}

pub struct FileCandidate {
    pub row: inventory::Row,
    pub producer: Producer,
    pub blocked_reason: Option<Message>,
}

pub struct Policy {
    pub roots: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    pub docker_context: Option<String>,
}

impl Policy {
    fn permits(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| path != root && path.starts_with(root))
            && !path.ancestors().any(protected)
            && !path
                .ancestors()
                .skip(1)
                .take_while(|dir| self.roots.iter().any(|root| dir.starts_with(root)))
                .any(|dir| game_storage(dir).is_some())
    }

    pub fn producer(&self, path: &Path) -> Option<Producer> {
        if !self.permits(path) {
            return None;
        }
        if let Some(storage) = game_storage(path) {
            return Some(storage);
        }
        if self.outputs.iter().any(|output| path == output) {
            return Some(Producer::DeclaredOutput);
        }
        // Package contents can contain their own build markers and manifests;
        // they are part of the installed dependency, not separate project outputs.
        if path
            .ancestors()
            .skip(1)
            .any(|dir| dir.file_name().is_some_and(|name| name == "node_modules"))
        {
            return None;
        }
        if matches!(
            path.file_name().and_then(|s| s.to_str()),
            Some("incremental" | "deps")
        ) && cargo_cache::cache_root(path).is_some_and(|target| self.permits(target))
        {
            return Some(if path.ends_with("incremental") {
                Producer::CargoIncremental
            } else {
                Producer::CargoDependencies
            });
        }
        if cargo_cache::incremental_unit(path).is_some_and(|target| self.permits(target)) {
            return Some(Producer::CargoIncremental);
        }
        let parent = path.parent()?;
        match path.file_name()?.to_str()? {
            "node_modules"
                if regular_file(&parent.join("package.json"))
                    && !parent
                        .ancestors()
                        .any(|dir| dir.file_name().is_some_and(|name| name == "node_modules")) =>
            {
                Some(Producer::NodeModules)
            }
            "target"
                if regular_file(&parent.join("Cargo.toml"))
                    && (regular_file(&path.join(".rustc_info.json"))
                        || regular_file(&path.join("CACHEDIR.TAG")))
                    && (directory(&path.join("debug"))
                        || directory(&path.join("release"))
                        || directory(&path.join("doc"))) =>
            {
                Some(Producer::Cargo)
            }
            ".next"
                if has_dependency(parent, "next")
                    && (regular_file(&path.join("BUILD_ID"))
                        || (directory(&path.join("server")) && directory(&path.join("cache")))) =>
            {
                Some(Producer::NextJs)
            }
            "dist"
                if has_dependency(parent, "vite")
                    && regular_file(&path.join("index.html"))
                    && directory(&path.join("assets")) =>
            {
                Some(Producer::Vite)
            }
            "__pycache__" if python_bytecode(path, parent) => Some(Producer::PythonBytecode),
            _ if relocated_cargo(path) => Some(Producer::CargoRelocated),
            _ => None,
        }
    }

    fn review_path(&self, path: &Path) -> Result<(PathBuf, Producer)> {
        let (path, producer) = self.check_location(path)?;
        git::protect_tracked(&path, producer == Producer::DeclaredOutput)?;
        Ok((path, producer))
    }

    fn check_location(&self, path: &Path) -> Result<(PathBuf, Producer)> {
        let path = inventory::safe_dir(path)?;
        let producer = self.producer(&path).context(Message::new(
            "No verified output producer",
            "何が生成したフォルダか確認できませんでした。",
        ))?;
        if producer.is_partial_cargo() {
            let unit = path
                .parent()
                .is_some_and(|parent| parent.ends_with("incremental"));
            for parent in path.ancestors().skip(1).take(if unit { 3 } else { 2 }) {
                for marker in [".still-active", ".keep", ".gitkeep", ".git"] {
                    match fs::symlink_metadata(parent.join(marker)) {
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                        Ok(_) => {
                            return Err(Message::new(
                                format!("Protected parent marker: {marker}"),
                                format!("親フォルダの保護マーカー {marker} を検出しました。"),
                            )
                            .into())
                        }
                    }
                }
            }
        }
        if producer.is_review_only() {
            let (english, japanese) = producer.explanation();
            return Err(Message::new(english, japanese).into());
        }
        if producer == Producer::CargoRelocated && !self.has_enclosing_cargo_project(&path) {
            return Err(Message::new(
                "No enclosing Cargo project verified. Declare a dedicated folder with --output.",
                "対応する Cargo プロジェクトを確認できません。再生成できる専用フォルダなら、出力先として登録してください。",
            ).into());
        }
        if let Some(evidence) = path.ancestors().find(|dir| evidence_name(dir)) {
            return Err(evidence_reason(evidence).into());
        }
        if std::env::current_dir()?.starts_with(&path) {
            return Err(Message::new(
                "The current working directory is inside this output",
                "現在の作業フォルダが候補の中にあります。そこから移動して再確認してください。",
            )
            .into());
        }
        Ok((path, producer))
    }

    fn has_enclosing_cargo_project(&self, path: &Path) -> bool {
        let mut ancestors = path.ancestors();
        // Don't look past a nested repository or outside the cleanup roots.
        if ancestors
            .next()
            .is_some_and(|p| p.join(".git").symlink_metadata().is_ok())
        {
            return false;
        }
        for dir in ancestors {
            if !self.roots.iter().any(|root| dir.starts_with(root)) {
                break;
            }
            if regular_file(&dir.join("Cargo.toml")) {
                return true;
            }
            if dir.join(".git").symlink_metadata().is_ok() {
                break;
            }
        }
        false
    }

    pub fn prepare(&self, path: &Path) -> Result<Prepared> {
        let (path, producer) = self.review_path(path)?;
        let tree = stamp_output(&path, producer)?;
        Ok(Prepared {
            path,
            producer,
            tree,
        })
    }

    pub fn remove(&self, approved: &Prepared) -> Result<()> {
        let current = self.prepare(&approved.path)?;
        if current.producer != approved.producer || current.tree != approved.tree {
            return Err(Message::new(
                "The output changed after review. Refresh and review it again.",
                "確認した後に内容が変わりました。再スキャンしてから選び直してください。",
            )
            .into());
        }
        if let Some(context) = &self.docker_context {
            super::docker::protect_bind_mounts(context, &current.path)?;
        }
        filesystem::ensure_not_open(&current.path)?;
        filesystem::remove_output(&current.path, &current.tree)
    }
}

pub fn file_candidates(scan: &inventory::Inventory, policy: &Policy) -> Vec<FileCandidate> {
    let mut found: Vec<_> = scan
        .directories
        .iter()
        .filter_map(|row| {
            let well_known = matches!(
                row.path.file_name().and_then(|name| name.to_str()),
                Some(
                    "target"
                        | ".next"
                        | "dist"
                        | "node_modules"
                        | "__pycache__"
                        | "Library"
                        | "Assets"
                        | "DerivedDataCache"
                        | "Content"
                        | "Saved"
                        | "Engine"
                )
            );
            if !well_known
                && !scan.cargo_marker_dirs.contains(&row.path)
                && !policy.outputs.contains(&row.path)
            {
                return None;
            }
            let producer = policy.producer(&row.path)?;
            Some((row, producer))
        })
        .collect();
    // A cross-compiled platform directory can also carry Cargo markers.
    // Keep one automatic candidate for its enclosing build output.
    let cargo_roots: HashSet<_> = found
        .iter()
        .filter(|(_, producer)| matches!(producer, Producer::Cargo | Producer::CargoRelocated))
        .map(|(row, _)| row.path.as_path())
        .collect();
    found.retain(|(row, producer)| {
        *producer != Producer::CargoRelocated
            || !row
                .path
                .ancestors()
                .skip(1)
                .any(|dir| cargo_roots.contains(dir))
    });
    if found.is_empty() {
        return Vec::new();
    }
    let mounts = policy
        .docker_context
        .as_deref()
        .map(super::docker::bind_mount_paths)
        .transpose();
    let inspect = |row: &inventory::Row, producer: Producer, git_checked_parent: Option<&Path>| {
        let preflight = (|| -> Result<()> {
            let (path, _) = if git_checked_parent.is_some_and(|parent| {
                row.path.parent() == Some(parent) && parent.ends_with("incremental")
            }) {
                policy.check_location(&row.path)?
            } else {
                policy.review_path(&row.path)?
            };
            let mount_paths = mounts.as_ref().map_err(|e| Message::new(
                format!("Could not verify Docker bind mounts: {e:#}"),
                "Docker に接続してマウント先を確認できませんでした。接続先を確認して再スキャンしてください。",
            ))?;
            if let Some(paths) = mount_paths {
                super::docker::check_bind_mounts(&path, paths)?;
            }
            let mut vendor = vendor::Fixtures::new(&path, producer);
            walk_tree(
                &path,
                |relative| vendor.digest(relative),
                |_, relative, meta, _| verify_cargo_entry(producer, &path, relative, meta),
            )
        })();
        FileCandidate {
            row: row.clone(),
            producer,
            blocked_reason: preflight.err().as_ref().map(Message::from_error),
        }
    };
    let workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(4));
    let mut files: Vec<_> = std::thread::scope(|scope| {
        found
            .chunks(found.len().div_ceil(workers))
            .map(|chunk| {
                let inspect = &inspect;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(row, producer)| inspect(row, producer, None))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|worker| worker.join().expect("candidate review worker"))
            .collect()
    });
    // Only expand a held target; keep the ordinary Cargo row for a clean build tree.
    let held_targets: HashSet<_> = files
        .iter()
        .filter(|c| c.blocked_reason.is_some() && c.producer == Producer::Cargo)
        .map(|c| c.row.path.clone())
        .collect();
    if !held_targets.is_empty() {
        for row in &scan.directories {
            if !matches!(
                row.path.file_name().and_then(|s| s.to_str()),
                Some("incremental" | "deps")
            ) || !row
                .path
                .ancestors()
                .nth(2)
                .is_some_and(|p| held_targets.contains(p))
            {
                continue;
            }
            if let Some(producer) = policy.producer(&row.path).filter(|p| p.is_partial_cargo()) {
                files.push(inspect(row, producer, None));
            }
        }
    }
    // A stopped build can leave one unfinished compilation among hundreds of
    // completed units. Verify the parent's Git scope once for discovery. Each
    // selected unit gets its own Git check again during review and removal.
    let held_incremental: HashSet<_> = files
        .iter()
        .filter(|candidate| {
            candidate.producer == Producer::CargoIncremental
                && candidate.row.path.ends_with("incremental")
                && candidate.blocked_reason.is_some()
                && policy.review_path(&candidate.row.path).is_ok()
        })
        .map(|candidate| candidate.row.path.clone())
        .collect();
    if !held_incremental.is_empty() {
        for row in &scan.directories {
            let Some(parent) = row.path.parent() else {
                continue;
            };
            if held_incremental.contains(parent)
                && cargo_cache::incremental_unit(&row.path).is_some()
            {
                files.push(inspect(row, Producer::CargoIncremental, Some(parent)));
            }
        }
    }
    files.sort_unstable_by(|a, b| {
        b.row
            .usage
            .size()
            .cmp(&a.row.usage.size())
            .then_with(|| a.row.path.cmp(&b.row.path))
    });
    files
}

pub struct Prepared {
    pub path: PathBuf,
    pub producer: Producer,
    tree: Tree,
}

impl Prepared {
    pub fn files(&self) -> usize {
        self.tree
            .entries
            .values()
            .filter(|stamp| !stamp.is_dir)
            .count()
    }

    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    pub fn logical_bytes(&self) -> u64 {
        self.tree
            .entries
            .values()
            .filter(|stamp| !stamp.is_dir)
            .fold(0, |bytes, stamp| bytes.saturating_add(stamp.len))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Tree {
    pub parent: filesystem::Identity,
    pub entries: BTreeMap<PathBuf, Stamp>,
    pub fixture_digests: BTreeMap<PathBuf, [u8; 32]>,
    producer: Producer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub identity: filesystem::Identity,
    pub is_dir: bool,
    pub len: u64,
    pub modified: SystemTime,
    pub changed: (i64, i64),
    pub contents: Option<u64>,
}

#[cfg(windows)]
pub fn stamp_tree(path: &Path, reviewed: &Tree) -> Result<Tree> {
    stamp_output(path, reviewed.producer)
}

fn verify_cargo_entry(
    producer: Producer,
    path: &Path,
    relative: &Path,
    metadata: &fs::Metadata,
) -> Result<()> {
    if producer.is_partial_cargo() {
        cargo_cache::verify_entry(path, relative, metadata)
    } else if matches!(producer, Producer::Cargo | Producer::CargoRelocated) {
        cargo_cache::protect_unfinished(relative, metadata)
    } else {
        Ok(())
    }
}

fn stamp_output(path: &Path, producer: Producer) -> Result<Tree> {
    stamp_with(path, producer, |_, _, _| Ok(()))
}

pub(super) fn stamp_with(
    path: &Path,
    producer: Producer,
    mut inspect: impl FnMut(&Path, &Path, &fs::Metadata) -> Result<()>,
) -> Result<Tree> {
    let parent = path.parent().context("Cannot stage a filesystem root")?;
    let parent_identity = filesystem::identity(parent, &fs::symlink_metadata(parent)?)?;
    let mut entries = BTreeMap::new();
    let mut fixture_digests = BTreeMap::new();
    let mut vendor = vendor::Fixtures::new(path, producer);
    walk_tree(
        path,
        |relative| vendor.digest(relative),
        |entry, relative, metadata, fixture| {
            verify_cargo_entry(producer, path, relative, metadata)?;
            inspect(entry, relative, metadata)?;
            let stamp = filesystem::stamp(entry, metadata, fixture)?;
            if stamp.identity.device != parent_identity.device {
                bail!(
                    "Output crosses a filesystem boundary: {}",
                    inventory::display(relative)
                );
            }
            if let Some(digest) = fixture {
                fixture_digests.insert(relative.to_owned(), digest);
            }
            entries.insert(relative.to_owned(), stamp);
            Ok(())
        },
    )?;
    Ok(Tree {
        producer,
        parent: parent_identity,
        entries,
        fixture_digests,
    })
}

fn walk_tree(
    path: &Path,
    mut verified_fixture: impl FnMut(&Path) -> Option<[u8; 32]>,
    mut visit: impl FnMut(&Path, &Path, &fs::Metadata, Option<[u8; 32]>) -> Result<()>,
) -> Result<()> {
    let parent = path.parent().context("Cannot stage a filesystem root")?;
    #[cfg(target_os = "linux")]
    let parent_mount = filesystem::mount_id(parent)?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    let mut remaining = vec![path.to_owned()];
    while let Some(entry) = remaining.pop() {
        let relative = entry.strip_prefix(path)?;
        let name = if relative.as_os_str().is_empty() {
            entry.as_path()
        } else {
            relative
        };
        let path_label = || inventory::display(name);
        let metadata = fs::symlink_metadata(&entry).with_context(|| {
            Message::new(
                format!("Cannot inspect {}", path_label()),
                format!("ファイルの情報を確認できません: {}", path_label()),
            )
        })?;
        #[cfg(target_os = "linux")]
        if filesystem::mount_id(&entry)? != parent_mount {
            return Err(Message::new(
                format!("Output crosses a mount boundary: {}", path_label()),
                format!("別にマウントされた領域を含みます: {}", path_label()),
            )
            .into());
        }
        if inventory::is_link(&metadata) {
            return Err(Message::new(
                format!(
                    "Contains a symbolic link or reparse point: {}",
                    path_label()
                ),
                format!("リンクを含むため保護: {}", path_label()),
            )
            .into());
        }
        if !inventory::same_device(&parent_metadata, &metadata) {
            return Err(Message::new(
                format!("Output crosses a filesystem boundary: {}", path_label()),
                format!("別のファイルシステムを含みます: {}", path_label()),
            )
            .into());
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(Message::new(
                format!("Contains an unsupported file type: {}", path_label()),
                format!("対応していない種類のファイルを含みます: {}", path_label()),
            )
            .into());
        }
        if evidence_name(&entry) {
            return Err(evidence_reason(name).into());
        }
        let needs_protection = !relative.as_os_str().is_empty() && protected(&entry);
        let fixture = if needs_protection && metadata.is_file() && vendor::eligible_name(relative) {
            verified_fixture(relative)
        } else {
            None
        };
        if (needs_protection && fixture.is_none())
            || (!relative.as_os_str().is_empty() && game_storage(&entry).is_some())
        {
            let (english, japanese) = if entry
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
            {
                ("Protected Git metadata", "Git の管理情報を保護")
            } else {
                match entry
                    .extension()
                    .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
                    .as_deref()
                {
                    Some("pem" | "key" | "p12" | "pfx" | "crt" | "cer" | "der") => (
                        "Protected key or certificate",
                        "鍵・証明書形式のファイルを保護",
                    ),
                    Some("sqlite" | "sqlite3" | "db" | "dump") => {
                        ("Protected database or dump", "データベースやダンプを保護")
                    }
                    _ => (
                        "Contains a protected entry",
                        "削除から保護している項目を含みます",
                    ),
                }
            };
            let label = path_label();
            return Err(Message::new(
                format!("{english}: {label}"),
                format!("{japanese}: {label}"),
            )
            .into());
        }
        visit(&entry, relative, &metadata, fixture)?;
        if metadata.is_dir() {
            let read = || {
                Message::new(
                    format!("Cannot list {}", path_label()),
                    format!("フォルダの中身を確認できません: {}", path_label()),
                )
            };
            for child in fs::read_dir(&entry).with_context(read)? {
                remaining.push(child.with_context(read)?.path());
            }
        }
    }
    Ok(())
}

pub fn verify_staged(tree: &Tree, payload: &Path) -> Result<()> {
    let mut seen = 0;
    #[cfg(target_os = "macos")]
    let (mut changed_directories, mut finder_parents) = (HashSet::new(), HashSet::new());
    walk_tree(
        payload,
        |relative| tree.fixture_digests.get(relative).copied(),
        |entry, relative, metadata, fixture| {
            #[cfg(target_os = "macos")]
            if !tree.entries.contains_key(relative) && filesystem::finder::recognized(entry) {
                finder_parents.insert(
                    relative
                        .parent()
                        .context("Missing metadata parent")?
                        .to_owned(),
                );
                return Ok(());
            }
            let expected = tree
                .entries
                .get(relative)
                .context("An unreviewed entry appeared in staging")?;
            let mut actual = filesystem::stamp(entry, metadata, fixture)?;
            if relative.as_os_str().is_empty() {
                // Moving the root into staging can change its ctime.
                actual.changed = expected.changed;
            }
            #[cfg(target_os = "macos")]
            if actual != *expected
                && actual.is_dir
                && expected.is_dir
                && actual.identity == expected.identity
            {
                changed_directories.insert(relative.to_owned());
                seen += 1;
                return Ok(());
            }
            if actual != *expected {
                bail!("The staged output differs from what was reviewed");
            }
            seen += 1;
            Ok(())
        },
    )?;
    #[cfg(target_os = "macos")]
    if !changed_directories.is_subset(&finder_parents) {
        bail!("Staged directory changed after review");
    }
    if seen != tree.entries.len() {
        bail!("A reviewed entry is missing from staging");
    }
    Ok(())
}

fn evidence_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "logs"
            | "test-results"
            | "test-reports"
            | "surefire-reports"
            | "junit.xml"
            | "test-results.xml"
    ) || lower.strip_suffix(".json").is_some_and(|stem| {
        matches!(
            stem.rsplit(['-', '_', '.']).next(),
            Some("result" | "results" | "report")
        )
    }) || lower.ends_with(".log")
        || lower.ends_with(".xcresult")
        || lower.ends_with(".trx")
}

fn evidence_reason(path: &Path) -> Message {
    let label = inventory::display(path);
    Message::new(
        format!("May contain work logs or test results: {label}"),
        format!("作業ログ・テスト結果の可能性があるため保留: {label}"),
    )
}

pub fn protected(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    protected_name(&name)
        || matches!(
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some(
                "pem"
                    | "key"
                    | "p12"
                    | "pfx"
                    | "crt"
                    | "cer"
                    | "der"
                    | "sqlite"
                    | "sqlite3"
                    | "db"
                    | "dump"
            )
        )
}

fn protected_name(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".env"
            | ".envrc"
            | ".ssh"
            | ".still-active"
            | ".keep"
            | ".gitkeep"
            | "id_rsa"
            | "id_ed25519"
            | ".docker"
            | ".colima"
            | "docker.raw"
            | "docker.qcow2"
    ) || name.starts_with(".env.")
        || name.starts_with(".still-stage-")
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !inventory::is_link(&m))
}

fn directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !inventory::is_link(&m))
}

fn game_storage(path: &Path) -> Option<Producer> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    if name == "Engine"
        && directory(&path.join("Build"))
        && regular_file(&path.join("Build/Build.version"))
    {
        return Some(Producer::UnrealEngine);
    }
    if matches!(name, "Assets" | "Library")
        && directory(&parent.join("Assets"))
        && regular_file(&parent.join("ProjectSettings/ProjectVersion.txt"))
        && regular_file(&parent.join("Packages/manifest.json"))
    {
        return match name {
            "Assets" => Some(Producer::UnityAssets),
            "Library"
                if regular_file(&path.join("ArtifactDB"))
                    && regular_file(&path.join("SourceAssetDB")) =>
            {
                Some(Producer::UnityLibrary)
            }
            _ => None,
        };
    }
    if matches!(name, "DerivedDataCache" | "Content" | "Saved")
        && directory(&parent.join("Content"))
        && fs::read_dir(parent).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "uproject")
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
            })
        })
    {
        return Some(if name == "DerivedDataCache" {
            Producer::UnrealCache
        } else {
            Producer::UnrealProjectData
        });
    }
    None
}

fn relocated_cargo(path: &Path) -> bool {
    let tag = path.join("CACHEDIR.TAG");
    let cargo_tag = regular_file(&tag)
        && fs::File::open(&tag).is_ok_and(|file| {
            let mut header = Vec::new();
            file.take(256).read_to_end(&mut header).is_ok()
                && header.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55")
                && header
                    .windows(b"cache directory tag created by cargo".len())
                    .any(|part| part == b"cache directory tag created by cargo")
        });
    if !regular_file(&path.join(".rustc_info.json")) && !cargo_tag {
        return false;
    }
    fn has_profile(path: &Path) -> bool {
        directory(&path.join(".fingerprint")) && directory(&path.join("deps"))
    }
    fn subdirs(path: &Path) -> impl Iterator<Item = PathBuf> {
        fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
    }
    subdirs(path).any(|dir| {
        has_profile(&dir)
            || (dir
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains('-'))
                && subdirs(&dir).any(|profile| has_profile(&profile)))
    })
}

fn python_bytecode(cache: &Path, parent: &Path) -> bool {
    if parent.ancestors().any(|dir| {
        dir.file_name().is_some_and(|name| {
            [
                ".venv",
                "venv",
                ".tox",
                ".nox",
                "site-packages",
                "node_modules",
                ".direnv",
            ]
            .iter()
            .any(|environment| name.eq_ignore_ascii_case(environment))
        })
    }) {
        return false;
    }
    let Ok(mut entries) = fs::read_dir(cache).map(Iterator::peekable) else {
        return false;
    };
    entries.peek().is_some()
        && entries.all(|entry| {
            let Ok(entry) = entry else { return false };
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".pyc")) else {
                return false;
            };
            let stem = stem
                .strip_suffix(".opt-1")
                .or_else(|| stem.strip_suffix(".opt-2"))
                .unwrap_or(stem);
            let Some((source, version)) = stem.rsplit_once(".cpython-") else {
                return false;
            };
            !source.is_empty()
                && !version.is_empty()
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && regular_file(&entry.path())
                && regular_file(&parent.join(format!("{source}.py")))
        })
}

fn has_dependency(project: &Path, package: &str) -> bool {
    let manifest = project.join("package.json");
    if !regular_file(&manifest) {
        return false;
    }
    let mut bytes = Vec::new();
    if fs::File::open(manifest)
        .and_then(|file| file.take(1024 * 1024).read_to_end(&mut bytes))
        .is_err()
    {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    ["dependencies", "devDependencies"]
        .iter()
        .any(|field| value[field][package].is_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Policy) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let output = root.join("target");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("build.bin"), b"generated").unwrap();
        let policy = Policy {
            roots: vec![root],
            outputs: vec![output.clone()],
            docker_context: None,
        };
        (temporary, output, policy)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staged_verification_accepts_only_new_regular_finder_metadata() -> Result<()> {
        let (_temp, output, policy) = fixture();
        fs::create_dir(output.join("debug"))?;
        let prepared = policy.prepare(&output)?;
        let staged = output.with_file_name("payload");
        fs::rename(output, &staged)?;
        fs::write(staged.join("debug/.DS_Store"), b"\0\0\0\x01Bud1metadata")?;
        verify_staged(&prepared.tree, &staged)?;
        fs::write(staged.join("debug/keep.txt"), b"user file")?;
        assert!(verify_staged(&prepared.tree, &staged).is_err());
        Ok(())
    }

    #[test]
    fn mutation_after_selection_invalidates_approval() {
        let (_temporary, output, policy) = fixture();
        let approved = policy.prepare(&output).unwrap();
        fs::write(output.join("build.bin"), b"new bytes").unwrap();
        assert!(policy.remove(&approved).is_err());
        assert_eq!(fs::read(output.join("build.bin")).unwrap(), b"new bytes");
    }

    #[test]
    fn same_size_content_change_is_detected_even_if_timestamps_match() {
        let (_temporary, output, policy) = fixture();
        let approved = policy.prepare(&output).unwrap();
        fs::write(output.join("build.bin"), b"new bytes").unwrap();
        let mut current = policy.prepare(&output).unwrap();
        let old = approved.tree.entries.get(Path::new("build.bin")).unwrap();
        let new = current
            .tree
            .entries
            .get_mut(Path::new("build.bin"))
            .unwrap();
        assert_eq!(old.len, new.len);
        new.modified = old.modified;
        new.changed = old.changed;
        assert_ne!(old.contents, new.contents);
        assert_ne!(approved.tree, current.tree);
    }

    #[test]
    fn test_evidence_added_after_review_keeps_output_intact() {
        let (_temporary, output, policy) = fixture();
        let approved = policy.prepare(&output).unwrap();
        fs::create_dir(output.join("test-results")).unwrap();
        let error = policy.remove(&approved).unwrap_err();
        assert!(error.to_string().contains("test-results"));
        assert!(output.join("build.bin").is_file());
    }

    #[test]
    fn protects_credentials_and_nested_repos() {
        let (_temporary, output, policy) = fixture();
        fs::create_dir(output.join(".git")).unwrap();
        assert!(policy.prepare(&output).is_err());
        let (_temporary, output, policy) = fixture();
        fs::write(output.join(".env"), "").unwrap();
        assert!(policy.prepare(&output).is_err());
    }

    #[test]
    fn held_target_exposes_only_independently_verified_build_caches() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        let target = root.join("target");
        let profile = target.join("debug");
        let deps = profile.join("deps");
        let incremental = profile.join("incremental");
        fs::create_dir_all(&deps)?;
        fs::create_dir(&incremental)?;
        fs::create_dir(profile.join(".fingerprint"))?;
        fs::write(profile.join(".cargo-lock"), b"")?;
        fs::write(target.join(".rustc_info.json"), b"{}")?;
        fs::write(root.join("Cargo.toml"), b"[workspace]")?;
        fs::write(root.join(".gitignore"), b"/target/")?;
        git::check(git::run(&root, ["init", "-q"])?, "init")?;
        let cert = target.join("cacert.pem");
        let log = target.join("cargo-test.log");
        let result = deps.join("test-result.json");
        fs::write(&cert, b"certificate")?;
        fs::write(&log, b"test evidence")?;
        fs::write(&result, b"{}")?;
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: vec![],
            docker_context: None,
        };
        let scan = inventory::scan(&root, 2, None)?;
        let rows = file_candidates(&scan, &policy);
        let eligible = |path: &Path| {
            rows.iter()
                .find(|c| c.row.path == path)
                .map(|c| c.blocked_reason.is_none())
        };
        assert_eq!(eligible(&target), Some(false));
        assert_eq!(eligible(&deps), Some(false));
        assert_eq!(eligible(&incremental), Some(true));
        let approved = policy.prepare(&incremental)?;
        fs::write(incremental.join("new-result.json"), b"evidence")?;
        assert!(policy.remove(&approved).is_err());
        assert!(cert.exists() && log.exists() && result.exists() && incremental.exists());
        // A later review still protects result JSON even without a certificate or log.
        fs::remove_file(&cert)?;
        fs::remove_file(&log)?;
        assert!(policy.prepare(&target).is_err());
        assert!(result.exists());
        Ok(())
    }

    #[test]
    fn finished_units_remain_reviewable_beside_an_unfinished_build() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        let target = root.join("target");
        let profile = target.join("debug");
        let incremental = profile.join("incremental");
        let completed = incremental.join("demo-0123456789abc");
        let running = incremental.join("other-0123456789abc");
        let session = "s-ho9wx39w3k-0u5spzg";
        let generation = format!("{session}-0xny0l54dee30tk2bxf7bttju");
        for unit in [&completed, &running] {
            fs::create_dir_all(unit.join(&generation))?;
            fs::write(unit.join(&generation).join("dep-graph.bin"), b"generated")?;
            fs::write(unit.join(format!("{session}.lock")), b"")?;
        }
        let working = running.join(format!("{session}-working"));
        fs::create_dir(&working)?;
        fs::create_dir(profile.join(".fingerprint"))?;
        fs::write(profile.join(".cargo-lock"), b"")?;
        fs::write(target.join(".rustc_info.json"), b"{}")?;
        fs::write(root.join("Cargo.toml"), b"[workspace]")?;
        fs::write(root.join(".gitignore"), b"/target/")?;
        git::check(git::run(&root, ["init", "-q"])?, "init")?;
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: vec![],
            docker_context: None,
        };
        let scan = inventory::scan(&root, 2, None)?;
        let rows = file_candidates(&scan, &policy);
        let eligible = |path: &Path| {
            rows.iter()
                .any(|row| row.row.path == path && row.blocked_reason.is_none())
        };
        assert!(eligible(&completed));
        for path in [&running, &incremental, &target] {
            assert!(!eligible(path));
            assert!(policy.prepare(path).is_err());
        }
        let reviewed = policy.prepare(&completed)?;
        fs::write(target.join(".keep"), b"keep all outputs")?;
        assert!(policy.remove(&reviewed).is_err());
        assert!(completed.is_dir());
        fs::remove_file(target.join(".keep"))?;
        fs::write(completed.join("my-notes.txt"), b"preserve")?;
        assert!(policy.remove(&reviewed).is_err());
        assert!(working.is_dir());
        assert!(completed.join("my-notes.txt").is_file());
        fs::remove_file(completed.join("my-notes.txt"))?;
        git::check(
            git::run(&root, ["add", "-f", "--", completed.to_str().unwrap()])?,
            "add fixture",
        )?;
        assert!(policy.prepare(&completed).is_err());
        assert!(!file_candidates(&scan, &policy)
            .iter()
            .any(|c| c.row.path == completed && c.blocked_reason.is_none()));
        Ok(())
    }

    #[test]
    fn matches_only_cpython_cache_with_consistent_sources() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let cache = root.join("__pycache__");
        fs::create_dir(&cache)?;
        fs::write(root.join("module.py"), b"source")?;
        fs::write(cache.join("module.cpython-313.opt-1.pyc"), b"bytecode")?;
        assert!(python_bytecode(&cache, &root));
        fs::write(cache.join("orphan.cpython-313.pyc"), b"bytecode")?;
        assert!(!python_bytecode(&cache, &root));
        Ok(())
    }

    #[test]
    fn node_modules_is_visible_but_protected_and_does_not_expose_package_outputs() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let project = root.join("project");
        let modules = project.join("node_modules");
        let package = modules.join("some-package");
        let dist = package.join("dist");
        fs::create_dir_all(dist.join("assets"))?;
        fs::write(project.join("package.json"), b"{}")?;
        fs::write(
            package.join("package.json"),
            b"{\"devDependencies\":{\"vite\":\"1\"}}",
        )?;
        fs::write(dist.join("index.html"), b"generated")?;
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: vec![],
            docker_context: None,
        };
        let scan = inventory::scan(&root, 2, None)?;
        let candidates = file_candidates(&scan, &policy);
        let row = candidates
            .iter()
            .find(|candidate| candidate.row.path == modules)
            .unwrap();
        assert_eq!(row.producer, Producer::NodeModules);
        assert!(row.blocked_reason.is_some());
        assert!(policy.prepare(&modules).is_err());
        assert!(policy.producer(&dist).is_none());
        assert!(dist.join("index.html").exists());
        Ok(())
    }

    #[test]
    fn relocated_output_reports_ownership_and_tracked_file_blockers() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let project = root.join("project");
        let output = project.join("runs/cargo-output");
        let profile = output.join("aarch64-apple-darwin/custom");
        fs::create_dir_all(profile.join(".fingerprint"))?;
        fs::create_dir_all(profile.join("deps"))?;
        fs::write(output.join(".rustc_info.json"), b"{}")?;
        fs::write(output.join("aarch64-apple-darwin/.rustc_info.json"), b"{}")?;
        fs::write(profile.join("deps/demo.bin"), b"build output")?;
        fs::create_dir(project.join("target"))?;
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: Vec::new(),
            docker_context: None,
        };
        let scan = inventory::scan(&root, 2, None)?;
        let files = file_candidates(&scan, &policy);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].row.path, output);
        assert_eq!(files[0].producer, Producer::CargoRelocated);
        assert!(files[0]
            .blocked_reason
            .as_ref()
            .unwrap()
            .to_string()
            .contains("No enclosing Cargo project"));

        fs::write(project.join("Cargo.toml"), b"[workspace]\n")?;
        fs::write(project.join(".gitignore"), b"/runs/\n")?;
        git::check(git::run(&project, ["init", "-q"])?, "init")?;
        let scan = inventory::scan(&root, 2, None)?;
        assert!(file_candidates(&scan, &policy)[0].blocked_reason.is_none());
        git::check(
            git::run(
                &project,
                ["add", "-f", "--", "runs/cargo-output/.rustc_info.json"],
            )?,
            "add",
        )?;
        assert!(file_candidates(&scan, &policy)[0]
            .blocked_reason
            .as_ref()
            .unwrap()
            .to_string()
            .contains("tracked by Git"));
        assert!(profile.join("deps/demo.bin").is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn candidate_list_explains_nested_symlink_without_following_it() -> Result<()> {
        let (_temporary, output, policy) = fixture();
        let source = output.with_file_name("keep");
        fs::write(&source, b"keep")?;
        fs::create_dir(output.join("debug"))?;
        std::os::unix::fs::symlink(&source, output.join("debug/link"))?;
        let scan = inventory::scan(&policy.roots[0], 2, None)?;
        let files = file_candidates(&scan, &policy);
        assert_eq!(files.len(), 1);
        let blocked = files[0].blocked_reason.as_ref().unwrap().to_string();
        assert!(blocked.contains("symbolic link"));
        assert!(blocked.contains("debug/link"));
        assert_eq!(fs::read(&source)?, b"keep");
        Ok(())
    }

    #[test]
    fn unity_cache_requires_project_markers_and_stays_protected() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let library = root.join("Library");
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: Vec::new(),
            docker_context: None,
        };
        fs::create_dir(&library)?;
        fs::write(library.join("ArtifactDB"), b"data")?;
        fs::write(library.join("SourceAssetDB"), b"data")?;
        assert_eq!(policy.producer(&library), None);
        for dir in ["Assets", "Packages", "ProjectSettings"] {
            fs::create_dir(root.join(dir))?;
        }
        fs::write(root.join("Packages/manifest.json"), b"{}")?;
        fs::write(root.join("ProjectSettings/ProjectVersion.txt"), b"version")?;
        assert_eq!(policy.producer(&library), Some(Producer::UnityLibrary));
        let assets = root.join("Assets");
        assert_eq!(policy.producer(&assets), Some(Producer::UnityAssets));
        assert!(policy
            .prepare(&library)
            .err()
            .is_some_and(|error| error.to_string().contains("reimport")));
        Ok(())
    }

    #[test]
    fn unreal_project_data_cannot_be_declared_as_an_output() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().canonicalize()?;
        let project = root.join("project");
        let content = project.join("Content");
        let manually_declared = content.join("generated");
        let cache = project.join("DerivedDataCache");
        let saved = project.join("Saved");
        let engine = root.join("ue/Engine");
        for folder in [&manually_declared, &cache, &saved, &engine.join("Build")] {
            fs::create_dir_all(folder)?;
        }
        fs::write(project.join("test.uproject"), "{}")?;
        fs::write(engine.join("Build/Build.version"), "{}")?;
        let policy = Policy {
            roots: vec![root.clone()],
            outputs: vec![manually_declared.clone(), content.clone(), project.clone()],
            docker_context: None,
        };
        let scan = inventory::scan(&root, 2, None)?;
        let candidates = file_candidates(&scan, &policy);
        for path in [&content, &cache, &saved, &engine, &project] {
            assert!(candidates
                .iter()
                .any(|row| row.row.path == *path && row.blocked_reason.is_some()));
        }
        assert!(!candidates
            .iter()
            .any(|row| row.row.path == manually_declared));
        Ok(())
    }

    #[test]
    fn preserves_tracked_files_when_output_spelling_changes_case() {
        let (_temporary, output, policy) = fixture();
        let repo = output.parent().unwrap();
        let original = repo.join("TARGET-original");
        fs::rename(&output, &original).unwrap();
        let uppercase = repo.join("TARGET");
        fs::rename(&original, &uppercase).unwrap();
        git::check(git::run(repo, ["init", "-q"]).unwrap(), "init").unwrap();
        git::check(
            git::run(repo, ["add", "--force", "--", "TARGET/build.bin"]).unwrap(),
            "add",
        )
        .unwrap();
        fs::rename(&uppercase, &original).unwrap();
        fs::rename(&original, &output).unwrap();

        let error = policy.prepare(&output).err().expect("tracked output");
        assert!(error.to_string().contains("tracked by Git"));
        assert!(output.join("build.bin").is_file());
    }

    #[test]
    fn only_removes_the_confirmed_output() {
        let (_temporary, output, policy) = fixture();
        let keep = output.with_file_name("keep");
        fs::write(&keep, "keep").unwrap();
        #[cfg(unix)]
        fs::hard_link(output.join("build.bin"), output.join("linked.bin")).unwrap();
        let approved = policy.prepare(&output).unwrap();
        policy.remove(&approved).unwrap();
        assert!(!output.exists());
        assert_eq!(fs::read_to_string(&keep).unwrap(), "keep");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn removal_refuses_output_with_an_open_file() -> Result<()> {
        let (_temporary, output, policy) = fixture();
        let file = fs::File::open(output.join("build.bin"))?;
        let prepared = policy.prepare(&output)?;
        assert!(policy
            .remove(&prepared)
            .err()
            .is_some_and(|error| error.to_string().contains("files open")));
        assert!(output.is_dir());
        drop(file);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_cannot_redirect_removal() {
        use std::os::unix::fs::symlink;
        let (_temporary, output, mut policy) = fixture();
        let nested = output.join("dist");
        fs::create_dir(&nested).unwrap();
        policy.outputs.push(nested.clone());
        let approved = policy.prepare(&nested).unwrap();
        let moved = output.with_file_name("moved");
        fs::rename(&output, &moved).unwrap();
        symlink(&moved, &output).unwrap();
        assert!(policy.remove(&approved).is_err());
        assert!(moved.join("dist").is_dir());
    }
}
