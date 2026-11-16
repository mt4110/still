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
    let repo = inventory::safe_dir(&args.repo)?;
    let root = inventory::safe_dir(&args.root)?;
    if !args.base.starts_with("refs/") {
        bail!("Use a full Git reference, such as refs/heads/main");
    }
    git::check(
        git::run(&repo, ["check-ref-format", &args.base])?,
        "check-ref-format",
    )?;
    let grafts = git::check(
        git::run(&repo, ["rev-parse", "--git-path", "info/grafts"])?,
        "git-path",
    )?;
    let grafts = repo.join(os_string(grafts.strip_suffix(b"\n").unwrap_or(&grafts))?);
    match grafts.symlink_metadata() {
        Ok(_) => bail!("The repository uses a grafted commit graph"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let baseline = resolve(&repo, &args.base)?;
    let pr_data = pull_requests(&repo, &args.github_prs)?;
    let entries = list(&repo)?;
    if !entries
        .first()
        .is_some_and(|entry| entry.path.canonicalize().is_ok_and(|p| p == repo))
    {
        bail!("--repo must be the primary checkout");
    }
    let mut scoped = entries.iter().skip(1).filter(|entry| {
        entry.path != root
            && (entry.path.starts_with(&root)
                || entry
                    .path
                    .canonicalize()
                    .is_ok_and(|p| p != root && p.starts_with(&root)))
    });
    if let Some(path) = &args.remove {
        let path = inventory::safe_dir(path)?;
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
            let outcome = inventory::safe_dir(&entry.path)
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
        _ => bail!("Git could not verify ancestry"),
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
        bail!("Locked, prunable, bare, or integration branch");
    }
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().join(".codex")));
    if codex_home.is_some_and(|home| {
        let worktrees = home.join("worktrees");
        path.starts_with(&worktrees)
            || worktrees
                .canonicalize()
                .is_ok_and(|root| path.starts_with(root))
    }) {
        bail!("Codex-managed. Use its worktree lifecycle to preserve the chat association.");
    }
    if std::env::current_dir()?.starts_with(path) {
        bail!("The current working directory is inside the worktree");
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
        matched.context("No matching commit or merged PR on the chosen baseline")?
    };

    verify_local_state(path, item, baseline)?;
    Ok(reason)
}

fn verify_local_state(path: &Path, item: &Worktree, baseline: &str) -> Result<()> {
    let marker = fs::symlink_metadata(path.join(".git"))?;
    if !marker.is_file() || inventory::is_link(&marker) {
        bail!("Expected a linked-worktree .git file");
    }
    if resolve(path, "HEAD")? != item.head {
        bail!("Worktree HEAD changed");
    }
    let root_meta = fs::symlink_metadata(path)?;
    let mut pending = vec![path.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if dir == path && entry.file_name() == ".git" {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if inventory::is_link(&metadata)
                || !inventory::same_device(&root_meta, &metadata)
                || super::artifacts::protected(&entry.path())
                || (!metadata.is_dir() && !metadata.is_file())
            {
                bail!(
                    "Worktree contains a protected or unsupported entry: {}",
                    inventory::display(&entry.path())
                );
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
        bail!("Executable Git filters are configured or Git config could not be verified");
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
        bail!("Has modified, untracked, or ignored files. Review generated outputs separately.");
    }
    let tracked = git::check(
        git::run(path, ["ls-files", "--stage", "-z"])?,
        "ls-files --stage",
    )?;
    if tracked
        .split(|&byte| byte == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        bail!("Contains Git submodules");
    }
    let flags = git::check(git::run(path, ["ls-files", "-v", "-z"])?, "ls-files -v")?;
    if flags.split(|&byte| byte == 0).any(|entry| {
        entry
            .first()
            .is_some_and(|flag| *flag == b'S' || flag.is_ascii_lowercase())
    }) {
        bail!("Contains sparse-checkout or assume-unchanged entries");
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
            bail!("Git has private configuration or an operation in progress");
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
        bail!("Worktree has private Git references");
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
            bail!("The worktree reflog contains local-only history");
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
