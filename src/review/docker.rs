use super::{bytes, message::Message};
use crate::cli::{DockerArgs, ListFormat};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, TimeDelta, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) mod storage;

#[derive(Debug, Eq, PartialEq)]
struct Daemon {
    endpoint: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    #[serde(rename = "Id")]
    id: String,
    image: String,
    status: String,
    finished: String,
    disposable: Option<String>,
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Image {
    #[serde(rename = "Id")]
    id: String,
    tags: Option<Vec<String>>,
    digests: Option<Vec<String>>,
    size: u64,
    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    created: Option<String>,
    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    last_tagged: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    source: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerSummary {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "ImageID")]
    image_id: String,
    state: String,
    labels: Option<ContainerLabels>,
    mounts: Option<Vec<Mount>>,
}

#[derive(Deserialize)]
struct ContainerLabels {
    #[serde(rename = "still.disposable")]
    disposable: Option<String>,
}

const CONTAINER_TEMPLATE: &str = r#"{"Id":{{json .Id}},"Image":{{json .Image}},"Status":{{json .State.Status}},"Finished":{{json .State.FinishedAt}},"Disposable":{{json (index .Config.Labels "still.disposable")}}}"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Cache {
    #[serde(rename = "ID")]
    id: String,
    reclaimable: bool,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    parents: Option<Vec<String>>,
    shared: bool,
    mutable: bool,
    size: String,
    #[serde(rename = "Type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EngineCache {
    #[serde(rename = "ID")]
    id: String,
    last_used_at: String,
    in_use: String,
    shared: String,
    cache_type: String,
}

