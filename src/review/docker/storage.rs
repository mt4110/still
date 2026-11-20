use super::{command, command_on, validate_context, validate_endpoint};
use crate::review::app_storage::command_output;
use crate::review::message::Message;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
mod actions;
pub(crate) use actions::{Receipt, Review};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Container,
    Image,
    Volume,
    Cache,
}

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct Resource {
    #[serde(rename = "ID")]
    pub id: String,
    pub name: String,
    pub size: String,
    pub status: String,
    pub detail: String,
    pub shared_size: String,
    pub unique_size: String,
    pub mounts: String,
    pub last_used: String,
    pub operation: String,
    #[serde(skip)]
    pub created: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub started: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub finished: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub restart_count: u64,
    #[serde(skip)]
    pub eligible: bool,
    #[serde(skip, default = "unavailable")]
    pub reason: Message,
    #[serde(skip)]
    pub estimated_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Response {
    images: Option<Vec<Resource>>,
    containers: Option<Vec<Resource>>,
    volumes: Option<Vec<Resource>>,
    build_cache: Option<Vec<Resource>>,
}

pub(crate) struct Report {
    pub context: String,
    pub observed_at: chrono::DateTime<chrono::Local>,
    pub groups: Vec<(Kind, Vec<Resource>)>,
    daemon: Option<actions::Pin>,
    snapshots: std::collections::BTreeMap<String, actions::Metadata>,
}

pub(crate) struct CacheCleanup {
    pub context: String,
    pub days: u16,
    endpoint: String,
    pin: actions::Pin,
}

pub(crate) struct CacheCleanupResult {
    pub context: String,
    pub days: u16,
    pub result: Result<String>,
    pub before: Option<u64>,
    pub after: Option<u64>,
}

impl CacheCleanup {
    pub fn prepare(context: &str, days: u16) -> Result<Self> {
        anyhow::ensure!(
            (1..=3650).contains(&days),
            "Choose a retention period between 1 and 3650 days"
        );
        let daemon = super::connect(context)?;
        let pin = actions::capture(&daemon.endpoint)?;
        Ok(Self {
            context: context.to_owned(),
            days,
            endpoint: daemon.endpoint,
            pin,
        })
    }

    pub fn apply(self) -> CacheCleanupResult {
        let free = || {
            directories::BaseDirs::new()
                .and_then(|d| crate::review::filesystem::available(d.home_dir()))
        };
        let before = free();
        let result = checked_cache_cleanup(
            || {
                let current = super::connect(&self.context)?;
                anyhow::ensure!(
                    current.endpoint == self.endpoint
                        && actions::capture(&self.endpoint)? == self.pin,
                    "Docker context or daemon changed after confirmation"
                );
                Ok(())
            },
            || {
                let filter = format!("until={}h", u64::from(self.days) * 24);
                command_output(command_on(&self.endpoint).args(["builder", "prune", "--force", "--filter", &filter]), Duration::from_secs(300))
                    .context("Docker cache cleanup did not complete; some records may already have been removed")
            },
        );
        CacheCleanupResult {
            context: self.context,
            days: self.days,
            result,
            before,
            after: free(),
        }
    }
}

fn checked_cache_cleanup(
    verify: impl FnOnce() -> Result<()>,
    run: impl FnOnce() -> Result<String>,
) -> Result<String> {
    verify()?;
    run()
}

// Project only storage metadata. Container commands, environment variables,
// credentials, log contents and application data are outside this view.
const FORMAT: &str = r#"{
"Images":[{{range $i,$v := .Images}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json (printf "%s:%s" $v.Repository $v.Tag)}},"Size":{{json $v.Size}},"Status":{{json $v.Containers}},"SharedSize":{{json $v.SharedSize}},"UniqueSize":{{json $v.UniqueSize}}}{{end}}],
"Containers":[{{range $i,$v := .Containers}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json $v.Names}},"Size":{{json $v.Size}},"Status":{{json $v.State}},"Detail":{{json $v.Image}},"Mounts":{{json $v.Mounts}}}{{end}}],
"Volumes":[{{range $i,$v := .Volumes}}{{if $i}},{{end}}{"ID":{{json $v.Name}},"Name":{{json $v.Name}},"Size":{{json $v.Size}},"Status":{{json $v.Links}},"Detail":{{json $v.Driver}}}{{end}}],
"BuildCache":[{{range $i,$v := .BuildCache}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json $v.CacheType}},"LastUsed":{{json $v.LastUsedSince}},"Operation":{{ $op := index (split $v.Description " ") 0 }}{{if eq $op "pulled"}}"pull"{{else if eq $op "RUN"}}"run"{{else if eq $op "COPY"}}"copy"{{else if eq $op "ADD"}}"add"{{else}}""{{end}},"Size":{{json $v.Size}},"Status":{{json $v.InUse}},"Detail":{{json $v.Shared}}}{{end}}]
}"#;

