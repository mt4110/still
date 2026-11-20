//! Explicit user choices for the Mac workspace cleanup flow. A kept path
//! protects both its contents and any candidate whose removal would include it.
use super::{display, Message};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

#[derive(Default, Debug)]
pub(super) struct KeepRules {
    paths: BTreeSet<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u8,
    paths: BTreeSet<PathBuf>,
}

fn settings_file() -> Result<PathBuf> {
    Ok(crate::storage_paths::project_dirs()
        .context("Could not locate the still settings directory")?
        .data_local_dir()
        .join("workspace-kept-paths.json"))
}

impl KeepRules {
    pub(super) fn load() -> Result<Self> {
        Self::read(&settings_file()?)
    }

    fn read(file: &Path) -> Result<Self> {
        let reader = match fs::File::open(file) {
            Ok(file) => BufReader::new(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let doc: Document = serde_json::from_reader(reader)?;
        anyhow::ensure!(doc.version == 1, "Unsupported keep settings version");
        anyhow::ensure!(
            doc.paths.iter().all(|p| valid_path(p)),
            "Invalid path in keep settings"
        );
        Ok(Self { paths: doc.paths })
    }

    pub(super) fn conflict(&self, path: &Path) -> Option<&Path> {
        // Check both directions. The ordered range starts with the first
        // possible descendant because Path compares whole components.
        path.ancestors()
            .find_map(|p| self.paths.get(p))
            .or_else(|| {
                self.paths
                    .range(path.to_owned()..)
                    .next()
                    .filter(|p| p.starts_with(path))
            })
            .map(PathBuf::as_path)
    }

    pub(super) fn reason(&self, path: &Path) -> Option<Message> {
        let kept = display(self.conflict(path)?);
        Some(Message::new(
            format!("Kept in the Mac app: {kept}"),
            format!("Mac アプリで「残す」に設定済み · {kept}"),
        ))
    }

    pub(super) fn update(path: &Path, keep: bool) -> Result<()> {
        Self::update_in(&settings_file()?, path, keep)
    }

    fn update_in(file: &Path, path: &Path, keep: bool) -> Result<()> {
        anyhow::ensure!(valid_path(path), "Expected an absolute folder path");
        let Self { mut paths } = Self::read(file)?;
        if keep {
            paths.insert(path.to_owned());
        } else {
            paths.remove(path);
        }
        let directory = file.parent().context("No keep settings directory")?;
        fs::create_dir_all(directory)?;
        let temp = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(temp.as_file(), &Document { version: 1, paths })?;
        temp.as_file().sync_all()?;
        temp.persist(file)?;
        Ok(())
    }

    pub(super) fn check(&self, path: &Path) -> Result<()> {
        if let Some(reason) = self.reason(path) {
            return Err(reason.into());
        }
        Ok(())
    }
}

fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path.parent().is_some()
        && !path.components().any(|c| c == Component::ParentDir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_children_block_enclosing_and_nested_outputs_across_sessions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("settings/rules.json");
        let output = Path::new("/workspace/demo/target");
        let kept = output.join("debug/incremental");
        KeepRules::update_in(&file, &kept, true)?;
        let rules = KeepRules::read(&file)?;
        for path in [output, kept.as_path(), &kept.join("part")] {
            assert_eq!(rules.conflict(path), Some(kept.as_path()));
        }
        assert!(rules.check(&output.join("debug/deps")).is_ok());
        assert!(rules.check(&output.with_file_name("target-other")).is_ok());
        KeepRules::update_in(&file, &kept, false)?;
        assert!(KeepRules::read(&file)?.check(output).is_ok());
        Ok(())
    }

    #[test]
    fn unknown_or_damaged_settings_never_get_replaced_by_an_empty_set() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("settings.json");
        for content in [
            "{",
            r#"{"version":2,"paths":[]}"#,
            r#"{"version":1,"paths":["/a/../b"]}"#,
        ] {
            fs::write(&file, content)?;
            assert!(KeepRules::update_in(&file, Path::new("/workspace/cache"), true).is_err());
            assert_eq!(fs::read_to_string(&file)?, content);
        }
        Ok(())
    }
}
