use super::message::Message;
use super::{bytes, filesystem, git, inventory};
use crate::cli::{ListFormat, WorktreeArgs};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Default, Eq, PartialEq)]
struct Worktree {
    path: PathBuf,
    head: String,
    branch: Option<String>,
    unavailable: bool,
}

#[derive(Serialize)]
struct Observation {
    path: String,
    head: String,
    reason: String,
    removable: bool,
    logical_bytes: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct ReviewCandidate {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub reason: Message,
    pub removable: bool,
}

#[derive(Deserialize)]
struct PullRequest {
    merged: bool,
    head: Head,
    base: Base,
    merge_commit_sha: Option<String>,
    number: u64,
}

#[derive(Deserialize)]
struct Head {
    sha: String,
}

#[derive(Deserialize)]
struct Base {
    #[serde(rename = "ref")]
    branch: String,
    repo: Repository,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
}

pub fn run(args: &WorktreeArgs) -> Result<()> {
    let repo = inventory::safe_workspace_dir(&args.repo)?;
    let root = inventory::safe_workspace_dir(&args.root)?;
    let baseline = resolve_baseline(&repo, &args.base)?;
    let pr_data = pull_requests(&repo, &args.github_prs)?;
    let entries = linked_worktrees(&repo)?;
    let roots = std::slice::from_ref(&root);
    let mut scoped = entries
        .iter()
        .filter(|entry| in_review_scope(&entry.path, roots));
    if let Some(path) = &args.remove {
        let path = inventory::safe_workspace_dir(path)?;
        if !strictly_below_root(&path, roots) {
            bail!("Not a linked worktree beneath --root");
        }
        let entry = scoped
            .find(|entry| entry.path.canonicalize().is_ok_and(|p| p == path))
            .context("Not a linked worktree beneath --root")?;
        let reason = eligible(&repo, entry, &path, &args.base, &baseline, &pr_data)?;
        if !io::stdin().is_terminal() {
            bail!("Worktree removal requires an interactive terminal");
        }
        print!("Remove {} ({reason})? [y/N] ", inventory::display(&path));
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        if response.trim() == "y" {
            if resolve(&repo, &args.base)? != baseline || !list(&repo)?.contains(entry) {
                bail!("The worktree or integration baseline changed after review");
            }
            eligible(&repo, entry, &path, &args.base, &baseline, &pr_data)?;
            remove(&repo, entry, &path, &baseline)?;
            println!("Removed {}", inventory::display(&path));
        }
        return Ok(());
    }

    let observations: Vec<_> = scoped
        .map(|entry| {
            let outcome = inventory::safe_workspace_dir(&entry.path)
                .and_then(|path| eligible(&repo, entry, &path, &args.base, &baseline, &pr_data));
            let removable = outcome.is_ok();
            let logical_bytes = if removable {
                inventory::scan(&entry.path, 4, None)
                    .ok()
                    .and_then(|scan| scan.directories.first().map(|row| row.usage.logical_bytes))
            } else {
                None
            };
            Observation {
                path: inventory::display(&entry.path),
                head: entry.head.clone(),
                reason: outcome.unwrap_or_else(|err| err.to_string()),
                removable,
                logical_bytes,
            }
        })
        .collect();

    match args.format {
        ListFormat::Json => println!(
            "{}",
            serde_json::json!({
                "base": args.base, "base_oid": baseline, "worktrees": observations,
            })
        ),
        ListFormat::Text => {
            println!("{} at {}", args.base, baseline);
            for item in observations {
                let status = if item.removable {
                    "Candidate"
                } else {
                    "Protected"
                };
                let size = item.logical_bytes.map(bytes).unwrap_or_else(|| "·".into());
                println!("{status}  {size:>12}  {}  ({})", item.path, item.reason);
            }
        }
    }
    Ok(())
}

pub(super) fn review_candidates(
    repo: &Path,
    cleanup_roots: &[PathBuf],
    base: &str,
) -> Result<Vec<ReviewCandidate>> {
    if cleanup_roots.is_empty() {
        return Ok(Vec::new());
    }
    let repo = inventory::safe_dir(repo)?;
    let entries: Vec<_> = linked_worktrees(&repo)?
        .into_iter()
        .filter(|entry| {
            in_review_scope(&entry.path, cleanup_roots) || is_codex_managed(&entry.path)
        })
        .collect();
    let baseline = entries
        .iter()
        .any(|entry| !is_codex_managed(&entry.path))
        .then(|| {
            resolve_baseline(&repo, base)
                .with_context(|| format!("Could not verify the local baseline {base}"))
        })
        .transpose()?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            if is_codex_managed(&entry.path) {
                return ReviewCandidate {
                    path: entry.path,
                    repo: repo.clone(),
                    reason: Message::new(
                        "Codex-managed worktree; managed in the associated chat",
                        "Codex 管理のワークツリーです。紐づいたチャットから管理してください。",
                    ),
                    removable: false,
                };
            }
            let outcome = inventory::safe_dir(&entry.path).and_then(|path| {
                if !strictly_below_root(&path, cleanup_roots) {
                    return Err(Message::new(
                        "Resolved path is outside the configured cleanup roots",
                        "解決したパスが設定済みの掃除対象外にあります。",
                    )
                    .into());
                }
                let baseline = baseline
                    .as_deref()
                    .context("Git integration baseline unavailable")?;
                let reason = eligible(&repo, &entry, &path, base, baseline, &[])?;
                filesystem::ensure_not_open(&path)?;
                Ok((path, reason))
            });
            let (path, reason, removable) = match outcome {
                Ok((path, reason)) => (
                    path,
                    Message::new(reason, format!("{base} への取り込みを確認しました。")),
                    true,
                ),
                Err(err) => (entry.path, Message::from_error(&err), false),
            };
            ReviewCandidate {
                path,
                repo: repo.clone(),
                reason,
                removable,
            }
        })
        .collect())
}