// Only offer local connections. inspect and each destructive operation independently
// resolve and verify the selected endpoint again, since context files can change.
pub(crate) fn local_contexts() -> Result<Vec<String>> {
    let output = command_output(
        command().args([
            "context",
            "ls",
            "--format",
            r#"[{{json .Name}},{{json .DockerEndpoint}}]"#,
        ]),
        Duration::from_secs(10),
    )?;
    parse_contexts(&output)
}

fn parse_contexts(output: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let [name, endpoint]: [String; 2] = serde_json::from_str(line)?;
        if !name.starts_with('-')
            && validate_context(&name).is_ok()
            && validate_endpoint(&endpoint).is_ok()
        {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

pub(crate) fn inspect(context: &str) -> Result<Report> {
    let context = if context.trim().is_empty() {
        command_output(command().args(["context", "show"]), Duration::from_secs(10))?
            .trim()
            .to_owned()
    } else {
        context.trim().to_owned()
    };
    validate_context(&context)?;
    let endpoint = command_output(
        command().args([
            "context",
            "inspect",
            "--format",
            "{{json .Endpoints.docker.Host}}",
            &context,
        ]),
        Duration::from_secs(10),
    )?;
    let endpoint: String = serde_json::from_str(&endpoint)?;
    validate_endpoint(&endpoint)?;
    let pin = actions::capture(&endpoint)?;
    let response = command_output(
        command_on(&endpoint).args(["system", "df", "--verbose", "--format", FORMAT]),
        Duration::from_secs(30),
    )
    .context("Docker の容量を取得できませんでした。Docker Desktop または Colima と接続先を確認してください")?;
    let mut report = parse(context, &response)?;
    actions::enrich(&mut report, pin)?;
    Ok(report)
}

fn parse(context: String, json: &str) -> Result<Report> {
    let response: Response = serde_json::from_str(json)?;
    let groups = [
        (Kind::Container, response.containers),
        (Kind::Image, response.images),
        (Kind::Volume, response.volumes),
        (Kind::Cache, response.build_cache),
    ]
    .into_iter()
    .map(|(kind, items)| {
        let mut items = items.unwrap_or_default();
        items.sort_by(|a, b| {
            size_bytes(&b.size)
                .cmp(&size_bytes(&a.size))
                .then_with(|| a.name.cmp(&b.name))
        });
        for item in &mut items {
            item.reason = if kind == Kind::Container {
                unavailable()
            } else {
                Message::new(
                    "This category is view-only; images, volumes and cache are retained",
                    "この種類は表示専用です。イメージ・ボリューム・キャッシュは保持します",
                )
            };
        }
        (kind, items)
    })
    .collect();
    Ok(Report {
        context,
        observed_at: chrono::Local::now(),
        daemon: None,
        snapshots: Default::default(),
        groups,
    })
}

fn unavailable() -> Message {
    Message::new(
        "Complete container metadata could not be verified",
        "コンテナの完全な情報を確認できないため削除できません",
    )
}

impl Default for Resource {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            size: String::new(),
            status: String::new(),
            detail: String::new(),
            shared_size: String::new(),
            unique_size: String::new(),
            mounts: String::new(),
            last_used: String::new(),
            operation: String::new(),
            created: None,
            started: None,
            finished: None,
            restart_count: 0,
            eligible: false,
            reason: unavailable(),
            estimated_bytes: None,
        }
    }
}

pub(crate) fn size_bytes(size: &str) -> Option<u64> {
    let number_end = size.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let number: f64 = size[..number_end].parse().ok()?;
    let scale = match size[number_end..].trim() {
        "B" => 1.0,
        "kB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => return None,
    };
    let bytes = number * scale;
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_picker_omits_remote_endpoints_and_option_like_names() -> Result<()> {
        let output = r#"["default","unix:///var/run/docker.sock"]
["production","ssh://docker@server"]
["--help","unix:///tmp/docker.sock"]
["colima","unix:///tmp/colima.sock"]"#;
        assert_eq!(parse_contexts(output)?, ["colima", "default"]);
        assert!(parse_contexts("broken").is_err());
        Ok(())
    }

    #[test]
    fn cache_cleanup_never_runs_if_connection_verification_fails() {
        let called = std::cell::Cell::new(false);
        let result = checked_cache_cleanup(
            || anyhow::bail!("daemon changed"),
            || {
                called.set(true);
                Ok(String::new())
            },
        );
        assert!(result.is_err());
        assert!(!called.get());
    }

    #[test]
    fn storage_inventory_preserves_referenced_data_and_orders_display_sizes() -> Result<()> {
        let report = parse(
            "local".into(),
            r#"{"Images":null,"Containers":[],"BuildCache":[],"Volumes":[
                {"ID":"small","Name":"small","Size":"900MB","Status":"0"},
                {"ID":"data","Name":"data","Size":"2GB","Status":"3"},
                {"ID":"unknown","Name":"unknown","Size":"N/A","Status":"0"}
            ]}"#,
        )?;
        let volumes = &report.groups[2].1;
        assert_eq!(volumes[0].name, "data");
        assert_eq!(volumes[0].status, "3");
        assert_eq!(volumes[2].name, "unknown");
        assert_eq!(size_bytes("N/A"), None);
        Ok(())
    }
}