const ENGINE_CACHE_TEMPLATE: &str = r#"{{range .BuildCache}}{"ID":{{json .ID}},"LastUsedAt":{{json .LastUsedAt}},"InUse":{{json .InUse}},"Shared":{{json .Shared}},"CacheType":{{json .CacheType}}}{{println}}{{end}}"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Image,
    Container,
    Cache,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Candidate {
    key: String,
    id: String,
    kind: Kind,
    bytes: Option<u64>,
    reason: &'static str,
    evidence: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ReviewCandidate {
    pub key: String,
    pub reason: Message,
    pub estimated_bytes: Option<u64>,
    pub estimate_note: &'static str,
    pub eligible: bool,
}

impl From<Candidate> for ReviewCandidate {
    fn from(candidate: Candidate) -> Self {
        let (estimated_bytes, estimate_note) = match candidate.kind {
            Kind::Image => (None, "Shared image layers keep reclaim unknown"),
            Kind::Container => (candidate.bytes, "Docker writable-layer estimate"),
            Kind::Cache => (candidate.bytes, "Docker BuildKit storage estimate"),
        };
        Self {
            key: candidate.key,
            reason: Message::new(
                candidate.reason,
                match candidate.kind {
                    Kind::Image => "コンテナから参照されていない、取得元を確認できるイメージです。",
                    Kind::Container => "停止済みで、使い捨てと明示されたコンテナです。",
                    Kind::Cache => "古く、共有されていない BuildKit キャッシュです。",
                },
            ),
            estimated_bytes,
            estimate_note,
            eligible: true,
        }
    }
}

pub(super) struct ReviewFindings {
    pub items: Vec<ReviewCandidate>,
    pub cache_error: Option<String>,
    pub volume_error: Option<String>,
}

pub(super) fn review_candidates(context: &str, include_cache: bool) -> Result<ReviewFindings> {
    let daemon = connect(context)
        .with_context(|| format!("Could not inspect Docker context {context:?}"))?;
    let (buildx, setup_error) = match include_cache.then(tempfile::tempdir).transpose() {
        Ok(state) => (state, None),
        Err(err) => (
            None,
            Some(format!("Could not initialize Buildx inspection: {err}")),
        ),
    };
    let (items, cache_error) = candidates(&daemon, 7, buildx.as_ref())?;
    let mut items: Vec<_> = items.into_iter().map(ReviewCandidate::from).collect();
    let volume_error = match protected_volumes(&daemon) {
        Ok(volumes) => {
            items.extend(volumes);
            None
        }
        Err(err) => Some(err.to_string()),
    };
    Ok(ReviewFindings {
        items,
        cache_error: cache_error.or(setup_error),
        volume_error,
    })
}

impl Candidate {
    fn new(
        kind: Kind,
        id: String,
        size: Option<u64>,
        reason: &'static str,
        evidence: Vec<String>,
    ) -> Self {
        let prefix = match kind {
            Kind::Image => "image",
            Kind::Container => "container",
            Kind::Cache => "cache",
        };
        Self {
            key: format!("{prefix}:{id}"),
            kind,
            id,
            bytes: size,
            reason,
            evidence,
        }
    }
}

pub fn run(args: &DockerArgs) -> Result<()> {
    let daemon = connect(&args.context)?;
    // A fresh Buildx state directory prevents a configured remote/shared builder from
    // overriding the built-in builder of the explicitly selected daemon.
    let buildx = args.include_cache.then(tempfile::tempdir).transpose()?;
    let (candidates, cache_error) = candidates(&daemon, args.older_than_days, buildx.as_ref())?;
    if let Some(key) = &args.remove {
        let selected = candidates
            .iter()
            .find(|candidate| &candidate.key == key)
            .context("The exact Docker resource is not currently eligible")?;
        if !io::stdin().is_terminal() {
            bail!("Docker removal requires an interactive terminal");
        }
        print!(
            "Remove {} on {} ({})? [y/N] ",
            selected.key, args.context, selected.reason
        );
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        if response.trim() == "y" {
            let current = connect(&args.context)?;
            if daemon != current {
                bail!("Docker daemon changed after review");
            }
            if !self::candidates(&current, args.older_than_days, buildx.as_ref())?
                .0
                .contains(selected)
            {
                bail!("Docker resource, usage, or references changed after review");
            }
            remove(&current, selected, args.older_than_days, buildx.as_ref())?;
            println!("Removed {}", selected.key);
        }
        return Ok(());
    }

    let (volumes, volume_error) = match protected_volumes(&daemon) {
        Ok(volumes) => (volumes, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    };
    match args.format {
        ListFormat::Json => println!(
            "{}",
            serde_json::json!({
                "context": args.context, "daemon_id": daemon.id, "candidates": candidates,
                "cache_error": cache_error,
                "protected_volumes": volumes, "volume_error": volume_error,
            })
        ),
        ListFormat::Text => {
            println!("Docker context {} (sizes inside Docker)", args.context);
            for candidate in candidates {
                let size = candidate.bytes.map(bytes).unwrap_or_else(|| "·".into());
                println!("{size:>12}  {}  ({})", candidate.key, candidate.reason);
            }
            if let Some(error) = cache_error {
                eprintln!("BuildKit not assessed: {error}");
            }
            for volume in volumes {
                println!("Protected  {}  ({})", volume.key, volume.reason);
            }
            if let Some(error) = volume_error {
                eprintln!("Volumes not assessed: {error}");
            }
        }
    }
    Ok(())
}

fn protected_volumes(daemon: &Daemon) -> Result<Vec<ReviewCandidate>> {
    let raw = execute(
        command_on(&daemon.endpoint).args([
            "volume",
            "ls",
            "--filter",
            "dangling=true",
            "--format",
            "{{json .Name}}",
        ]),
        "list unreferenced volumes",
    )?;
    std::str::from_utf8(&raw)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let name: String = serde_json::from_str(line)?;
            Ok(ReviewCandidate {
                key: format!("volume:{name}"),
                reason: Message::new(
                    "Unreferenced volume; may contain persistent data",
                    "未接続のボリュームです。データが残っている可能性があるため保護しています。",
                ),
                estimated_bytes: None,
                estimate_note: "Host reclaim estimate unavailable",
                eligible: false,
            })
        })
        .collect()
}

fn connect(context: &str) -> Result<Daemon> {
    validate_context(context)?;
    let mut command = command();
    let result = execute(
        command.args([
            "context",
            "inspect",
            "--format",
            "{{json .Endpoints.docker.Host}}",
            context,
        ]),
        "context inspect",
    )?;
    let endpoint: String = serde_json::from_slice(&result)?;
    validate_endpoint(&endpoint)?;
    let mut base = command_on(&endpoint);
    let result = execute(base.args(["info", "--format", "{{json .ID}}"]), "info")?;
    let id: String = serde_json::from_slice(&result)?;
    if id.trim().is_empty() {
        bail!("Docker did not identify the daemon");
    }
    Ok(Daemon { endpoint, id })
}

