use super::*;
use anyhow::ensure;
use std::collections::{BTreeMap, BTreeSet};

const LIMIT: usize = 2048;
const PROJECTION: &str = r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},"auto_remove":{{json .HostConfig.AutoRemove}},"created":{{json .Created}},"state":{{json .State.Status}},"running":{{json .State.Running}},"paused":{{json .State.Paused}},"restarting":{{json .State.Restarting}},"started":{{json .State.StartedAt}},"finished":{{json .State.FinishedAt}},"restart_count":{{json .RestartCount}},"restart_policy":{{json .HostConfig.RestartPolicy.Name}},"mounts":[{{range $i,$v := .Mounts}}{{if $i}},{{end}}{"kind":{{json $v.Type}},"name":{{json $v.Name}},"source":{{json $v.Source}},"destination":{{json $v.Destination}},"rw":{{json $v.RW}}}{{end}}],"keep":{{json (index .Config.Labels "still.keep")}},"kubernetes":{{json (index .Config.Labels "io.kubernetes.pod.name")}},"kubernetes_uid":{{json (index .Config.Labels "io.kubernetes.pod.uid")}},"kubernetes_container":{{json (index .Config.Labels "io.kubernetes.container.name")}},"swarm":{{json (index .Config.Labels "com.docker.swarm.service.id")}},"swarm_task":{{json (index .Config.Labels "com.docker.swarm.task.id")}},"swarm_node":{{json (index .Config.Labels "com.docker.swarm.node.id")}},"compose_project":{{json (index .Config.Labels "com.docker.compose.project")}},"compose_service":{{json (index .Config.Labels "com.docker.compose.service")}}}"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pin {
    endpoint: String,
    id: String,
}
#[cfg_attr(test, derive(serde::Serialize))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct Metadata {
    id: String,
    name: String,
    image: String,
    auto_remove: bool,
    created: String,
    state: String,
    running: bool,
    paused: bool,
    restarting: bool,
    started: String,
    finished: String,
    restart_count: u64,
    restart_policy: String,
    mounts: Vec<Mount>,
    #[serde(deserialize_with = "present_option")]
    keep: Option<String>,
    #[serde(deserialize_with = "present_option")]
    kubernetes: Option<String>,
    #[serde(deserialize_with = "present_option")]
    kubernetes_uid: Option<String>,
    #[serde(deserialize_with = "present_option")]
    kubernetes_container: Option<String>,
    #[serde(deserialize_with = "present_option")]
    swarm: Option<String>,
    #[serde(deserialize_with = "present_option")]
    swarm_task: Option<String>,
    #[serde(deserialize_with = "present_option")]
    swarm_node: Option<String>,
    #[serde(deserialize_with = "present_option")]
    compose_project: Option<String>,
    #[serde(deserialize_with = "present_option")]
    compose_service: Option<String>,
}
#[cfg_attr(test, derive(serde::Serialize))]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Mount {
    kind: String,
    #[serde(deserialize_with = "present_option")]
    name: Option<String>,
    source: String,
    destination: String,
    rw: bool,
}

fn present_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