fn resolve_baseline(repo: &Path, base: &str) -> Result<String> {
    if !base.starts_with("refs/") {
        bail!("Use a full Git reference, such as refs/heads/main");
    }
    git::check(
        git::run(repo, ["check-ref-format", base])?,
        "check-ref-format",
    )?;
    let grafts = git::check(
        git::run(repo, ["rev-parse", "--git-path", "info/grafts"])?,
        "git-path",
    )?;
    let grafts = repo.join(os_string(grafts.strip_suffix(b"\n").unwrap_or(&grafts))?);
    match grafts.symlink_metadata() {
        Ok(_) => bail!("The repository uses a grafted commit graph"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    resolve(repo, base)
}

fn linked_worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let entries = list(repo)?;
    if !entries
        .first()
        .is_some_and(|entry| entry.path.canonicalize().is_ok_and(|p| p == repo))
    {
        bail!("--repo must be the primary checkout");
    }
    Ok(entries.into_iter().skip(1).collect())
}

fn in_review_scope(path: &Path, roots: &[PathBuf]) -> bool {
    strictly_below_root(path, roots)
        || path
            .canonicalize()
            .is_ok_and(|resolved| strictly_below_root(&resolved, roots))
}

fn strictly_below_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path != root && path.starts_with(root))
}

fn is_codex_managed(path: &Path) -> bool {
    let configured = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let default = directories::UserDirs::new().map(|dirs| dirs.home_dir().join(".codex"));
    configured.into_iter().chain(default).any(|home| {
        let worktrees = home.join("worktrees");
        path.starts_with(&worktrees)
            || worktrees
                .canonicalize()
                .is_ok_and(|root| path.starts_with(root))
    })
}