fn validate_context(context: &str) -> Result<()> {
    if context.is_empty()
        || !context
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        bail!("Invalid Docker context name");
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    if !endpoint.starts_with("unix:///") && !endpoint.starts_with("npipe:////./pipe/") {
        bail!("Select a Docker context with a local Unix socket or Windows named pipe");
    }
    Ok(())
}

pub fn protect_bind_mounts(context: &str, output: &Path) -> Result<()> {
    check_bind_mounts(output, &bind_mount_paths(context)?)
}

pub(super) fn bind_mount_paths(context: &str) -> Result<Vec<PathBuf>> {
    let daemon = connect(context)?;
    let records = inspect::<Option<Vec<Mount>>>(
        &daemon,
        "container",
        &["ls", "-aq", "--no-trunc"],
        "{{json .Mounts}}",
    )
    .or_else(|inspect_error| {
        container_summary(&daemon)
            .map(|summary| summary.into_iter().map(|entry| entry.mounts).collect())
            .with_context(|| format!("{inspect_error}; Engine container summary unavailable"))
    })?;
    let mut sources = Vec::new();
    for mount in records.into_iter().flatten().flatten() {
        if mount.kind == "bind" {
            // Docker Desktop reports some host paths under /host_mnt.
            let source = mount
                .source
                .strip_prefix("/host_mnt")
                .ok()
                .map(|source| Path::new("/").join(source))
                .unwrap_or(mount.source);
            sources.push(source.canonicalize().unwrap_or(source));
        }
    }
    Ok(sources)
}

pub(super) fn check_bind_mounts(output: &Path, sources: &[PathBuf]) -> Result<()> {
    if sources
        .iter()
        .any(|source| source.starts_with(output) || output.starts_with(source))
    {
        return Err(super::message::Message::new(
            "Output overlaps a container bind mount",
            "Docker コンテナにマウントされているフォルダと重なるため保護しています。",
        )
        .into());
    }
    Ok(())
}

fn candidates(
    daemon: &Daemon,
    days: u16,
    buildx: Option<&tempfile::TempDir>,
) -> Result<(Vec<Candidate>, Option<String>)> {
    let cutoff = Utc::now() - TimeDelta::days(i64::from(days));
    let containers = container_inventory(daemon)?;
    let images = inspect::<Image>(
        daemon,
        "image",
        &["ls", "-aq", "--no-trunc"],
        r#"{"Id":{{json .Id}},"Tags":{{json .RepoTags}},"Digests":{{json .RepoDigests}},"Size":{{json .Size}}}"#,
    )?;
    let mut items = engine_candidates(containers, images, cutoff);
    for container in items
        .iter_mut()
        .filter(|item| item.kind == Kind::Container && item.bytes.is_none())
    {
        // Measuring writable layers can be expensive; only measure eligible stopped containers.
        container.bytes = execute(
            command_on(&daemon.endpoint).args([
                "container",
                "inspect",
                "--size",
                "--format",
                "{{json .SizeRw}}",
                &container.id,
            ]),
            "container size",
        )
        .ok()
        .and_then(|output| serde_json::from_slice::<Option<u64>>(&output).ok())
        .flatten();
    }

    let cache_error = if let Some(buildx) = buildx {
        match cache_candidates(daemon, buildx, cutoff) {
            Ok(mut cache) => {
                items.append(&mut cache);
                None
            }
            Err(err) => Some(err.to_string()),
        }
    } else {
        None
    };
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
    Ok((items, cache_error))
}

fn container_inventory(daemon: &Daemon) -> Result<Vec<Container>> {
    inspect::<Container>(
        daemon,
        "container",
        &["ls", "-aq", "--no-trunc"],
        CONTAINER_TEMPLATE,
    )
    .or_else(|inspect_error| {
        containers_from_summary(daemon)
            .with_context(|| format!("{inspect_error}; Engine container summary unavailable"))
    })
}