trait Backend {
    fn run(&self, endpoint: &str, args: &[&str]) -> Result<String>;
    fn inventory_output(&self, endpoint: &str, args: &[&str]) -> Result<String> {
        self.run(endpoint, args)
    }
    fn free(&self) -> Option<u64> {
        None
    }
}
struct Docker;
impl Backend for Docker {
    fn run(&self, endpoint: &str, args: &[&str]) -> Result<String> {
        command_output(command_on(endpoint).args(args), Duration::from_secs(30))
    }
    fn inventory_output(&self, endpoint: &str, args: &[&str]) -> Result<String> {
        partial_output(command_on(endpoint).args(args))
    }
    fn free(&self) -> Option<u64> {
        let home = directories::BaseDirs::new()?;
        let stat = rustix::fs::statvfs(home.home_dir()).ok()?;
        stat.f_bavail.checked_mul(stat.f_frsize)
    }
}
// Docker can exit nonzero while returning complete rows for surviving IDs.
// This path is inventory-only; prepare/apply still require successful commands.
fn partial_output(command: &mut std::process::Command) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::process::Stdio;
    use std::time::Instant;
    let mut output = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if output.metadata()?.len() > 4 * 1024 * 1024 || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Docker inventory exceeded its time or output limit");
        }
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    output.take(4 * 1024 * 1024 + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() <= 4 * 1024 * 1024,
        "Docker inventory output limit exceeded"
    );
    Ok(text)
}
fn daemon(backend: &impl Backend, endpoint: &str) -> Result<Pin> {
    validate_endpoint(endpoint)?;
    let id: String =
        serde_json::from_str(&backend.run(endpoint, &["info", "--format", "{{json .ID}}"])?)?;
    ensure!(!id.trim().is_empty(), "Docker daemon identity is missing");
    Ok(Pin {
        endpoint: endpoint.into(),
        id,
    })
}
fn verify(backend: &impl Backend, pin: &Pin) -> Result<()> {
    ensure!(
        daemon(backend, &pin.endpoint)? == *pin,
        "Docker daemon changed; refresh the inventory"
    );
    Ok(())
}
fn full_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn ids(backend: &impl Backend, pin: &Pin) -> Result<Vec<String>> {
    let output = backend.run(
        &pin.endpoint,
        &["container", "ls", "--all", "--quiet", "--no-trunc"],
    )?;
    let items: Vec<String> = output
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    ensure!(
        items.len() <= LIMIT,
        "Container inventory exceeds the bounded review limit"
    );
    ensure!(
        items.iter().all(|id| full_id(id)),
        "Docker returned incomplete container IDs"
    );
    ensure!(
        items.iter().collect::<BTreeSet<_>>().len() == items.len(),
        "Duplicate container IDs"
    );
    Ok(items)
}
fn metadata(
    backend: &impl Backend,
    pin: &Pin,
    requested: &[String],
) -> Result<BTreeMap<String, Metadata>> {
    metadata_mode(backend, pin, requested, false)
}
fn metadata_mode(
    backend: &impl Backend,
    pin: &Pin,
    requested: &[String],
    inventory: bool,
) -> Result<BTreeMap<String, Metadata>> {
    ensure!(
        requested.len() <= LIMIT && requested.iter().all(|s| full_id(s)),
        "Invalid container selection"
    );
    let mut result = BTreeMap::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    for batch in requested.chunks(32) {
        ensure!(
            std::time::Instant::now() < deadline,
            "Container metadata review timed out"
        );
        let mut args = vec!["container", "inspect", "--format", PROJECTION];
        args.extend(batch.iter().map(String::as_str));
        let output = if inventory {
            backend.inventory_output(&pin.endpoint, &args)?
        } else {
            backend.run(&pin.endpoint, &args)?
        };
        for line in output.lines().filter(|s| !s.is_empty()) {
            let item: Metadata = match serde_json::from_str(line) {
                Ok(item) => item,
                Err(_) if inventory => continue,
                Err(err) => return Err(err).context("Incomplete container metadata"),
            };
            ensure!(
                batch.contains(&item.id),
                "Docker inspect returned a different container ID"
            );
            ensure!(
                result.insert(item.id.clone(), item).is_none(),
                "Duplicate inspect result"
            );
        }
        // Confirmation and removal require complete successful output for all IDs.
        ensure!(
            inventory || batch.iter().all(|id| result.contains_key(id)),
            "Docker inspect omitted a container"
        );
    }
    Ok(result)
}
fn timestamp(value: &str) -> Option<DateTime<Utc>> {
    let time = DateTime::parse_from_rfc3339(value)
        .ok()?
        .with_timezone(&Utc);
    (time.timestamp() > 0).then_some(time)
}
impl Metadata {
    fn blocked_reason(&self) -> Option<Message> {
        let pair = if !full_id(&self.id)
            || self.name.is_empty()
            || self.image.is_empty()
            || timestamp(&self.created).is_none()
            || DateTime::parse_from_rfc3339(&self.started).is_err()
            || DateTime::parse_from_rfc3339(&self.finished).is_err()
        {
            (
                "Container identity or timestamps are incomplete",
                "ID または日時情報が不完全です",
            )
        } else if self.auto_remove {
            (
                "Automatic removal is enabled; retained-volume behavior is not guaranteed",
                "自動削除が有効なため、ボリューム保持を保証できません",
            )
        } else if self.running
            || self.paused
            || self.restarting
            || !matches!(self.state.as_str(), "exited" | "created")
        {
            (
                "Only stopped or created containers can be selected",
                "停止済み・作成済みのコンテナだけを選択できます",
            )
        } else if self
            .keep
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        {
            (
                "Protected by still.keep=true",
                "still.keep=true により保護されています",
            )
        } else if [
            &self.kubernetes,
            &self.kubernetes_uid,
            &self.kubernetes_container,
            &self.swarm,
            &self.swarm_task,
            &self.swarm_node,
        ]
        .iter()
        .any(|v| v.is_some())
        {
            (
                "Managed by Kubernetes or Swarm",
                "Kubernetes または Swarm の管理対象です",
            )
        } else if self.restart_policy != "no" {
            (
                "Automatic restart policy is configured",
                "自動再起動ポリシーが設定されています",
            )
        } else {
            return None;
        };
        Some(Message::new(pair.0, pair.1))
    }
    fn reason(&self) -> Message {
        self.blocked_reason().unwrap_or_else(|| Message::new(
            "Manual review required; stopped does not mean unused. Mounts, volumes and images are retained",
            "手動確認が必要です。停止済みでも未使用とは限りません。マウント・ボリューム・イメージは保持します",
        ))
    }
    fn eligible(&self) -> bool {
        self.blocked_reason().is_none()
    }
    fn update(&self, item: &mut Resource) {
        item.id = self.id.clone();
        item.name = self.name.trim_start_matches('/').to_owned();
        item.status = self.state.clone();
        item.created = timestamp(&self.created);
        item.started = timestamp(&self.started);
        item.finished = timestamp(&self.finished);
        item.restart_count = self.restart_count;
        item.eligible = self.eligible();
        item.reason = self.reason();
        item.estimated_bytes = size_bytes(&item.size);
    }
}