fn list(repo: &Path) -> Result<Vec<Worktree>> {
    let raw = git::check(
        git::run(repo, ["worktree", "list", "--porcelain", "-z"])?,
        "worktree list",
    )?;
    let mut entries = Vec::new();
    let mut current = Worktree::default();
    for field in raw.split(|&byte| byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            current.path = PathBuf::from(os_string(path)?);
        } else if let Some(head) = field.strip_prefix(b"HEAD ") {
            current.head = String::from_utf8(head.to_vec())?;
        } else if let Some(branch) = field.strip_prefix(b"branch ") {
            current.branch = Some(String::from_utf8(branch.to_vec())?);
        } else if field.starts_with(b"locked") || field.starts_with(b"prunable") || field == b"bare"
        {
            current.unavailable = true;
        } else if field.is_empty() && !current.path.as_os_str().is_empty() {
            entries.push(std::mem::take(&mut current));
        }
    }
    Ok(entries)
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}

#[cfg(windows)]
fn os_string(bytes: &[u8]) -> Result<OsString> {
    Ok(OsString::from(String::from_utf8(bytes.to_vec())?))
}

fn resolve(repo: &Path, reference: &str) -> Result<String> {
    let result = git::check(
        git::run(
            repo,
            [
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?,
        "rev-parse",
    )?;
    let oid = std::str::from_utf8(&result)?.trim();
    if !valid_oid(oid) {
        bail!("Invalid baseline commit");
    }
    Ok(oid.to_owned())
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ancestor(repo: &Path, head: &str, base: &str) -> Result<bool> {
    if !valid_oid(head) || !valid_oid(base) {
        bail!("Invalid Git object ID");
    }
    let output = git::run(repo, ["merge-base", "--is-ancestor", head, base])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Message::new(
            "Git could not verify ancestry",
            "Git でコミットの取り込み関係を確認できませんでした。",
        )
        .into()),
    }
}

fn eligible(
    repo: &Path,
    item: &Worktree,
    path: &Path,
    base_ref: &str,
    baseline: &str,
    prs: &[PullRequest],
) -> Result<String> {
    let base_branch = base_ref
        .strip_prefix("refs/heads/")
        .or_else(|| base_ref.strip_prefix("refs/remotes/origin/"));
    let branch = item
        .branch
        .as_deref()
        .and_then(|b| b.strip_prefix("refs/heads/"));
    if item.unavailable || branch == Some("main") || (branch.is_some() && branch == base_branch) {
        return Err(Message::new(
            "Locked, prunable, bare, or integration branch",
            "このワークツリーはロック済み、参照切れ、ベア、または統合先ブランチです。",
        )
        .into());
    }
    if is_codex_managed(path) {
        return Err(Message::new("Codex-managed. Use its worktree lifecycle to preserve the chat association.", "Codex 管理のワークツリーです。チャットとの紐づきを保つため Codex 側で管理してください。").into());
    }
    if std::env::current_dir()?.starts_with(path) {
        return Err(Message::new(
            "The current working directory is inside the worktree",
            "現在の作業フォルダがワークツリーの中にあります。",
        )
        .into());
    }
    let reason = if ancestor(repo, &item.head, baseline)? {
        format!("HEAD is reachable from {base_ref}")
    } else {
        let mut matched = None;
        for pr in prs {
            if pr.merged && pr.head.sha == item.head && base_branch == Some(pr.base.branch.as_str())
            {
                if let Some(result) = &pr.merge_commit_sha {
                    if ancestor(repo, result, baseline)? {
                        matched = Some(format!("PR #{} HEAD and merge result verified", pr.number));
                        break;
                    }
                }
            }
        }
        matched.context(Message::new(
            "No matching commit or merged PR on the chosen baseline",
            "指定した統合先で、該当コミットの取り込みやマージ済みの PR を確認できませんでした。",
        ))?
    };

    verify_local_state(path, item, baseline)?;
    Ok(reason)
}

fn verify_local_state(path: &Path, item: &Worktree, baseline: &str) -> Result<()> {
    let marker = fs::symlink_metadata(path.join(".git"))?;
    if !marker.is_file() || inventory::is_link(&marker) {
        return Err(Message::new(
            "Expected a linked-worktree .git file",
            "リンク済みワークツリーの .git ファイルを確認できません。",
        )
        .into());
    }
    if resolve(path, "HEAD")? != item.head {
        return Err(Message::new(
            "Worktree HEAD changed",
            "ワークツリーの HEAD が変わりました。再スキャンしてください。",
        )
        .into());
    }
    let root_meta = fs::symlink_metadata(path)?;
    #[cfg(target_os = "linux")]
    let mount = {
        let parent = path.parent().context("Worktree parent missing")?;
        let mount = filesystem::mount_id(parent)?;
        if filesystem::mount_id(path)? != mount {
            return Err(Message::new(
                "The worktree root is a Linux mount point",
                "ワークツリーが別にマウントされた領域にあります。",
            )
            .into());
        }
        mount
    };
    let mut pending = vec![path.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if dir == path && entry.file_name() == ".git" {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            #[cfg(target_os = "linux")]
            if filesystem::mount_id(&entry.path())? != mount {
                bail!(
                    "Worktree contains a separate Linux mount: {}",
                    inventory::display(&entry.path())
                );
            }
            if inventory::is_link(&metadata)
                || !inventory::same_device(&root_meta, &metadata)
                || super::artifacts::protected(&entry.path())
                || (!metadata.is_dir() && !metadata.is_file())
            {
                let entry = inventory::display(&entry.path());
                return Err(Message::new(
                    format!("Worktree contains a protected or unsupported entry: {entry}"),
                    format!("保護対象または確認できない項目を含みます: {entry}"),
                )
                .into());
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }

    // Git status may run repository-configured clean filters when inspecting a file.
    let filters = git::run(
        path,
        ["config", "--get-regexp", r"^filter\..*\.(clean|process)$"],
    )?;
    if !filters.stdout.is_empty() || filters.status.code() != Some(1) {
        return Err(Message::new(
            "Executable Git filters are configured or Git config could not be verified",
            "外部処理を実行する Git フィルタがあるか、Git の設定を確認できませんでした。",
        )
        .into());
    }
    let state = git::check(
        git::run(
            path,
            [
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
                "--ignore-submodules=none",
            ],
        )?,
        "status",
    )?;
    if !state.is_empty() {
        return Err(Message::new(
            "Has modified, untracked, or ignored files. Review generated outputs separately.",
            "変更済み・未追跡・無視対象のファイルが残っています。生成物は個別に確認してください。",
        )
        .into());
    }
    let tracked = git::check(
        git::run(path, ["ls-files", "--stage", "-z"])?,
        "ls-files --stage",
    )?;
    if tracked
        .split(|&byte| byte == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        return Err(Message::new(
            "Contains Git submodules",
            "Git サブモジュールを含むため保護しています。",
        )
        .into());
    }
    let flags = git::check(git::run(path, ["ls-files", "-v", "-z"])?, "ls-files -v")?;
    if flags.split(|&byte| byte == 0).any(|entry| {
        entry
            .first()
            .is_some_and(|flag| *flag == b'S' || flag.is_ascii_lowercase())
    }) {
        return Err(Message::new(
            "Contains sparse-checkout or assume-unchanged entries",
            "一部を展開しない設定か、変更検出から除外されたファイルがあります。",
        )
        .into());
    }
    let admin = git::check(
        git::run(path, ["rev-parse", "--absolute-git-dir"])?,
        "git-dir",
    )?;
    let admin = PathBuf::from(os_string(admin.strip_suffix(b"\n").unwrap_or(&admin))?);
    for name in [
        "index.lock",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "config.worktree",
        "BISECT_LOG",
    ] {
        if admin.join(name).symlink_metadata().is_ok() {
            return Err(Message::new(
                "Git has private configuration or an operation in progress",
                "ワークツリー固有の Git 設定があるか、Git の操作が進行中です。",
            )
            .into());
        }
    }
    let private_refs = git::check(
        git::run(
            path,
            [
                "for-each-ref",
                "--format=%(refname)",
                "refs/worktree/",
                "refs/rewritten/",
                "refs/bisect/",
            ],
        )?,
        "private refs",
    )?;
    if !private_refs.is_empty() {
        return Err(Message::new(
            "Worktree has private Git references",
            "ワークツリー固有の Git 参照が残っています。",
        )
        .into());
    }
    let reflog = git::check(
        git::run(path, ["reflog", "show", "--format=%H", "HEAD"])?,
        "reflog",
    )?;
    let retained = item
        .branch
        .as_ref()
        .map(|branch| resolve(path, branch))
        .transpose()?;
    for oid in std::str::from_utf8(&reflog)?.lines() {
        if !ancestor(path, oid, retained.as_deref().unwrap_or(baseline))?
            && !ancestor(path, oid, baseline)?
        {
            return Err(Message::new(
                "The worktree reflog contains local-only history",
                "ワークツリーの履歴にローカルだけのコミットがあります。",
            )
            .into());
        }
    }
    Ok(())
}

fn remove(repo: &Path, entry: &Worktree, path: &Path, baseline: &str) -> Result<()> {
    filesystem::ensure_not_open(path)?;
    let parent = path.parent().context("Worktree parent missing")?;
    let stage = parent.join(format!(".still-stage-{}", uuid::Uuid::new_v4()));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&stage)?;
    let staged = stage.join("worktree");
    let note_path = stage.join("original-path.txt");
    let mut note = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&note_path)?;
    writeln!(note, "{}", inventory::display(path))?;
    note.sync_all()?;
    drop(note);
    let path_arg = path.as_os_str();
    let staged_arg = staged.as_os_str();
    use std::ffi::OsStr;
    git::check(
        git::run(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("move"),
                OsStr::new("--"),
                path_arg,
                staged_arg,
            ],
        )?,
        "worktree move",
    )?;

    let result = (|| {
        // The same registered commit and original branch are still required after the move.
        let registered = list(repo)?.into_iter().any(|item| {
            !item.unavailable
                && item.head == entry.head
                && item.branch == entry.branch
                && item
                    .path
                    .canonicalize()
                    .is_ok_and(|p| p == staged.canonicalize().unwrap_or_else(|_| staged.clone()))
        });
        if !registered {
            bail!("Staged worktree is not registered at the expected commit");
        }
        verify_local_state(&staged, entry, baseline)?;
        git::check(
            git::run(
                repo,
                [
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    OsStr::new("--"),
                    staged_arg,
                ],
            )?,
            "worktree remove",
        )?;
        if staged.exists() || list(repo)?.iter().any(|item| item.path == staged) {
            bail!("Git still reports the staged worktree");
        }
        Ok::<(), anyhow::Error>(())
    })();
    result.with_context(|| {
        format!(
            "Check the staged worktree at {}",
            inventory::display(&staged)
        )
    })?;
    fs::remove_file(note_path)?;
    fs::remove_dir(stage)?;
    Ok(())
}

