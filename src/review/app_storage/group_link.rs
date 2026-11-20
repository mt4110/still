//! One explicitly selected app, one exact group identifier. No app enumeration.
use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Declaration {
    Match,
    NoMatch,
    NoGroups,
}

fn valid_group(group: &str) -> bool {
    !group.is_empty() && group.len() <= 1024 && !group.chars().any(char::is_control)
}

fn parse_groups(json: &str, group: &str) -> Result<Declaration> {
    let value: serde_json::Value = serde_json::from_str(json).context("Invalid entitlements")?;
    let object = value
        .as_object()
        .context("Expected an entitlements dictionary")?;
    let Some(groups) = object.get("com.apple.security.application-groups") else {
        return Ok(Declaration::NoGroups);
    };
    let groups = groups
        .as_array()
        .context("Invalid App Groups entitlement")?;
    let groups = groups
        .iter()
        .map(|value| value.as_str().context("Invalid App Group identifier"))
        .collect::<Result<Vec<_>>>()?;
    Ok(if groups.contains(&group) {
        Declaration::Match
    } else {
        Declaration::NoMatch
    })
}

// plutil parses the platform plist format. Keep entitlement data out of logs,
// saved reports and the UI; temporary files use private OS-created handles.
fn plist_to_json(input: &[u8]) -> Result<String> {
    let mut source = tempfile::tempfile()?;
    source.write_all(input)?;
    source.seek(SeekFrom::Start(0))?;
    let mut output = tempfile::tempfile()?;
    let mut child = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::from(source))
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start entitlement parser")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if output.metadata()?.len() > 4 * 1024 * 1024 {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Entitlement data is too large");
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "Could not parse entitlement data");
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Entitlement parser timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut result = String::new();
    output
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut result)?;
    ensure!(
        result.len() <= 4 * 1024 * 1024,
        "Entitlement data is too large"
    );
    Ok(result)
}

fn verify(app: &Path) -> Result<()> {
    super::command_output(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--all-architectures", "--"])
            .arg(app),
        Duration::from_secs(30),
    )
    .context("The application's signature could not be verified")?;
    Ok(())
}

fn main_executable(app: &Path) -> Result<std::path::PathBuf> {
    let contents = app.join("Contents");
    let executable_dir = contents.join("MacOS");
    for directory in [&contents, &executable_dir] {
        let metadata = std::fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Application layout could not be verified"
        );
    }
    let info = contents.join("Info.plist");
    let info_meta = std::fs::symlink_metadata(&info)?;
    ensure!(
        info_meta.is_file() && !info_meta.file_type().is_symlink(),
        "Application metadata could not be verified"
    );
    let name = super::command_output(
        Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleExecutable", "raw", "-o", "-", "--"])
            .arg(&info),
        Duration::from_secs(10),
    )
    .context("Could not read the application executable name")?;
    let name = std::ffi::OsStr::new(name.trim());
    let mut components = Path::new(name).components();
    ensure!(
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none(),
        "Invalid application executable name"
    );
    let executable = executable_dir.join(name);
    let metadata = std::fs::symlink_metadata(&executable)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Application executable could not be verified"
    );
    Ok(executable)
}

fn file_stamp(path: &Path) -> Result<(u64, u64, i64, i64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "Application entry changed during inspection"
    );
    Ok((
        meta.dev(),
        meta.ino(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.len(),
    ))
}

pub(crate) fn inspect(app: &Path, group: &str) -> Result<Declaration> {
    use std::os::unix::fs::MetadataExt;
    ensure!(valid_group(group), "Enter a valid App Group identifier");
    ensure!(
        app.is_absolute() && app.extension().is_some_and(|ext| ext == "app"),
        "Select an absolute .app bundle"
    );
    let metadata = std::fs::symlink_metadata(app).context("Cannot inspect selected application")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Expected a regular application bundle"
    );
    let app = app.canonicalize()?;
    let initial = std::fs::symlink_metadata(&app)?;
    ensure!(
        (metadata.dev(), metadata.ino()) == (initial.dev(), initial.ino()),
        "Selected application changed during inspection"
    );
    let executable = main_executable(&app)?;
    let initial_executable = file_stamp(&executable)?;
    let initial_info = file_stamp(&app.join("Contents/Info.plist"))?;
    verify(&app)?;
    let xml = super::command_output(
        Command::new("/usr/bin/codesign")
            .args(["-d", "--xml", "--entitlements", "-", "--"])
            .arg(&app),
        Duration::from_secs(15),
    )
    .context("Could not read signed entitlement data")?;
    let result = if xml.trim().is_empty() {
        Declaration::NoGroups
    } else {
        parse_groups(&plist_to_json(xml.as_bytes())?, group)?
    };
    // A second check reduces the chance of reporting a changed bundle. It is
    // not an atomic guarantee against concurrent modification.
    verify(&app)?;
    let final_metadata = std::fs::symlink_metadata(&app)?;
    ensure!(
        (
            initial.dev(),
            initial.ino(),
            initial.ctime(),
            initial.ctime_nsec()
        ) == (
            final_metadata.dev(),
            final_metadata.ino(),
            final_metadata.ctime(),
            final_metadata.ctime_nsec()
        ),
        "Selected application changed during inspection"
    );
    ensure!(
        main_executable(&app)? == executable
            && file_stamp(&executable)? == initial_executable
            && file_stamp(&app.join("Contents/Info.plist"))? == initial_info,
        "Application executable or metadata changed during inspection"
    );
    Ok(result)
}

pub(crate) fn run(args: &crate::cli::GroupLinkArgs) -> Result<()> {
    let result = inspect(&args.app, &args.group)?;
    println!(
        "Selected app: {}\nApp Group: {}",
        args.app.display(),
        args.group
    );
    println!(
        "{}",
        match result {
            Declaration::Match => "The signed entitlement declares this exact group.",
            Declaration::NoMatch => "The signed entitlement does not declare this exact group.",
            Declaration::NoGroups =>
                "No App Groups entitlement was found in the selected app's signature.",
        }
    );
    println!("Signature integrity does not prove publisher identity. This checks one app only; it does not establish ownership, use, or all group members. Concurrent changes may not always be detected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_match_and_unknown_data_are_distinct() {
        let json =
            r#"{"com.apple.security.application-groups":["group.test.one","group.test.two"]}"#;
        assert_eq!(
            parse_groups(json, "group.test.one").unwrap(),
            Declaration::Match
        );
        assert_eq!(
            parse_groups(json, "group.test").unwrap(),
            Declaration::NoMatch
        );
        assert_eq!(
            parse_groups("{}", "group.test").unwrap(),
            Declaration::NoGroups
        );
        assert!(parse_groups(
            r#"{"com.apple.security.application-groups":"group.test"}"#,
            "group.test"
        )
        .is_err());
        assert!(parse_groups("invalid", "group.test").is_err());
        assert!(!valid_group("bad\nname"));
    }
}
