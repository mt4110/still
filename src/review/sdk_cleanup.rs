mod residue;

use super::{app_storage, inventory, message::Message};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use ignore::WalkBuilder;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum Target {
    Runtime {
        image: Uuid,
        build: String,
    },
    ResidualRuntime {
        identifier: String,
        build: String,
    },
    Ndk {
        version: String,
        device: u64,
        inode: u64,
        modified: SystemTime,
    },
}

#[derive(Clone)]
pub(super) struct Item {
    pub target: Target,
    pub label: String,
    pub size: Option<u64>,
    pub basis: &'static str,
    pub reason: Message,
    pub selectable: bool,
}

#[derive(Default)]
pub(super) struct Report {
    pub items: Vec<Item>,
    pub notes: Vec<String>,
}

pub(super) struct Review {
    pub items: Vec<Item>,
    roots: Vec<PathBuf>,
}

impl Review {
    pub fn new(report: &Report, selected: &BTreeSet<Target>, roots: Vec<PathBuf>) -> Result<Self> {
        let items: Vec<_> = report
            .items
            .iter()
            .filter(|item| item.selectable && selected.contains(&item.target))
            .cloned()
            .collect();
        if items.is_empty() || items.len() != selected.len() {
            bail!("Select only versions marked as candidates");
        }
        Ok(Self { items, roots })
    }

    pub fn apply(self) -> Result<Receipt> {
        let machine = Machine::open(&self.roots)?;
        apply_selection(self, &machine)
    }
}

pub(super) struct Receipt {
    pub before: u64,
    pub after: Result<u64>,
    pub results: Vec<(String, Result<Removal>)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Removal {
    Removed,
    Partial { image_bytes: Option<u64> },
    Unverified,
}

trait Backend {
    fn scan(&self) -> Report;
    fn remove(&self, target: &Target) -> Result<Removal>;
    fn free(&self) -> Result<u64>;
}

fn apply_selection(review: Review, machine: &impl Backend) -> Result<Receipt> {
    let fresh = machine.scan();
    if let Some(item) = review
        .items
        .iter()
        .find(|item| !still_selectable(&fresh, item))
    {
        bail!(
            "{} changed or is now protected. Scan again to review it.",
            item.label
        );
    }
    let before = machine
        .free()
        .context("Could not measure free space before cleanup")?;
    let mut results = Vec::new();
    let mut stopped = false;
    for item in review.items {
        let result = if stopped {
            Err(anyhow::anyhow!(
                "Skipped because the previous removal was not confirmed"
            ))
        } else if !results.is_empty() && !still_selectable(&machine.scan(), &item) {
            Err(anyhow::anyhow!("Use or installation changed after review"))
        } else {
            machine.remove(&item.target)
        };
        stopped |= !matches!(result, Ok(Removal::Removed));
        results.push((item.label, result));
    }
    Ok(Receipt {
        before,
        after: machine.free(),
        results,
    })
}

fn still_selectable(report: &Report, selected: &Item) -> bool {
    report.items.iter().any(|live| {
        live.selectable && live.target == selected.target && live.label == selected.label
    })
}

pub(super) fn scan(roots: &[PathBuf]) -> Result<Report> {
    Ok(Machine::open(roots)?.scan())
}

struct Machine {
    home: PathBuf,
    sdk: PathBuf,
    roots: Vec<PathBuf>,
}

impl Machine {
    fn open(roots: &[PathBuf]) -> Result<Self> {
        let dirs = directories::BaseDirs::new().context("Could not locate home")?;
        let home = dirs.home_dir().canonicalize()?;
        let mut roots = roots.to_vec();
        for name in ["_workspace", "AndroidStudioProjects"] {
            let root = home.join(name);
            if fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir() || inventory::is_link(&m)) {
                roots.push(root);
            }
        }
        roots.sort();
        roots.dedup();
        Ok(Self {
            sdk: home.join("Library/Android/sdk"),
            home,
            roots,
        })
    }

