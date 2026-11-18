use super::message::Message;
use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

pub fn root_for(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .map(Path::to_owned)
}

pub fn run(root: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<Output> {
    let mut command = super::external_command("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--no-pager", "--no-replace-objects"])
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context(Message::new(
            "Could not run Git",
            "Git を実行できませんでした。Git が使える状態か確認してください。",
        ))
}

pub fn check(output: Output, operation: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        return Err(Message::new(
            format!("Git {operation} failed ({})", output.status),
            format!("Git で安全性を確認できませんでした: {operation}"),
        )
        .into());
    }
    Ok(output.stdout)
}

pub fn protect_tracked(path: &Path, explicitly_declared: bool) -> Result<()> {
    let Some(repo) = root_for(path) else {
        if explicitly_declared {
            return Ok(());
        }
        return Err(Message::new("No Git project detected. Use --output for a dedicated script output.", "Git プロジェクトが見つかりません。再生成できる専用フォルダなら、出力先として登録してください。").into());
    };
    if repo == path {
        return Err(Message::new(
            "The output path is itself a Git repository",
            "候補そのものが Git リポジトリです。",
        )
        .into());
    }
    let prefix = check(run(&repo, ["rev-parse", "--show-prefix"])?, "rev-parse")?;
    if !prefix.iter().all(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err(Message::new(
            "Git root does not match the detected project",
            "検出したプロジェクトと Git のルートが一致しません。",
        )
        .into());
    }
    let relative = path.strip_prefix(&repo)?;
    // A case-insensitive filesystem can resolve a tracked path under a different spelling.
    let mut pathspec = OsString::from(":(top,icase,literal)");
    // Git pathspecs use forward slashes, including on Windows.
    for (index, component) in relative.components().enumerate() {
        if index > 0 {
            pathspec.push("/");
        }
        pathspec.push(component.as_os_str());
    }
    let args = [OsStr::new("ls-files"), OsStr::new("-z"), OsStr::new("--")];
    if !check(
        run(&repo, args.into_iter().chain([pathspec.as_os_str()]))?,
        "ls-files",
    )?
    .is_empty()
    {
        return Err(Message::new(
            "Contains files tracked by Git",
            "Git で追跡しているファイルを含むため保護しています。",
        )
        .into());
    }
    if !explicitly_declared {
        let args = [
            OsStr::new("ls-files"),
            OsStr::new("--others"),
            OsStr::new("--exclude-standard"),
            OsStr::new("-z"),
            OsStr::new("--"),
            pathspec.as_os_str(),
        ];
        if !check(run(&repo, args)?, "ls-files --others")?.is_empty() {
            return Err(Message::new("Contains files outside Git ignore rules. Check them or declare a dedicated --output.", "Git の無視ルールに含まれない未追跡ファイルがあります。残すものが混ざっていないか確認してください。").into());
        }
    }
    Ok(())
}
