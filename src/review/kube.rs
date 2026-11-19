use super::{external_command, message::Message};
use crate::cli::{KubeArgs, ListFormat};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Stdio;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(super) struct Source {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub namespace: String,
}

impl Source {
    pub fn new(kubeconfig: PathBuf, context: String, namespace: String) -> Result<Self> {
        let source = Self {
            kubeconfig: kubeconfig
                .canonicalize()
                .context("Cannot open kubeconfig")?,
            context,
            namespace,
        };
        source.validate()?;
        Ok(source)
    }

    fn validate(&self) -> Result<()> {
        if !self.kubeconfig.is_absolute() || self.context.trim().is_empty() {
            bail!("Select a kubeconfig and an explicit Kubernetes context");
        }
        let ns = self.namespace.as_bytes();
        if ns.is_empty()
            || ns.len() > 63
            || !ns[0].is_ascii_lowercase() && !ns[0].is_ascii_digit()
            || !ns[ns.len() - 1].is_ascii_alphanumeric()
            || !ns
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            || self.namespace.starts_with("kube-")
            || matches!(self.namespace.as_str(), "kube" | "local-path-storage")
        {
            bail!("Select one development namespace outside Kubernetes system namespaces");
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(super) struct Candidate {
    pub label: String,
    pub reason: Message,
    pub eligible: bool,
}

pub fn run(args: &KubeArgs) -> Result<()> {
    let source = Source::new(
        args.kubeconfig.clone(),
        args.context.clone(),
        args.namespace.clone(),
    )?;
    let found = candidates(&source)?;
    match args.format {
        ListFormat::Json => println!(
            "{}",
            serde_json::json!({"context": source.context, "namespace": source.namespace, "candidates": found})
        ),
        ListFormat::Text => {
            println!(
                "{} / {} (review only; cluster disk reclaim unknown)",
                source.context, source.namespace
            );
            for item in found {
                println!(
                    "{}  {}  ({})",
                    if item.eligible { "Review" } else { "Protected" },
                    item.label,
                    item.reason
                );
            }
        }
    }
    Ok(())
}

pub(super) fn candidates(source: &Source) -> Result<Vec<Candidate>> {
    source.validate()?;
    // Keep the inspected configuration fixed for all requests. The private temporary
    // file is removed on drop and raw credentials are never printed or parsed.
    let file = File::open(&source.kubeconfig)?;
    if !file.metadata()?.is_file() {
        bail!("Kubeconfig must be a regular file");
    }
    let mut input = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut input)?;
    if input.len() > 1024 * 1024 {
        bail!("Kubeconfig exceeds 1 MiB");
    }
    let mut snapshot = tempfile::NamedTempFile::new()?;
    snapshot.write_all(&input)?;
    snapshot.flush()?;
    let command = |args: &[&str]| -> Result<Value> {
        let mut cmd = external_command("kubectl");
        cmd.env("KUBECTL_KUBERC", "false").env_remove("KUBECONFIG");
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd
            .arg("--kubeconfig")
            .arg(snapshot.path())
            .arg("--context")
            .arg(&source.context)
            .arg("--request-timeout=10s")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("Could not launch kubectl")?;
        if !output.status.success() {
            bail!(
                "kubectl could not complete the local review ({})",
                output.status
            );
        }
        serde_json::from_slice(&output.stdout).context("Invalid JSON from kubectl")
    };
    let config = command(&["config", "view", "--minify", "-o", "json"])?;
    validate_config(&config)?;
    let mut found = Vec::new();
    let cutoff = Utc::now() - TimeDelta::days(7);
    for (kind, base) in [("Job", "/apis/batch/v1"), ("Pod", "/api/v1")] {
        let route = format!(
            "{base}/namespaces/{}/{}s",
            source.namespace,
            kind.to_ascii_lowercase()
        );
        let response = command(&["get", "--raw", &route])?;
        let items = response["items"]
            .as_array()
            .context("Expected a Kubernetes list")?;
        found.extend(
            items
                .iter()
                .filter_map(|item| classify(item, kind, source, cutoff)),
        );
    }
    Ok(found)
}

fn validate_config(config: &Value) -> Result<()> {
    let cluster = &config["clusters"][0]["cluster"];
    let user = &config["users"][0]["user"];
    let endpoint = cluster["server"]
        .as_str()
        .context("Kubernetes context has no API endpoint")?;
    let host = endpoint
        .strip_prefix("https://")
        .context("Use a local HTTPS Kubernetes endpoint")?;
    let ip = host
        .parse::<SocketAddr>()
        .map(|address| address.ip())
        .or_else(|_| {
            let literal = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host);
            literal.parse::<IpAddr>()
        })?;
    if !ip.is_loopback()
        || cluster.get("proxy-url").is_some()
        || cluster["insecure-skip-tls-verify"].as_bool() == Some(true)
        || user.get("exec").is_some()
        || user.get("auth-provider").is_some()
        || user.get("tokenFile").is_some()
    {
        bail!("Use a local loopback Kubernetes endpoint with verified TLS and no proxy or external credential helper");
    }
    for path in [
        &cluster["certificate-authority"],
        &user["client-certificate"],
        &user["client-key"],
    ] {
        if path
            .as_str()
            .is_some_and(|p| !std::path::Path::new(p).is_absolute())
        {
            bail!("Use embedded certificates or absolute certificate paths in the kubeconfig");
        }
    }
    Ok(())
}

fn classify(item: &Value, kind: &str, source: &Source, cutoff: DateTime<Utc>) -> Option<Candidate> {
    let metadata = &item["metadata"];
    let name = metadata["name"].as_str()?;
    if metadata["namespace"].as_str()? != source.namespace {
        return None;
    }
    let status = &item["status"];
    let conditions = status["conditions"].as_array().into_iter().flatten();
    let (failed, finished) = if kind == "Job" {
        let terminal = conditions.filter(|c| c["status"] == "True");
        let failed = terminal.clone().any(|c| c["type"] == "Failed");
        let finished = terminal
            .filter(|c| c["type"] == "Complete" || c["type"] == "Failed")
            .filter_map(|c| timestamp(&c["lastTransitionTime"]))
            .max()
            .or_else(|| timestamp(&status["completionTime"]));
        if !failed && finished.is_none() {
            return None;
        }
        (failed, finished)
    } else {
        let phase = status["phase"].as_str()?;
        if !matches!(phase, "Succeeded" | "Failed") {
            return None;
        }
        let finished = [
            "containerStatuses",
            "initContainerStatuses",
            "ephemeralContainerStatuses",
        ]
        .into_iter()
        .flat_map(|key| status[key].as_array().into_iter().flatten())
        .filter_map(|c| timestamp(&c["state"]["terminated"]["finishedAt"]))
        .max();
        (phase == "Failed", finished)
    };
    let (english, japanese, eligible) = if failed {
        (
            "Failed workload; keep it for investigation",
            "失敗した処理です。調査に使えるよう保護しています。",
            false,
        )
    } else if ["ownerReferences", "finalizers"]
        .iter()
        .any(|key| metadata[key].as_array().is_some_and(|v| !v.is_empty()))
        || !metadata["deletionTimestamp"].is_null()
        || status["active"].as_u64().unwrap_or(0) > 0
    {
        (
            "Managed, active, or terminating resource",
            "他の処理が管理しているか、処理中のため保護しています。",
            false,
        )
    } else if metadata["labels"]["still.disposable"] != "true" {
        (
            "Missing still.disposable=true label",
            "使い捨てを示す still.disposable=true ラベルがありません。",
            false,
        )
    } else if finished.is_none_or(|time| time > cutoff) {
        (
            "Completion unverified or less than 7 days ago",
            "終了時刻が確認できないか、終了から7日未満です。",
            false,
        )
    } else {
        (
            "Completed at least 7 days ago and marked disposable; verify logs before cleanup",
            "終了から7日以上経過した使い捨ての処理です。片付ける前にログを確認してください。",
            true,
        )
    };
    Some(Candidate {
        label: format!("{}/{}/{kind}/{name}", source.context, source.namespace),
        reason: Message::new(english, japanese),
        eligible,
    })
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value.as_str().and_then(|s| s.parse().ok())
}
