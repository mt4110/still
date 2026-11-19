use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub roots: Vec<PathBuf>,
    pub cleanup_roots: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    pub docker_context: Option<String>,
    #[serde(default)]
    pub docker_cache: bool,
    #[serde(default)]
    pub git_repos: Vec<PathBuf>,
    #[serde(default)]
    pub git_base: Option<String>,
    #[serde(default)]
    pub kube: Option<super::kube::Source>,
}

impl Profile {
    pub fn load(name: &str) -> Result<Self> {
        let path = profile_path(name)?;
        let file = File::open(&path)
            .with_context(|| format!("Could not open review profile {name:?} at {path:?}"))?;
        let profile: Self = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("Could not read review profile {name:?} at {path:?}"))?;
        if profile.roots.is_empty()
            || profile
                .roots
                .iter()
                .chain(&profile.cleanup_roots)
                .chain(&profile.outputs)
                .chain(&profile.git_repos)
                .any(|path| !path.is_absolute())
        {
            bail!("Review profile {name:?} must contain at least one scan root and only absolute paths");
        }
        Ok(profile)
    }

    pub fn save(&self, name: &str) -> Result<()> {
        self.write(name, true)
    }

    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    pub fn create(&self, name: &str) -> Result<()> {
        self.write(name, false)
    }

    fn write(&self, name: &str, overwrite: bool) -> Result<()> {
        let path = profile_path(name)?;
        let dir = path.parent().context("Missing review profile directory")?;
        fs::create_dir_all(dir)
            .with_context(|| format!("Could not create review profile directory {dir:?}"))?;
        let mut file = NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        let saved = if overwrite {
            file.persist(&path)
        } else {
            file.persist_noclobber(&path)
        };
        saved.with_context(|| format!("Could not save review profile {name:?} at {path:?}"))?;
        Ok(())
    }

    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    pub fn names() -> Result<Vec<String>> {
        let path = profile_directory()?;
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let filename = entry.file_name();
            let name = filename
                .to_str()
                .and_then(|name| name.strip_prefix("profile-")?.strip_suffix(".json"));
            if let Some(name) = name.filter(|name| validate_name(name).is_ok()) {
                names.push(name.to_owned());
            }
        }
        names.sort_unstable();
        Ok(names)
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    let name = name.as_bytes();
    let alphanumeric = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if name.len() > 64
        || !name.first().is_some_and(alphanumeric)
        || !name
            .iter()
            .all(|byte| alphanumeric(byte) || matches!(byte, b'-' | b'_'))
    {
        bail!("Use a profile name of 1–64 lowercase ASCII letters, digits, hyphens or underscores, starting with a letter or digit");
    }
    Ok(())
}

fn profile_path(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(profile_directory()?.join(format!("profile-{name}.json")))
}

fn profile_directory() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "antigravity", "still")
        .context("Could not determine the still configuration directory")?;
    Ok(dirs.config_dir().join("review-profiles"))
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn profile_names_are_single_unambiguous_path_components() {
        assert!(validate_name("weekly").is_ok());
        assert!(validate_name("work-2_local").is_ok());
        for name in [
            "",
            ".",
            "../weekly",
            "x/y",
            "x\\y",
            "C:weekly",
            "Weekly",
            "_work",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_name(&"a".repeat(65)).is_err());
    }
}
