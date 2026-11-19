use super::{command, command_on, validate_context, validate_endpoint};
use crate::review::app_storage::command_output;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Container,
    Image,
    Volume,
    Cache,
}

#[derive(Default, Deserialize)]
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
}

// Project only storage metadata. Container commands, environment variables,
// credentials, log contents and application data are outside this view.
const FORMAT: &str = r#"{
"Images":[{{range $i,$v := .Images}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json (printf "%s:%s" $v.Repository $v.Tag)}},"Size":{{json $v.Size}},"Status":{{json $v.Containers}},"SharedSize":{{json $v.SharedSize}},"UniqueSize":{{json $v.UniqueSize}}}{{end}}],
"Containers":[{{range $i,$v := .Containers}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json $v.Names}},"Size":{{json $v.Size}},"Status":{{json $v.State}},"Detail":{{json $v.Image}},"Mounts":{{json $v.Mounts}}}{{end}}],
"Volumes":[{{range $i,$v := .Volumes}}{{if $i}},{{end}}{"ID":{{json $v.Name}},"Name":{{json $v.Name}},"Size":{{json $v.Size}},"Status":{{json $v.Links}},"Detail":{{json $v.Driver}}}{{end}}],
"BuildCache":[{{range $i,$v := .BuildCache}}{{if $i}},{{end}}{"ID":{{json $v.ID}},"Name":{{json $v.CacheType}},"Size":{{json $v.Size}},"Status":{{json $v.InUse}},"Detail":{{json $v.Shared}}}{{end}}]
}"#;

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
    let response = command_output(
        command_on(&endpoint).args(["system", "df", "--verbose", "--format", FORMAT]),
        Duration::from_secs(30),
    )
    .context("Docker の容量を取得できませんでした。Docker Desktop または Colima と接続先を確認してください")?;
    parse(context, &response)
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
        (kind, items)
    })
    .collect();
    Ok(Report {
        context,
        observed_at: chrono::Local::now(),
        groups,
    })
}

fn size_bytes(size: &str) -> Option<u64> {
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