fn container_summary(daemon: &Daemon) -> Result<Vec<ContainerSummary>> {
    let socket = daemon
        .endpoint
        .strip_prefix("unix://")
        .context("Engine summary requires a local Unix socket")?;
    let raw = execute(
        Command::new("curl")
            .args([
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--unix-socket",
                socket,
                "--max-time",
                "10",
                "http://localhost/containers/json?all=1",
            ])
            .stdin(Stdio::null()),
        "container summary",
    )?;
    Ok(serde_json::from_slice(&raw)?)
}

fn containers_from_summary(daemon: &Daemon) -> Result<Vec<Container>> {
    container_summary(daemon)?
        .into_iter()
        .map(|entry| {
            if !hex_id(&entry.id)
                || !hex_id(
                    entry
                        .image_id
                        .strip_prefix("sha256:")
                        .unwrap_or(&entry.image_id),
                )
            {
                bail!("Engine summary did not identify a container or its image");
            }
            let image = if entry.image_id.starts_with("sha256:") {
                entry.image_id
            } else {
                format!("sha256:{}", entry.image_id)
            };
            let labelled =
                entry.labels.and_then(|labels| labels.disposable).as_deref() == Some("true");
            if entry.state == "exited" && labelled {
                if let Ok(mut results) = inspect_ids::<Container>(
                    daemon,
                    "container",
                    CONTAINER_TEMPLATE,
                    std::slice::from_ref(&entry.id),
                ) {
                    if let Some(mut inspected) = results.pop() {
                        inspected.image = image;
                        return Ok(inspected);
                    }
                }
            }
            // Summary records remain image references even when the daemon cannot inspect them.
            Ok(Container {
                id: entry.id,
                image,
                status: entry.state,
                finished: String::new(),
                disposable: None,
                size: None,
            })
        })
        .collect()
}

fn inspect<T: DeserializeOwned>(
    daemon: &Daemon,
    kind: &str,
    list: &[&str],
    template: &str,
) -> Result<Vec<T>> {
    let mut ids = resource_ids(daemon, kind, list)?;
    for _ in 0..3 {
        let observed = inspect_ids(daemon, kind, template, &ids);
        let latest = resource_ids(daemon, kind, list)?;
        if latest == ids {
            return observed;
        }
        // An Engine may return partial output if a container disappears mid-inspection.
        ids = latest;
    }
    bail!("Docker {kind} inventory kept changing during inspection; refresh to try again")
}

fn resource_ids(daemon: &Daemon, kind: &str, list: &[&str]) -> Result<Vec<String>> {
    let ids = execute(command_on(&daemon.endpoint).arg(kind).args(list), "list")?;
    let mut ids: Vec<_> = std::str::from_utf8(&ids)?
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids
        .iter()
        .any(|id| !hex_id(id.strip_prefix("sha256:").unwrap_or(id.as_str())))
    {
        bail!("Docker returned an unexpected resource ID");
    }
    Ok(ids)
}

fn inspect_ids<T: DeserializeOwned>(
    daemon: &Daemon,
    kind: &str,
    template: &str,
    ids: &[String],
) -> Result<Vec<T>> {
    let mut records = Vec::new();
    for chunk in ids.chunks(64) {
        let mut cmd = command_on(&daemon.endpoint);
        cmd.args([kind, "inspect", "--format", template]);
        let inspected = json_lines::<T>(&execute(cmd.args(chunk), "inspect")?)?;
        if inspected.len() != chunk.len() {
            bail!("Docker returned an incomplete resource inventory");
        }
        records.extend(inspected);
    }
    Ok(records)
}

fn engine_candidates(
    containers: Vec<Container>,
    images: Vec<Image>,
    cutoff: DateTime<Utc>,
) -> Vec<Candidate> {
    let referenced: HashSet<_> = containers
        .iter()
        .map(|container| container.image.as_str())
        .collect();
    let mut items = Vec::new();
    for image in images {
        if let Some(candidate) = image_candidate(image, &referenced) {
            items.push(candidate);
        }
    }
    for container in &containers {
        if hex_id(&container.id)
            && container.status == "exited"
            && container.disposable.as_deref() == Some("true")
            && older(&container.finished, cutoff)
        {
            items.push(Candidate::new(
                Kind::Container,
                container.id.clone(),
                container.size,
                "Stopped, labelled still.disposable=true; writable layer only",
                vec![container.image.clone(), container.finished.clone()],
            ));
        }
    }
    items
}

