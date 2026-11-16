use super::bytes;
use crate::cli::{DockerArgs, ListFormat};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, TimeDelta, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    source: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Cache {
    #[serde(rename = "ID")]
    id: String,
    last_used_at: Option<String>,
    reclaimable: bool,
    shared: bool,
    mutable: bool,
    size: String,
    #[serde(rename = "Type")]
    kind: String,
}

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

    match args.format {
        ListFormat::Json => println!(
            "{}",
            serde_json::json!({
                "context": args.context, "daemon_id": daemon.id, "candidates": candidates,
                "cache_error": cache_error,
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
        }
    }
    Ok(())
}

fn connect(context: &str) -> Result<Daemon> {
    if context.is_empty()
        || !context
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        bail!("Invalid Docker context name");
    }
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
    if !endpoint.starts_with("unix:///") && !endpoint.starts_with("npipe:////./pipe/") {
        bail!("Select a Docker context with a local Unix socket or Windows named pipe");
    }
    let mut base = command_on(&endpoint);
    let result = execute(base.args(["info", "--format", "{{json .ID}}"]), "info")?;
    let id: String = serde_json::from_slice(&result)?;
    if id.trim().is_empty() {
        bail!("Docker did not identify the daemon");
    }
    Ok(Daemon { endpoint, id })
}

pub fn protect_bind_mounts(context: &str, output: &Path) -> Result<()> {
    let daemon = connect(context)?;
    let records = inspect::<Option<Vec<Mount>>>(
        &daemon,
        "container",
        &["ls", "-aq", "--no-trunc"],
        "{{json .Mounts}}",
    )?;
    for mount in records.into_iter().flatten().flatten() {
        if mount.kind == "bind" {
            // Docker Desktop reports some host paths under /host_mnt.
            let source = mount
                .source
                .strip_prefix("/host_mnt")
                .ok()
                .map(|source| Path::new("/").join(source))
                .unwrap_or(mount.source);
            let source = source.canonicalize().unwrap_or(source);
            if source.starts_with(output) || output.starts_with(&source) {
                bail!("Output overlaps a container bind mount");
            }
        }
    }
    Ok(())
}

fn candidates(
    daemon: &Daemon,
    days: u16,
    buildx: Option<&tempfile::TempDir>,
) -> Result<(Vec<Candidate>, Option<String>)> {
    let cutoff = Utc::now() - TimeDelta::days(i64::from(days));
    let containers = inspect::<Container>(
        daemon,
        "container",
        &["ls", "-aq", "--no-trunc"],
        r#"{"Id":{{json .Id}},"Image":{{json .Image}},"Status":{{json .State.Status}},"Finished":{{json .State.FinishedAt}},"Disposable":{{json (index .Config.Labels "still.disposable")}},"Size":{{json .SizeRw}}}"#,
    )?;
    let images = inspect::<Image>(
        daemon,
        "image",
        &["ls", "-aq", "--no-trunc"],
        r#"{"Id":{{json .Id}},"Tags":{{json .RepoTags}},"Digests":{{json .RepoDigests}},"Size":{{json .Size}}}"#,
    )?;
    let mut items = engine_candidates(containers, images, cutoff);

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

fn inspect<T: DeserializeOwned>(
    daemon: &Daemon,
    kind: &str,
    list: &[&str],
    template: &str,
) -> Result<Vec<T>> {
    let ids = execute(command_on(&daemon.endpoint).arg(kind).args(list), "list")?;
    let mut ids: Vec<_> = std::str::from_utf8(&ids)?
        .lines()
        .filter(|s| !s.is_empty())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids
        .iter()
        .any(|id| !hex_id(id.strip_prefix("sha256:").unwrap_or(id)))
    {
        bail!("Docker returned an unexpected resource ID");
    }
    let mut records = Vec::new();
    for chunk in ids.chunks(64) {
        let mut cmd = command_on(&daemon.endpoint);
        cmd.args([kind, "inspect", "--format", template]);
        if kind == "container" {
            cmd.arg("--size");
        }
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
        let mut tags = image.tags.unwrap_or_default();
        let mut digests = image.digests.unwrap_or_default();
        if hex_id(image.id.strip_prefix("sha256:").unwrap_or(""))
            && !referenced.contains(image.id.as_str())
            && !digests.is_empty()
            && tags.len() <= 1
        {
            tags.sort();
            digests.sort();
            let evidence = tags.into_iter().chain(digests).collect();
            items.push(Candidate::new(
                Kind::Image,
                image.id,
                Some(image.size),
                "No container reference; registry digest recorded; size includes shared layers",
                evidence,
            ));
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

fn cache_candidates(
    daemon: &Daemon,
    buildx: &tempfile::TempDir,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Candidate>> {
    Ok(cache_records(daemon, buildx)?
        .into_iter()
        .filter_map(|cache| {
            let last_used = cache.last_used_at?;
            if cache.reclaimable
                && !cache.shared
                && !cache.mutable
                && cache.kind == "regular"
                && older(&last_used, cutoff)
                && cache_id(&cache.id)
            {
                Some(Candidate::new(
                    Kind::Cache,
                    cache.id,
                    cache.size.parse().ok(),
                    "Old, private, immutable BuildKit cache; Docker storage estimate",
                    vec![last_used],
                ))
            } else {
                None
            }
        })
        .collect())
}

fn cache_records(daemon: &Daemon, buildx: &tempfile::TempDir) -> Result<Vec<Cache>> {
    let raw = execute(
        buildx_command(daemon, buildx).args([
            "du",
            "--builder",
            "default",
            "--timeout",
            "5s",
            "--format",
            "json",
        ]),
        "Buildx du",
    )?;
    json_lines::<Cache>(&raw)
}

fn older(time: &str, cutoff: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(time).is_ok_and(|t| t.year_ce().1 > 1 && t < cutoff)
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
            execute(
                buildx_command(daemon, buildx).args([
                    "prune",
                    "--builder",
                    "default",
                    "--timeout",
                    "5s",
                    "--force",
                    "--filter",
                    &format!("id~=^{id}$"),
                    "--filter",
                    &format!("until={}h", u64::from(days) * 24),
                    "--filter",
                    "inuse=false",
                    "--filter",
                    "mutable=false",
                    "--filter",
                    "shared=false",
                    "--filter",
                    "type=regular",
                ]),
                "Buildx prune",
            )?;
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
    let mut command = Command::new("docker");
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
        bail!("Docker {operation} failed ({})", output.status);
    }
    Ok(output.stdout)
}

fn json_lines<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>> {
    std::str::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}