    fn manager(&self) -> Result<PathBuf> {
        let bin = inventory::home_scan_dir(&self.home, &self.sdk.join("cmdline-tools/latest/bin"))?;
        let manager = bin.join("sdkmanager");
        let meta = manager.symlink_metadata()?;
        if !meta.is_file() || inventory::is_link(&meta) || meta.mode() & 0o111 == 0 {
            bail!("Android SDK Manager is unavailable");
        }
        Ok(manager)
    }

    fn ndk_root(&self) -> Result<PathBuf> {
        inventory::home_scan_dir(&self.home, &self.sdk.join("ndk"))
    }
}

impl Backend for Machine {
    fn scan(&self) -> Report {
        let mut result = Report::default();
        for (name, source) in [("Xcode", scan_xcode(self)), ("Android", scan_ndk(self))] {
            match source {
                Ok(mut items) => result.items.append(&mut items),
                Err(error) => result.notes.push(format!("{name}: {error:#}")),
            }
        }
        result.items.sort_by(|a, b| {
            b.selectable
                .cmp(&a.selectable)
                .then(b.size.cmp(&a.size))
                .then(a.label.cmp(&b.label))
        });
        result
    }

    fn remove(&self, target: &Target) -> Result<Removal> {
        match target {
            Target::Runtime { image, build } => remove_runtime(*image, build),
            Target::ResidualRuntime { .. } => {
                bail!("Residual images are not removable through Xcode")
            }
            Target::Ndk { version, .. } => {
                let ndk = self.ndk_root()?;
                let path = ndk.join(version);
                if ndk_identity(&path, version)? != *target {
                    bail!("NDK directory changed after review");
                }
                let before: Vec<_> = installed_ndks(&ndk)?
                    .into_iter()
                    .filter(|(name, _, _)| name != version)
                    .map(|(_, target, _)| target)
                    .collect();
                let package = format!("ndk;{version}");
                let sdk_root = format!("--sdk_root={}", self.sdk.display());
                let tool = app_storage::command_output(
                    Command::new(self.manager()?).args([
                        sdk_root.as_str(),
                        "--uninstall",
                        &package,
                    ]),
                    Duration::from_secs(90),
                );
                let after: Vec<_> = match installed_ndks(&ndk) {
                    Ok(rows) => rows.into_iter().map(|(_, target, _)| target).collect(),
                    Err(error) if missing(&error) && !ndk.exists() => Vec::new(),
                    Err(error) => return Err(error),
                };
                if !matches!(path.symlink_metadata(), Err(error) if error.kind() == ErrorKind::NotFound)
                    || before != after
                {
                    tool?;
                    bail!("SDK Manager did not confirm removal of only the selected version");
                }
                Ok(Removal::Removed)
            }
        }
    }

    fn free(&self) -> Result<u64> {
        Ok(volume(Path::new("/System/Volumes/Data"))?.1)
    }
}