fn image_candidate(image: Image, referenced: &HashSet<&str>) -> Option<Candidate> {
    let mut tags = image.tags.unwrap_or_default();
    let mut digests = image.digests.unwrap_or_default();
    if !hex_id(image.id.strip_prefix("sha256:").unwrap_or(""))
        || referenced.contains(image.id.as_str())
        || digests.is_empty()
        || tags.len() > 1
    {
        return None;
    }
    tags.sort();
    digests.sort();
    Some(Candidate::new(
        Kind::Image,
        image.id,
        Some(image.size),
        "No container reference; registry digest recorded; size includes shared layers",
        tags.into_iter().chain(digests).collect(),
    ))
}

#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
type InspectedImage = (Candidate, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
pub(super) struct ImageRemovalReview {
    pub context: String,
    daemon: Daemon,
    item: Candidate,
    pub created: Option<DateTime<Utc>>,
    pub last_tagged: Option<DateTime<Utc>>,
}

#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
impl ImageRemovalReview {
    pub fn prepare(context: &str, id: &str) -> Result<Self> {
        let daemon = connect(context)?;
        let (item, created, last_tagged) = Self::inspect(&daemon, id)?;
        Ok(Self {
            context: context.into(),
            daemon,
            item,
            created,
            last_tagged,
        })
    }

    fn inspect(daemon: &Daemon, id: &str) -> Result<InspectedImage> {
        anyhow::ensure!(
            hex_id(id.strip_prefix("sha256:").unwrap_or("")),
            "The complete image ID is required"
        );
        let containers = container_inventory(daemon)?;
        let referenced = containers
            .iter()
            .map(|container| container.image.as_str())
            .collect();
        let images = inspect_ids::<Image>(
            daemon,
            "image",
            r#"{"Id":{{json .Id}},"Tags":{{json .RepoTags}},"Digests":{{json .RepoDigests}},"Size":{{json .Size}},"Created":{{json .Created}},"LastTagged":{{with .Metadata}}{{json .LastTagTime}}{{else}}null{{end}}}"#,
            &[id.to_owned()],
        )?;
        let image = images
            .into_iter()
            .find(|image| image.id == id)
            .context("Docker did not return the exact image ID")?;
        let created = image.created.as_deref().and_then(cache_timestamp);
        let last_tagged = image.last_tagged.as_deref().and_then(cache_timestamp);
        let candidate = image_candidate(image, &referenced).context(Message::new(
            "The image is referenced, has multiple tags, or has no registry digest",
            "コンテナから参照されているか、複数のタグがあるか、取得元のダイジェストを確認できません。",
        ))?;
        Ok((candidate, created, last_tagged))
    }

    pub fn id(&self) -> &str {
        &self.item.id
    }

    pub fn evidence(&self) -> &[String] {
        &self.item.evidence
    }

    pub fn apply(self) -> Result<()> {
        let current = connect(&self.context)?;
        anyhow::ensure!(
            current == self.daemon,
            "Docker context or daemon changed after review"
        );
        anyhow::ensure!(
            Self::inspect(&current, self.id())?.0 == self.item,
            "Image metadata or references changed. Review it again"
        );
        anyhow::ensure!(
            connect(&self.context)? == current,
            "Docker daemon changed during recheck"
        );
        remove(&current, &self.item, 7, None)?;
        anyhow::ensure!(
            connect(&self.context)? == current,
            "Docker daemon changed after removal"
        );
        anyhow::ensure!(
            !resource_ids(&current, "image", &["ls", "-aq", "--no-trunc"])?
                .iter()
                .any(|id| id.trim_start_matches("sha256:")
                    == self.id().trim_start_matches("sha256:")),
            "The image is still listed after removal"
        );
        Ok(())
    }
}

const CACHE_SAFETY_FILTERS: [&str; 3] = ["type=regular", "private=\"\"", "immutable=\"\""];

fn cache_parent_ids<'a>(records: impl Iterator<Item = &'a Cache>) -> Result<HashSet<&'a str>> {
    // A record can look reclaimable in du while a child still references it.
    let mut parents = HashSet::new();
    for record in records {
        for parent in record.parents.iter().flatten() {
            anyhow::ensure!(cache_id(parent), "Buildx returned an invalid parent ID");
            parents.insert(parent.as_str());
        }
    }
    Ok(parents)
}