fn pull_requests(repo: &Path, numbers: &[u64]) -> Result<Vec<PullRequest>> {
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    let origin = git::check(
        git::run(repo, ["remote", "get-url", "origin"])?,
        "remote get-url",
    )?;
    let origin = std::str::from_utf8(&origin)?.trim();
    let repo = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .context("The configured origin is not GitHub.com")?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        })
    {
        bail!("Invalid GitHub repository in origin");
    }
    numbers.iter().map(|number| {
        let result = Command::new("gh").args([
            "api", "--hostname", "github.com", "--method", "GET", &format!("repos/{repo}/pulls/{number}"),
            "--jq", "{number,merged,merge_commit_sha,head:{sha:.head.sha},base:{ref:.base.ref,repo:{full_name:.base.repo.full_name}}}",
        ]).env("GH_PROMPT_DISABLED", "1").stdin(Stdio::null()).output()?;
        let result = git::check(result, "GitHub PR lookup")?;
        let pr = serde_json::from_slice::<PullRequest>(&result)?;
        if !pr.base.repo.full_name.eq_ignore_ascii_case(repo) || pr.number != *number {
            bail!("PR belongs to a different repository");
        }
        Ok(pr)
    }).collect()
}

#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
pub(super) struct RemovalReview {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub base: String,
    baseline: String,
    entry: Worktree,
}