pub(super) fn capture(endpoint: &str) -> Result<Pin> {
    daemon(&Docker, endpoint)
}

pub(super) fn enrich(report: &mut Report, pin: Pin) -> Result<()> {
    let backend = Docker;
    verify(&backend, &pin)?;
    let listed = ids(&backend, &pin)?;
    // Keep every inventory row visible even when a daemon returns partial inspect.
    let mut inspected = BTreeMap::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    for batch in listed.chunks(32) {
        if std::time::Instant::now() >= deadline {
            break;
        }
        if let Ok(items) = metadata_mode(&backend, &pin, batch, true) {
            inspected.extend(items);
        }
    }
    verify(&backend, &pin)?;
    let rows = &mut report
        .groups
        .iter_mut()
        .find(|(k, _)| *k == Kind::Container)
        .expect("container group")
        .1;
    for id in &listed {
        let matching: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.id.is_empty() && id.starts_with(&r.id))
            .map(|(i, _)| i)
            .collect();
        let index = if matching.len() == 1 {
            matching[0]
        } else {
            rows.push(Resource {
                id: id.clone(),
                name: id[..12].to_owned(),
                ..Resource::default()
            });
            rows.len() - 1
        };
        if let Some(meta) = inspected.get(id) {
            meta.update(&mut rows[index]);
        }
    }
    report.daemon = Some(pin);
    report.snapshots = inspected;
    Ok(())
}