#[derive(Deserialize)]
struct SimulatorList {
    runtimes: Vec<Simulator>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Simulator {
    identifier: String,
    buildversion: String,
    is_available: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    last_usage: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct DeviceList {
    devices: HashMap<String, Vec<Device>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    udid: Uuid,
    state: String,
    #[serde(default)]
    data_path: PathBuf,
}

#[derive(Deserialize)]
struct PairList {
    pairs: HashMap<String, Pair>,
}

#[derive(Deserialize)]
struct Pair {
    phone: PairedDevice,
    watch: PairedDevice,
}

#[derive(Deserialize)]
struct PairedDevice {
    udid: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreferredRuntime {
    chosen_runtime_build: String,
}

fn scan_xcode(machine: &Machine) -> Result<Vec<Item>> {
    let images = app_storage::runtimes()?;
    let SimulatorList { runtimes } = app_storage::simctl_json(&["list", "runtimes", "-j"])?;
    let residuals = residual_items(&images, &runtimes);
    if images.is_empty() {
        return Ok(residuals);
    }
    let DeviceList { devices } = app_storage::simctl_json(&["list", "devices", "-j"])?;
    let PairList { pairs } = app_storage::simctl_json(&["list", "pairs", "-j"])?;
    let preferred: BTreeMap<String, PreferredRuntime> =
        app_storage::simctl_json(&["runtime", "match", "list", "-j"])?;
    let paired: HashSet<_> = pairs
        .values()
        .flat_map(|pair| [pair.phone.udid, pair.watch.udid])
        .collect();
    let cutoff = Utc::now() - TimeDelta::days(45);
    let mut items: Vec<_> = images
        .iter()
        .map(|(id, image)| {
            let simulator = runtimes.iter().find(|r| {
                r.identifier == image.runtime_identifier && r.buildversion == image.build
            });
            let version = numeric_version(&image.version);
            let newer = version.is_some()
                && images.values().any(|other| {
                    other.platform_identifier == image.platform_identifier
                        && other.state == "Ready"
                        && numeric_version(&other.version) > version
                        && runtimes.iter().any(|r| {
                            r.identifier == other.runtime_identifier
                                && r.buildversion == other.build
                                && r.is_available
                        })
                });
            let is_preferred = preferred
                .values()
                .any(|p| p.chosen_runtime_build == image.build);
            let device_group = devices
                .get(&image.runtime_identifier)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            // If multiple images share a runtime identifier, the device list cannot
            // reliably tell us which image owns those devices.
            let ambiguous = images
                .values()
                .filter(|other| other.runtime_identifier == image.runtime_identifier)
                .count()
                > 1;
            let decision = if image.deletable != Some(true)
                || image.state != "Ready"
                || ambiguous
                || !simulator.is_some_and(|r| r.is_available)
                || preferred.is_empty()
            {
                blocked(
                    "Xcode cannot confirm that this exact runtime can be reviewed.",
                    "このランタイムを個別に確認できないため保護しています。",
                )
            } else if !newer || is_preferred {
                blocked(
                    "Newest available runtime or selected by the current Xcode SDK.",
                    "利用可能な最新版か、現在の Xcode SDK が選んでいる版です。",
                )
            } else if device_group
                .iter()
                .any(|d| d.state != "Shutdown" || paired.contains(&d.udid))
            {
                blocked(
                    "A simulator is running or paired with another device.",
                    "起動中か、別の端末とペアになっているシミュレータがあります。",
                )
            } else {
                match device_group.iter().try_fold(false, |found, device| {
                    Ok::<_, anyhow::Error>(found || device_has_apps(&machine.home, device)?)
                }) {
                    Err(_) => blocked(
                        "Could not check the simulator apps.",
                        "シミュレータ内のアプリを確認できないため保護しています。",
                    ),
                    Ok(true) => blocked(
                        "A simulator still has installed apps or test runners.",
                        "シミュレータにインストール済みアプリやテストランナーが残っています。",
                    ),
                    Ok(false) => usage_decision(simulator.expect("checked above"), cutoff),
                }
            };
            Ok(Item {
                target: Target::Runtime {
                    image: Uuid::parse_str(id)?,
                    build: image.build.clone(),
                },
                label: image.label(),
                size: image.size_bytes,
                basis: "Xcode estimate",
                reason: decision.1,
                selectable: decision.0,
            })
        })
        .collect::<Result<_>>()?;
    protect_runtime_references(machine, &mut items, &images);
    items.extend(residuals);
    Ok(items)
}

fn residual_items(
    images: &BTreeMap<String, app_storage::Runtime>,
    runtimes: &[Simulator],
) -> Vec<Item> {
    let mut seen = HashSet::new();
    runtimes
        .iter()
        .filter_map(|runtime| {
            if images.values().any(|image| {
                image.runtime_identifier == runtime.identifier
                    && image.build == runtime.buildversion
            }) || !seen.insert((&runtime.identifier, &runtime.buildversion))
            {
                return None;
            }
            let storage = residue::probe(&runtime.platform, &runtime.buildversion, None);
            if !storage.has_image() {
                return None;
            }
            let label = if runtime.name.is_empty() {
                &runtime.identifier
            } else {
                &runtime.name
            };
            Some(Item {
                target: Target::ResidualRuntime {
                    identifier: runtime.identifier.clone(),
                    build: runtime.buildversion.clone(),
                },
                label: format!("{label} runtime ({})", runtime.buildversion),
                size: storage.bytes(),
                basis: "remaining image",
                reason: Message::new(
                    "A disk image remains and Xcode does not list it for removal.",
                    "ディスクイメージが残っています。Xcode の削除管理一覧には表示されません。",
                ),
                selectable: false,
            })
        })
        .collect()
}

fn protect_runtime_references(
    machine: &Machine,
    items: &mut [Item],
    images: &BTreeMap<String, app_storage::Runtime>,
) {
    let candidates: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let Target::Runtime { image, .. } = item.target else {
                return None;
            };
            if !item.selectable {
                return None;
            }
            let record = images.get(&image.to_string().to_uppercase())?;
            Some((index, record.platform(), record.version.as_str()))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }

    let mut references = HashMap::new();
    let mut incomplete = false;
    for root in &machine.roots {
        let Ok(root) = inventory::home_scan_dir(&machine.home, root) else {
            incomplete = true;
            continue;
        };
        for entry in project_walker(&root) {
            let Ok(entry) = entry else {
                incomplete = true;
                continue;
            };
            let extension = entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("");
            let plan = matches!(extension, "xctestplan" | "xcscheme");
            if !plan
                && !matches!(extension, "xcconfig" | "sh" | "yml" | "yaml")
                && !matches!(entry.file_name().to_str(), Some("Fastfile" | "Makefile"))
            {
                continue;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(content) = read_small(entry.path(), 2 * 1024 * 1024) else {
                incomplete = true;
                continue;
            };
            for &(index, platform, version) in &candidates {
                if !references.contains_key(&index)
                    && runtime_version_mentioned(&content, platform, version, plan)
                {
                    references.insert(index, entry.path().to_owned());
                }
            }
        }
    }
    for (index, _, _) in candidates {
        let item = &mut items[index];
        if let Some(reference) = references.get(&index) {
            let name =
                inventory::display(reference.strip_prefix(&machine.home).unwrap_or(reference));
            item.selectable = false;
            item.reason = Message::new(
                format!("A local test plan or script references this version · ~/{name}"),
                format!("ローカルのテスト設定かスクリプトがこの版を参照 · ~/{name}"),
            );
        } else if incomplete {
            (item.selectable, item.reason) = blocked(
                "Could not complete the local test configuration check.",
                "ローカルのテスト設定を最後まで確認できませんでした。",
            );
        }
    }
}

fn runtime_version_mentioned(content: &str, platform: &str, version: &str, plan: bool) -> bool {
    let dashed_version = version.replace('.', "-");
    [
        format!("{platform} {version}"),
        format!("{platform}-{dashed_version}"),
        format!("OS={version}"),
    ]
    .iter()
    .any(|needle| {
        content.match_indices(needle).any(|(offset, text)| {
            content[offset + text.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_digit() && c != '.' && c != '-')
        })
    }) || (plan
        && content
            .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .any(|word| word == version))
}

fn usage_decision(simulator: &Simulator, cutoff: DateTime<Utc>) -> (bool, Message) {
    if simulator.last_usage.is_empty() {
        return blocked(
            "Xcode did not provide a use record.",
            "Xcode から利用記録を取得できないため保護しています。",
        );
    }
    let dates = simulator
        .last_usage
        .values()
        .filter(|text| !text.starts_with("0001-01-01T"))
        .map(|text| text.parse::<DateTime<Utc>>())
        .collect::<Result<Vec<_>, _>>();
    match dates {
        Err(_) => blocked("Xcode reported an unreadable use date.", "Xcode の最終利用時刻を判定できないため保護しています。"),
        Ok(dates) => match dates.into_iter().max() {
            Some(date) => (date <= cutoff, Message::new(
                format!("Last used {}. Review threshold is 45 days.", date.format("%Y-%m-%d")),
                format!("最終利用は {}。候補の基準は45日です。", date.format("%Y-%m-%d")),
            )),
            None => (true, Message::new(
                "No use recorded by Xcode. No running, paired or app-bearing devices were found.",
                "Xcode に利用記録がなく、起動中・ペア済み・アプリ入りの端末が見つかりませんでした。",
            )),
        }
    }
}

fn blocked(english: &str, japanese: &str) -> (bool, Message) {
    (false, Message::new(english, japanese))
}

fn device_has_apps(home: &Path, device: &Device) -> Result<bool> {
    // Override device sets may point elsewhere. Refuse to inspect those paths.
    let root = home.join("Library/Developer/CoreSimulator/Devices");
    if device.data_path
        != root
            .join(device.udid.hyphenated().to_string().to_uppercase())
            .join("data")
    {
        bail!("Unexpected simulator data location");
    }
    inventory::home_scan_dir(home, &device.data_path)?;
    for kind in [
        "Containers/Bundle/Application",
        "Containers/Data/Application",
    ] {
        let path = device.data_path.join(kind);
        match inventory::home_scan_dir(home, &path) {
            Ok(path) if fs::read_dir(&path)?.next().transpose()?.is_some() => return Ok(true),
            Ok(_) => {}
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn remove_runtime(image: Uuid, build: &str) -> Result<Removal> {
    let before = app_storage::runtimes()?;
    let id = image.hyphenated().to_string().to_uppercase();
    let exact = before.get(&id).context("Runtime no longer installed")?;
    if exact.build != build || exact.deletable != Some(true) {
        bail!("Runtime identity changed after review");
    }
    let command = |dry_run| {
        let mut tool = Command::new("/usr/bin/xcrun");
        tool.args(["simctl", "runtime", "delete", &id]);
        if dry_run {
            tool.arg("--dry-run");
        }
        app_storage::command_output(
            &mut tool,
            Duration::from_secs(if dry_run { 12 } else { 75 }),
        )
    };
    let preview = command(true)?;
    let matches: Vec<_> = preview
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if matches.len() != 1
        || !matches[0].contains("Would delete")
        || !matches[0].split_whitespace().any(|token| token == id)
    {
        bail!("Xcode did not confirm exactly one selected runtime");
    }
    let result = command(false);
    for attempt in 0..6 {
        let after = app_storage::runtimes()?;
        if !after.contains_key(&id) {
            if after.len() + 1 != before.len()
                || before.iter().any(|(key, item)| {
                    key != &id
                        && after
                            .get(key)
                            .is_none_or(|current| current.build != item.build)
                })
            {
                bail!("Xcode runtime inventory changed outside the selected item");
            }
            return Ok(verify_runtime_removal(exact));
        }
        if result.is_err() || attempt == 5 {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    result?;
    bail!("Xcode did not confirm removal of the selected runtime")
}

fn verify_runtime_removal(runtime: &app_storage::Runtime) -> Removal {
    let registered =
        app_storage::simctl_json::<SimulatorList>(&["list", "runtimes", "-j"]).map(|list| {
            list.runtimes.iter().any(|entry| {
                entry.identifier == runtime.runtime_identifier
                    && entry.buildversion == runtime.build
            })
        });
    let mut storage = residue::probe(runtime.platform(), &runtime.build, runtime.path.as_deref());
    storage.incomplete |= runtime.path.is_none();
    classify_removal(registered, storage)
}

fn classify_removal(registered: Result<bool>, storage: residue::Images) -> Removal {
    if storage.path_present || storage.has_image() || matches!(registered, Ok(true)) {
        Removal::Partial {
            image_bytes: storage.bytes(),
        }
    } else if registered.is_err() || storage.incomplete {
        Removal::Unverified
    } else {
        Removal::Removed
    }
}

fn numeric_version(version: &str) -> Option<Vec<u32>> {
    let numbers: Vec<u32> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    (numbers.len() >= 2).then_some(numbers)
}

fn scan_ndk(machine: &Machine) -> Result<Vec<Item>> {
    let root = match machine.ndk_root() {
        Ok(root) => root,
        Err(error) if missing(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let installed = installed_ndks(&root)?;
    if installed.is_empty() {
        return Ok(Vec::new());
    }
    let project = project_evidence(
        machine,
        installed
            .iter()
            .map(|(version, _, _)| version.as_str())
            .collect(),
    );
    let processes = app_storage::command_output(
        Command::new("/bin/ps").args(["-ww", "-axo", "comm="]),
        Duration::from_secs(8),
    )?;
    let same_volume = volume(&root)?.0 == volume(Path::new("/System/Volumes/Data"))?.0;
    let manager = machine.manager().is_ok();
    installed.into_iter().map(|(version, target, path)| {
        let mut du = Command::new("/usr/bin/du");
        du.arg("-skx").arg(&path);
        let size = app_storage::command_output(&mut du, Duration::from_secs(20)).ok()
            .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok()?.checked_mul(1024));
        let in_use = processes.lines().any(|line| line.trim_start().starts_with(&format!("{}/", path.display())))
            || ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"].into_iter().any(|var| {
                std::env::var_os(var).is_some_and(|value| Path::new(&value).starts_with(&path))
            });
        let decision = if in_use {
            blocked("A running process or NDK environment setting uses this version.", "実行中のプロセスか NDK の環境設定がこの版を使っています。")
        } else if let Some(reference) = project.references.get(&version) {
            let file = inventory::display(reference.strip_prefix(&machine.home).unwrap_or(reference));
            (false, Message::new(
                format!("A project setting or native build record references this version · ~/{file}"),
                format!("プロジェクト設定かネイティブビルドの記録でこの版を確認 · ~/{file}"),
            ))
        } else if project.incomplete || machine.roots.is_empty() {
            blocked("Could not complete the project dependency check.", "プロジェクト側の依存関係を最後まで確認できませんでした。")
        } else if project.unresolved_native {
            blocked("A native Android project does not identify which NDK version it uses.", "NDK の版を特定できないネイティブ Android プロジェクトがあります。")
        } else if !same_volume || !manager || size.is_none() {
            blocked("Removal requires a readable package on the Mac data volume and an installed SDK Manager.", "削除には、Mac のデータボリュームで読み取れるパッケージと SDK Manager が必要です。")
        } else {
            (true, Message::new(
                "No reference found in the checked project settings or native build records. Gradle may select a version implicitly.",
                "調べた設定とネイティブビルド記録に参照がありません。Gradle が版を自動で選ぶ場合があります。",
            ))
        };
        Ok(Item { target, label: format!("Android NDK {version}"), size, basis: "allocated", reason: decision.1, selectable: decision.0 })
    }).collect()
}

fn installed_ndks(root: &Path) -> Result<Vec<(String, Target, PathBuf)>> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let version = entry.file_name();
        let Some(version) = version
            .to_str()
            .filter(|name| name.split('.').count() == 3 && numeric_version(name).is_some())
        else {
            continue;
        };
        let path = entry.path();
        rows.push((version.to_owned(), ndk_identity(&path, version)?, path));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

fn ndk_identity(path: &Path, version: &str) -> Result<Target> {
    let meta = path.symlink_metadata()?;
    if !meta.is_dir() || inventory::is_link(&meta) {
        bail!("NDK {version} is not a regular installed package");
    }
    let props = read_small(&path.join("source.properties"), 32 * 1024)?;
    if !props.lines().any(|line| {
        line.split_once('=')
            .is_some_and(|(key, value)| key.trim() == "Pkg.Revision" && value.trim() == version)
    }) {
        bail!("NDK {version} package revision does not match the directory");
    }
    Ok(Target::Ndk {
        version: version.into(),
        device: meta.dev(),
        inode: meta.ino(),
        modified: meta.modified()?,
    })
}

#[derive(Default)]
struct ProjectEvidence {
    references: BTreeMap<String, PathBuf>,
    unresolved_native: bool,
    incomplete: bool,
}

fn project_evidence(machine: &Machine, versions: Vec<&str>) -> ProjectEvidence {
    let mut report = ProjectEvidence::default();
    let mut projects: BTreeMap<PathBuf, (bool, bool)> = BTreeMap::new();
    for root in &machine.roots {
        let Ok(root) = inventory::home_scan_dir(&machine.home, root) else {
            report.incomplete = true;
            continue;
        };
        let is_collection = root == machine.home.join("_workspace")
            || root == machine.home.join("AndroidStudioProjects");
        for item in project_walker(&root) {
            let Ok(entry) = item else {
                report.incomplete = true;
                continue;
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || !ndk_config(entry.path()) {
                continue;
            }
            let Ok(content) = read_small(entry.path(), 2 * 1024 * 1024) else {
                report.incomplete = true;
                continue;
            };
            let project = if is_collection {
                let relative = entry.path().strip_prefix(&root).expect("walker root");
                relative
                    .components()
                    .next()
                    .map_or_else(|| root.clone(), |part| root.join(part))
            } else {
                root.clone()
            };
            let state = projects.entry(project).or_default();
            let tokens: HashSet<_> = content
                .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
                .collect();
            for version in &versions {
                if tokens.contains(version) {
                    state.1 = true;
                    report
                        .references
                        .entry((*version).to_owned())
                        .or_insert_with(|| entry.path().to_owned());
                }
            }
            state.0 |= [
                "ndkVersion",
                "externalNativeBuild",
                "ndk.dir",
                "ndkPath",
                "CMAKE_ANDROID_NDK",
                "ANDROID_NDK_HOME",
                "cargo ndk",
            ]
            .iter()
            .any(|marker| content.contains(marker));
        }
    }
    report.unresolved_native = projects
        .values()
        .any(|(native, resolved)| *native && !resolved);
    report
}

fn project_walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .same_file_system(true)
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !(matches!(
                name.as_ref(),
                "node_modules" | "target" | "Library" | "Pods" | "dist" | "DerivedData"
            ) || (name.starts_with('.')
                && !matches!(name.as_ref(), ".cxx" | ".cargo" | ".github")))
        })
        .build()
}

fn ndk_config(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    matches!(
        name,
        "local.properties"
            | "gradle.properties"
            | "CMakeLists.txt"
            | "CMakeCache.txt"
            | "CMakePresets.json"
            | "Android.mk"
            | "build_model.json"
            | "android_gradle_build.json"
            | "build.ninja"
            | "build_command.txt"
    ) || name.ends_with(".gradle")
        || name.ends_with(".gradle.kts")
        || name.ends_with(".versions.toml")
        || (matches!(name, "config.toml" | "config")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|p| p == ".cargo"))
}

fn read_small(path: &Path, limit: u64) -> Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > limit {
        bail!("Input is not a small regular file");
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        bail!("Input exceeded the read limit");
    }
    Ok(text)
}

fn volume(path: &Path) -> Result<(String, u64)> {
    let output = app_storage::command_output(
        Command::new("/bin/df").arg("-kP").arg(path),
        Duration::from_secs(8),
    )?;
    let row: Vec<_> = output
        .lines()
        .last()
        .context("No volume reported")?
        .split_whitespace()
        .collect();
    let disk = row
        .first()
        .context("Missing volume identifier")?
        .to_string();
    let free: u64 = row.get(3).context("Missing available blocks")?.parse()?;
    Ok((
        disk,
        free.checked_mul(1024).context("Volume size overflow")?,
    ))
}

fn missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == ErrorKind::NotFound)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct TestMachine {
        scans: RefCell<VecDeque<Report>>,
        removals: RefCell<Vec<Target>>,
        measured: RefCell<VecDeque<u64>>,
    }

    impl Backend for TestMachine {
        fn scan(&self) -> Report {
            self.scans.borrow_mut().pop_front().unwrap()
        }

        fn remove(&self, target: &Target) -> Result<Removal> {
            self.removals.borrow_mut().push(target.clone());
            Ok(Removal::Removed)
        }

        fn free(&self) -> Result<u64> {
            Ok(self.measured.borrow_mut().pop_front().unwrap())
        }
    }

    fn runtime(id: u128, selectable: bool) -> Item {
        Item {
            target: Target::Runtime {
                image: Uuid::from_u128(id),
                build: format!("23S{id}"),
            },
            label: format!("watchOS 26.{id}"),
            size: Some(123),
            basis: "Xcode estimate",
            reason: Message::new("Test item", "試験用"),
            selectable,
        }
    }

    fn report(items: Vec<Item>) -> Report {
        Report {
            items,
            notes: Vec::new(),
        }
    }

    #[test]
    fn removes_only_the_confirmed_selection_and_returns_measured_values() -> Result<()> {
        let a = runtime(1, true);
        let b = runtime(2, true);
        let protected = runtime(3, false);
        let visible = report(vec![a.clone(), b, protected.clone()]);
        assert!(Review::new(&visible, &BTreeSet::from([protected.target]), Vec::new()).is_err());
        let review = Review::new(&visible, &BTreeSet::from([a.target.clone()]), Vec::new())?;
        let machine = TestMachine {
            scans: RefCell::new(VecDeque::from([visible])),
            removals: RefCell::default(),
            measured: RefCell::new(VecDeque::from([4000, 4765])),
        };
        let receipt = apply_selection(review, &machine)?;
        assert_eq!(*machine.removals.borrow(), [a.target]);
        assert_eq!(receipt.results.len(), 1);
        assert_eq!(receipt.before, 4000);
        assert_eq!(receipt.after?, 4765);
        Ok(())
    }

    #[test]
    fn revalidation_refuses_a_runtime_that_became_protected_after_review() -> Result<()> {
        let good = runtime(1, true);
        let review = Review::new(
            &report(vec![good.clone()]),
            &BTreeSet::from([good.target]),
            Vec::new(),
        )?;
        let machine = TestMachine {
            scans: RefCell::new(VecDeque::from([report(vec![runtime(1, false)])])),
            removals: RefCell::default(),
            measured: RefCell::default(),
        };
        assert!(apply_selection(review, &machine).is_err());
        assert!(machine.removals.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn android_evidence_keeps_a_version_from_build_records_and_holds_ambiguous_projects(
    ) -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let home = fixture.path().canonicalize()?;
        let root = home.join("_workspace");
        let used = root.join("app/build/intermediates");
        let ambiguous = root.join("other");
        fs::create_dir_all(&used)?;
        fs::create_dir_all(&ambiguous)?;
        fs::write(
            used.join("build_model.json"),
            r#"{ "ndk": "/some/sdk/ndk/27.1.12297006" }"#,
        )?;
        fs::write(
            ambiguous.join("build.gradle"),
            "android { ndkVersion rootProject.ext.ndkVersion }",
        )?;
        let machine = Machine {
            sdk: home.join("Library/Android/sdk"),
            home,
            roots: vec![root],
        };
        let evidence = project_evidence(&machine, vec!["27.1.12297006", "28.2.13676358"]);
        assert_eq!(
            evidence.references.keys().collect::<Vec<_>>(),
            ["27.1.12297006"]
        );
        assert!(evidence.unresolved_native);
        assert!(!evidence.incomplete);
        Ok(())
    }

    #[test]
    fn a_test_destination_protects_an_otherwise_selectable_runtime() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let home = fixture.path().canonicalize()?;
        let root = home.join("_workspace");
        fs::create_dir(&root)?;
        fs::write(
            root.join("test.sh"),
            "xcodebuild -destination 'platform=iOS Simulator,OS=26.2'",
        )?;
        let machine = Machine {
            sdk: home.join("Library/Android/sdk"),
            home,
            roots: vec![root],
        };
        let mut items = vec![runtime(1, true), runtime(2, true)];
        let images = [("26.2", 1), ("26.20", 2)]
            .into_iter()
            .map(|(version, id)| {
                (
                    Uuid::from_u128(id).to_string().to_uppercase(),
                    app_storage::Runtime {
                        platform_identifier: "com.apple.platform.iphonesimulator".into(),
                        version: version.into(),
                        build: format!("23S{id}"),
                        size_bytes: Some(123),
                        deletable: Some(true),
                        runtime_identifier: format!("example-{id}"),
                        state: "Ready".into(),
                        path: None,
                    },
                )
            })
            .collect();
        protect_runtime_references(&machine, &mut items, &images);
        assert!(!items[0].selectable);
        assert!(items[0].reason.to_string().contains("test.sh"));
        assert!(items[1].selectable);
        Ok(())
    }
}