#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
impl RemovalReview {
    pub fn prepare(repo: &Path, path: &Path, roots: &[PathBuf], base: &str) -> Result<Self> {
        let repo = inventory::safe_workspace_dir(repo)?;
        let path = inventory::safe_workspace_dir(path)?;
        anyhow::ensure!(
            strictly_below_root(&path, roots),
            "Worktree is outside the configured cleanup roots"
        );
        let baseline = resolve_baseline(&repo, base)?;
        let entry = linked_worktrees(&repo)?
            .into_iter()
            .find(|entry| entry.path.canonicalize().is_ok_and(|p| p == path))
            .context("This path is not a linked worktree")?;
        eligible(&repo, &entry, &path, base, &baseline, &[])?;
        filesystem::ensure_not_open(&path)?;
        Ok(Self {
            path,
            repo,
            base: base.into(),
            baseline,
            entry,
        })
    }

    pub fn head(&self) -> &str {
        &self.entry.head
    }

    pub fn apply(self) -> Result<()> {
        anyhow::ensure!(
            inventory::safe_workspace_dir(&self.repo)? == self.repo
                && inventory::safe_workspace_dir(&self.path)? == self.path
                && resolve_baseline(&self.repo, &self.base)? == self.baseline
                && linked_worktrees(&self.repo)?.contains(&self.entry),
            "The worktree or integration baseline changed after review"
        );
        eligible(
            &self.repo,
            &self.entry,
            &self.path,
            &self.base,
            &self.baseline,
            &[],
        )?;
        remove(&self.repo, &self.entry, &self.path, &self.baseline)
    }
}

