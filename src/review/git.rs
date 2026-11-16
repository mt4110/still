use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub fn root_for(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .map(Path::to_owned)
}

pub fn run(root: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<Output> {
    let mut command = Command::new("git");
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
        .context("Could not run Git")
}

pub fn check(output: Output, operation: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        bail!("Git {operation} failed ({})", output.status);
    }
    Ok(output.stdout)
}

pub fn protect_tracked(path: &Path, explicitly_declared: bool) -> Result<()> {
    let Some(repo) = root_for(path) else {
        if explicitly_declared {
            return Ok(());
        }
        bail!("No Git project detected. Use --output for a dedicated script output.");
    };
    if repo == path {
        bail!("The output path is itself a Git repository");
    }
    let prefix = check(run(&repo, ["rev-parse", "--show-prefix"])?, "rev-parse")?;
    if !prefix.iter().all(|byte| matches!(byte, b'\n' | b'\r')) {
        bail!("Git root does not match the detected project");
    }
    let relative = path.strip_prefix(&repo)?;
    // A case-insensitive filesystem can resolve a tracked path under a different spelling.
    let mut pathspec = OsString::from(":(top,icase,literal)");
    pathspec.push(relative);
    let args = [OsStr::new("ls-files"), OsStr::new("-z"), OsStr::new("--")];
    if !check(
        run(&repo, args.into_iter().chain([pathspec.as_os_str()]))?,
        "ls-files",
    )?
    .is_empty()
    {
        bail!("Contains files tracked by Git");
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
            bail!("Contains files outside Git ignore rules. Check them or declare a dedicated --output.");
        }
    }
    Ok(())
}