pub(crate) struct Review {
    pub items: Vec<Resource>,
    pub context: String,
    pin: Pin,
    snapshots: BTreeMap<String, Metadata>,
}
pub(crate) struct Receipt {
    pub outcomes: Vec<(String, Result<()>)>,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub estimated_bytes: Option<u64>,
}
impl Review {
    pub fn prepare(report: &Report, selection: &BTreeSet<String>) -> Result<Self> {
        Self::prepare_with(&Docker, report, selection)
    }
    fn prepare_with(
        backend: &impl Backend,
        report: &Report,
        selection: &BTreeSet<String>,
    ) -> Result<Self> {
        ensure!(
            !selection.is_empty() && selection.len() <= LIMIT,
            "Select containers for review"
        );
        let pin = report
            .daemon
            .clone()
            .context("Refresh the Docker inventory")?;
        verify(backend, &pin)?;
        let requested: Vec<_> = selection.iter().cloned().collect();
        let snapshots = metadata(backend, &pin, &requested)?;
        let mut items = Vec::new();
        for id in selection {
            let old = report
                .snapshots
                .get(id)
                .context("Container metadata was not verified in the inventory")?;
            let fresh = &snapshots[id];
            ensure!(
                old == fresh,
                "Container metadata changed; refresh and review again"
            );
            ensure!(fresh.eligible(), "{}", fresh.reason());
            let mut item = report
                .groups
                .iter()
                .filter(|(kind, _)| *kind == Kind::Container)
                .flat_map(|(_, rows)| rows)
                .find(|r| &r.id == id)
                .cloned()
                .context("Container is not in this inventory")?;
            fresh.update(&mut item);
            items.push(item);
        }
        verify(backend, &pin)?;
        Ok(Self {
            items,
            context: report.context.clone(),
            pin,
            snapshots,
        })
    }
    pub fn apply(self) -> Receipt {
        self.apply_with(&Docker)
    }
    fn apply_with(self, backend: &impl Backend) -> Receipt {
        let before = backend.free();
        let estimated_bytes = self
            .items
            .iter()
            .try_fold(0u64, |sum, item| sum.checked_add(item.estimated_bytes?));
        let mut outcomes = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        for item in &self.items {
            let result = (|| -> Result<()> {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "Container removal review timed out; remaining items were not removed"
                );
                verify(backend, &self.pin)?;
                let fresh = metadata(backend, &self.pin, std::slice::from_ref(&item.id))?;
                let current = fresh.get(&item.id).context("Container metadata missing")?;
                ensure!(
                    Some(current) == self.snapshots.get(&item.id) && current.eligible(),
                    "Container changed after confirmation; removal refused"
                );
                verify(backend, &self.pin)?;
                // Intentionally no force, volume, image, or prune flags.
                backend.run(&self.pin.endpoint, &["container", "rm", &item.id])?;
                verify(backend, &self.pin)?;
                ensure!(
                    !ids(backend, &self.pin)?.contains(&item.id),
                    "Container absence could not be verified"
                );
                Ok(())
            })();
            outcomes.push((item.id.clone(), result));
        }
        Receipt {
            outcomes,
            before,
            after: backend.free(),
            estimated_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    struct Stub {
        calls: RefCell<Vec<Vec<String>>>,
        changed_daemon: RefCell<bool>,
        records: RefCell<BTreeMap<String, Metadata>>,
        fail_rm: RefCell<BTreeSet<String>>,
        retain_after_rm: RefCell<bool>,
        wrong_id: RefCell<bool>,
        omit: RefCell<bool>,
    }
    impl Backend for Stub {
        fn run(&self, _: &str, args: &[&str]) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|v| v.to_string()).collect());
            match args {
                ["info", ..] => Ok(if *self.changed_daemon.borrow() {
                    "\"other\""
                } else {
                    "\"original\""
                }
                .into()),
                ["container", "ls", ..] => Ok(self
                    .records
                    .borrow()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")),
                ["container", "inspect", "--format", _, requested @ ..] => requested
                    .iter()
                    .skip(usize::from(*self.omit.borrow()))
                    .map(|id| {
                        let records = self.records.borrow();
                        let meta = records.get(*id).context("missing")?;
                        let mut value = serde_json::to_value(meta)?;
                        if *self.wrong_id.borrow() {
                            value["id"] = "f".repeat(64).into();
                        }
                        Ok(value.to_string())
                    })
                    .collect::<Result<Vec<_>>>()
                    .map(|v| v.join("\n")),
                ["container", "rm", id] => {
                    if self.fail_rm.borrow().contains(*id) {
                        anyhow::bail!("simulated removal failure");
                    }
                    if !*self.retain_after_rm.borrow() {
                        self.records.borrow_mut().remove(*id);
                    }
                    Ok(id.to_string())
                }
                _ => anyhow::bail!("unexpected command"),
            }
        }
        fn free(&self) -> Option<u64> {
            Some(123)
        }
    }
    fn fixture_value(id: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "name":"/fixture", "image":"sha256:fixture", "auto_remove":false, "created":"2026-01-01T00:00:00Z", "state":"exited", "running":false, "paused":false, "restarting":false, "started":"2026-01-01T00:00:01Z", "finished":"2026-01-01T00:00:02Z", "restart_count":0,"restart_policy":"no","mounts":[],"keep":null,"kubernetes":null,"kubernetes_uid":null,"kubernetes_container":null,"swarm":null,"swarm_task":null,"swarm_node":null,"compose_project":null,"compose_service":null})
    }
    fn setup(count: usize) -> (Stub, Report, BTreeSet<String>) {
        let backend = Stub::default();
        let mut report = parse(
            "colima".into(),
            r#"{"Containers":[],"Images":[],"Volumes":[],"BuildCache":[]}"#,
        )
        .unwrap();
        report.daemon = Some(Pin {
            endpoint: "unix:///test.sock".into(),
            id: "original".into(),
        });
        let mut selected = BTreeSet::new();
        for n in 1..=count {
            let id = format!("{n:064x}");
            let meta: Metadata = serde_json::from_value(fixture_value(&id)).unwrap();
            let mut resource = Resource {
                size: "2MB".into(),
                ..Default::default()
            };
            meta.update(&mut resource);
            report.groups[0].1.push(resource);
            report.snapshots.insert(id.clone(), meta.clone());
            backend.records.borrow_mut().insert(id.clone(), meta);
            selected.insert(id);
        }
        (backend, report, selected)
    }
    fn no_removals(backend: &Stub) {
        assert!(!backend
            .calls
            .borrow()
            .iter()
            .any(|v| v.get(1).is_some_and(|s| s == "rm")));
    }
    #[test]
    fn pin_change_blocks_prepare_and_apply() {
        let (backend, report, selected) = setup(1);
        let review = Review::prepare_with(&backend, &report, &selected).unwrap();
        *backend.changed_daemon.borrow_mut() = true;
        assert!(Review::prepare_with(&backend, &report, &selected).is_err());
        assert!(review.apply_with(&backend).outcomes[0].1.is_err());
        no_removals(&backend);
    }
    #[test]
    fn start_or_metadata_changes_invalidate_confirmation() {
        for (field, value) in [
            ("state", "running"),
            ("started", "2026-01-02T00:00:00Z"),
            ("keep", "true"),
            ("restart_policy", "always"),
        ] {
            let (backend, report, selected) = setup(1);
            let review = Review::prepare_with(&backend, &report, &selected).unwrap();
            let id = selected.first().unwrap();
            let mut records = backend.records.borrow_mut();
            let meta = records.get_mut(id).unwrap();
            match field {
                "state" => meta.state = value.into(),
                "started" => meta.started = value.into(),
                "keep" => meta.keep = Some(value.into()),
                _ => meta.restart_policy = value.into(),
            }
            drop(records);
            assert!(review.apply_with(&backend).outcomes[0].1.is_err());
            no_removals(&backend);
        }
    }
    #[test]
    fn inspect_id_mismatch_and_missing_results_fail_closed() {
        let (backend, report, selected) = setup(1);
        *backend.wrong_id.borrow_mut() = true;
        assert!(Review::prepare_with(&backend, &report, &selected).is_err());
        *backend.wrong_id.borrow_mut() = false;
        *backend.omit.borrow_mut() = true;
        assert!(Review::prepare_with(&backend, &report, &selected).is_err());
        no_removals(&backend);
    }
    #[test]
    fn partial_receipt_preserves_failures_and_exact_removal_flags() {
        let (backend, report, selected) = setup(2);
        backend
            .fail_rm
            .borrow_mut()
            .insert(selected.first().unwrap().clone());
        let receipt = Review::prepare_with(&backend, &report, &selected)
            .unwrap()
            .apply_with(&backend);
        assert!(receipt.outcomes[0].1.is_err());
        assert!(receipt.outcomes[1].1.is_ok());
        assert_eq!(receipt.before, Some(123));
        assert_eq!(receipt.after, Some(123));
        assert_eq!(receipt.estimated_bytes, Some(4_000_000));
        let calls = backend.calls.borrow();
        let removes: Vec<_> = calls
            .iter()
            .filter(|c| c.get(1).is_some_and(|s| s == "rm"))
            .collect();
        assert_eq!(removes.len(), 2);
        for call in removes {
            assert_eq!(call.len(), 3);
            assert_eq!(call[0], "container");
            assert!(full_id(&call[2]));
        }
    }
    #[test]
    fn running_managed_keep_and_restart_policy_are_protected() {
        let base: Metadata = serde_json::from_value(fixture_value(&"a".repeat(64))).unwrap();
        assert!(base.eligible());
        for variant in 0..8 {
            let mut item = base.clone();
            match variant {
                0 => item.running = true,
                1 => item.paused = true,
                2 => item.restarting = true,
                3 => item.keep = Some("true".into()),
                4 => item.kubernetes = Some("pod".into()),
                5 => item.swarm = Some("service".into()),
                6 => item.restart_policy = "on-failure".into(),
                _ => item.created = String::new(),
            }
            assert!(!item.eligible());
        }
    }
    #[test]
    fn short_ids_and_new_unreviewed_containers_are_never_selected() {
        let (backend, report, _) = setup(1);
        assert!(Review::prepare_with(&backend, &report, &BTreeSet::from(["abc".into()])).is_err());
        let unknown = "f".repeat(64);
        backend.records.borrow_mut().insert(
            unknown.clone(),
            serde_json::from_value(fixture_value(&unknown)).unwrap(),
        );
        assert!(Review::prepare_with(&backend, &report, &BTreeSet::from([unknown])).is_err());
        no_removals(&backend);
    }
    #[test]
    fn inventory_retains_complete_rows_but_confirmation_rejects_partial_batches() {
        let (backend, report, selected) = setup(2);
        *backend.omit.borrow_mut() = true;
        let requested: Vec<_> = selected.into_iter().collect();
        let pin = report.daemon.as_ref().unwrap();
        let rows = metadata_mode(&backend, pin, &requested, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows.contains_key(&requested[1]));
        assert!(metadata(&backend, pin, &requested).is_err());
    }
    #[test]
    fn missing_nullable_protection_fields_are_incomplete_metadata() {
        let mut value = fixture_value(&"a".repeat(64));
        value.as_object_mut().unwrap().remove("keep");
        assert!(serde_json::from_value::<Metadata>(value).is_err());
    }
    #[test]
    fn nonzero_inventory_command_keeps_only_bounded_stdout() {
        let output = partial_output(
            std::process::Command::new("/bin/sh")
                .args(["-c", "printf '%s\\n' '{\"id\":\"fixture\"}'; exit 1"]),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap()["id"],
            "fixture"
        );
    }
    #[test]
    fn rename_image_and_auto_remove_changes_reject_the_confirmed_snapshot() {
        for change in 0..3 {
            let (backend, report, selected) = setup(1);
            let review = Review::prepare_with(&backend, &report, &selected).unwrap();
            let id = selected.first().unwrap();
            let mut records = backend.records.borrow_mut();
            let item = records.get_mut(id).unwrap();
            match change {
                0 => item.name = "/renamed".into(),
                1 => item.image = "sha256:changed".into(),
                _ => item.auto_remove = true,
            }
            drop(records);
            assert!(review.apply_with(&backend).outcomes[0].1.is_err());
            no_removals(&backend);
        }
        let mut meta: Metadata = serde_json::from_value(fixture_value(&"a".repeat(64))).unwrap();
        meta.auto_remove = true;
        assert!(!meta.eligible());
        assert!(meta.reason().to_string().contains("Automatic removal"));
    }
    #[test]
    fn successful_rm_exit_requires_verified_absence() {
        let (backend, report, selected) = setup(1);
        *backend.retain_after_rm.borrow_mut() = true;
        let receipt = Review::prepare_with(&backend, &report, &selected)
            .unwrap()
            .apply_with(&backend);
        assert!(receipt.outcomes[0].1.is_err());
        assert!(receipt.outcomes[0]
            .1
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("absence"));
    }
}