#[cfg(all(test, feature = "desktop", target_os = "macos"))]
mod desktop_tests {
    use super::*;

    #[test]
    fn review_removes_only_the_unchanged_linked_checkout() -> Result<()> {
        let home = directories::BaseDirs::new().context("Home unavailable")?;
        let fixture = tempfile::tempdir_in(home.home_dir())?;
        let root = fixture.path().canonicalize()?;
        let repo = root.join("repo");
        fs::create_dir(&repo)?;
        let git = |args: &[&str]| git::check(git::run(&repo, args)?, "worktree test");
        git(&["init", "-b", "main"])?;
        git(&[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "fixture",
        ])?;
        let clean = root.join("merged");
        let changed = root.join("changed");
        git(&[
            "worktree",
            "add",
            "-b",
            "fixture-merged",
            clean.to_str().unwrap(),
        ])?;
        git(&[
            "worktree",
            "add",
            "-b",
            "fixture-changed",
            changed.to_str().unwrap(),
        ])?;
        let review = |path: &Path| {
            RemovalReview::prepare(&repo, path, std::slice::from_ref(&root), "refs/heads/main")
        };
        let changed_review = review(&changed)?;
        fs::write(changed.join("keep.txt"), "created after confirmation")?;
        assert!(changed_review.apply().is_err());
        assert!(changed.join("keep.txt").is_file());
        review(&clean)?.apply()?;
        assert!(!clean.exists());
        assert!(!linked_worktrees(&repo)?
            .iter()
            .any(|item| item.path == clean));
        git(&["show-ref", "--verify", "refs/heads/fixture-merged"])?;
        assert!(repo.join(".git").exists());
        Ok(())
    }
}