fn cache_candidates(
    daemon: &Daemon,
    buildx: &tempfile::TempDir,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Candidate>> {
    let engine = engine_cache_records(daemon)?;
    let records = cache_records(daemon, buildx)?;
    let parents = cache_parent_ids(records.iter())?;
    Ok(records
        .iter()
        .filter_map(|cache| {
            let engine = engine.get(&cache.id)?;
            let last_used = &engine.last_used_at;
            if cache.reclaimable
                && !parents.contains(cache.id.as_str())
                && !cache.shared
                && !cache.mutable
                && cache.kind == "regular"
                && engine.in_use == "false"
                && engine.shared == "false"
                && engine.cache_type == "regular"
                && older(last_used, cutoff)
                && cache_id(&cache.id)
            {
                Some(Candidate::new(
                    Kind::Cache,
                    cache.id.clone(),
                    cache_size(&cache.size),
                    "Old, private, immutable BuildKit cache; Docker storage estimate",
                    vec![last_used.clone()],
                ))
            } else {
                None
            }
        })
        .collect())
}

fn engine_cache_records(daemon: &Daemon) -> Result<HashMap<String, EngineCache>> {
    let raw = execute(
        command_on(&daemon.endpoint).args([
            "system",
            "df",
            "--verbose",
            "--format",
            ENGINE_CACHE_TEMPLATE,
        ]),
        "cache metadata",
    )?;
    let mut result = HashMap::new();
    for cache in json_lines::<EngineCache>(&raw)? {
        if !cache_id(&cache.id) || result.insert(cache.id.clone(), cache).is_some() {
            bail!("Docker returned an invalid or duplicate cache ID");
        }
    }
    Ok(result)
}

fn cache_records(daemon: &Daemon, buildx: &tempfile::TempDir) -> Result<Vec<Cache>> {
    let raw = execute(
        buildx_command(daemon, buildx).args(["du", "--builder", "default", "--format", "json"]),
        "Buildx du",
    )?;
    json_lines::<Cache>(&raw)
}

fn cache_timestamp(time: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(time)
        .or_else(|_| DateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S%.f %z %Z"))
        .ok()
        .filter(|time| time.year_ce().1 > 1)
        .map(|time| time.to_utc())
}

fn older(time: &str, cutoff: DateTime<Utc>) -> bool {
    cache_timestamp(time).is_some_and(|time| time < cutoff)
}

fn cache_size(value: &str) -> Option<u64> {
    if let Ok(bytes) = value.parse() {
        return Some(bytes);
    }
    let split = value.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let number: f64 = value[..split].parse().ok()?;
    let scale = match value[split..].trim() {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => return None,
    };
    let bytes = number * scale;
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

fn hex_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cache_id(id: &str) -> bool {
    (10..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn remove(
    daemon: &Daemon,
    candidate: &Candidate,
    days: u16,
    buildx: Option<&tempfile::TempDir>,
) -> Result<()> {
    let id = &candidate.id;
    match candidate.kind {
        Kind::Container => {
            execute(
                command_on(&daemon.endpoint).args(["container", "rm", id]),
                "container rm",
            )?;
        }
        Kind::Image => {
            execute(
                command_on(&daemon.endpoint).args(["image", "rm", "--no-prune", id]),
                "image rm",
            )?;
        }
        Kind::Cache => {
            let buildx = buildx.context("Buildx must be explicitly enabled")?;
            let id_filter = format!("id~=^{id}$");
            let age_filter = format!("until={}h", u64::from(days) * 24);
            // Verify the exact selector before pruning. BuildKit evaluates age during prune,
            // while eligibility was obtained from Engine's absolute last-use timestamp.
            let mut check = buildx_command(daemon, buildx);
            check.args([
                "du",
                "--builder",
                "default",
                "--format",
                "json",
                "--filter",
                &id_filter,
            ]);
            for filter in CACHE_SAFETY_FILTERS {
                check.args(["--filter", filter]);
            }
            let matching = json_lines::<Cache>(&execute(&mut check, "Buildx filter check")?)?;
            if matching.len() != 1
                || matching[0].id != *id
                || !matching[0].reclaimable
                || matching[0].shared
                || matching[0].mutable
                || matching[0].kind != "regular"
            {
                bail!("Buildx could not confirm the selected cache ID and safety filters");
            }
            let mut prune = buildx_command(daemon, buildx);
            prune.args([
                "prune",
                "--builder",
                "default",
                "--force",
                "--filter",
                &id_filter,
                "--filter",
                &age_filter,
            ]);
            for filter in CACHE_SAFETY_FILTERS {
                prune.args(["--filter", filter]);
            }
            execute(&mut prune, "Buildx prune")?;
            if cache_records(daemon, buildx)?
                .iter()
                .any(|record| record.id == *id)
            {
                bail!("The cache record is still present after pruning");
            }
        }
    }
    Ok(())
}

fn command() -> Command {
    let mut command = super::external_command("docker");
    command
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_HOST")
        .env_remove("BUILDX_BUILDER")
        .stdin(Stdio::null());
    command
}

fn command_on(endpoint: &str) -> Command {
    let mut command = command();
    command.args(["--host", endpoint]);
    command
}

fn buildx_command(daemon: &Daemon, state: &tempfile::TempDir) -> Command {
    let mut command = command_on(&daemon.endpoint);
    command.env("BUILDX_CONFIG", state.path()).arg("buildx");
    command
}

fn execute(command: &mut Command, operation: &str) -> Result<Vec<u8>> {
    let output = command.output().context("Could not run the Docker CLI")?;
    if !output.status.success() {
        let detail = docker_error_detail(&output.stderr);
        if detail.is_empty() {
            bail!("Docker {operation} failed ({})", output.status);
        }
        bail!("Docker {operation} failed ({}): {detail}", output.status);
    }
    Ok(output.stdout)
}

fn docker_error_detail(stderr: &[u8]) -> String {
    const LIMIT: usize = 1024;
    let text = String::from_utf8_lossy(stderr);
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let mut detail: String = trimmed.chars().take(LIMIT).collect();
    if trimmed.chars().count() > LIMIT {
        detail.push('…');
    }
    detail
}

fn json_lines<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>> {
    std::str::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{cache_size, docker_error_detail, older, Candidate, Kind, ReviewCandidate};

    #[test]
    fn cache_metadata_accepts_absolute_times_and_approximate_sizes() {
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-11-19T00:00:00Z")
            .unwrap()
            .to_utc();
        assert!(older("2026-02-12 10:59:42.845633065 +0000 UTC", cutoff));
        assert!(!older("9 months ago", cutoff));
        assert!(!older("2026-11-20T00:00:00Z", cutoff));
        assert_eq!(cache_size("136MB"), Some(136_000_000));
        assert_eq!(cache_size("829889526"), Some(829_889_526));
        assert_eq!(cache_size("unknown"), None);
    }

    #[test]
    fn docker_error_detail_is_readable_and_bounded() {
        assert_eq!(
            docker_error_detail(b"  daemon unavailable\n"),
            "daemon unavailable"
        );
        assert_eq!(docker_error_detail(b"\n"), "");
        let detail = docker_error_detail("あ".repeat(1025).as_bytes());
        assert_eq!(detail.chars().count(), 1025);
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn review_never_treats_shared_image_size_as_reclaimable() {
        let candidate = |kind| {
            ReviewCandidate::from(Candidate::new(
                kind,
                "id".into(),
                Some(512),
                "reason",
                vec![],
            ))
        };
        assert_eq!(candidate(Kind::Image).estimated_bytes, None);
        assert_eq!(candidate(Kind::Container).estimated_bytes, Some(512));
        assert_eq!(candidate(Kind::Cache).estimated_bytes, Some(512));
    }
}
