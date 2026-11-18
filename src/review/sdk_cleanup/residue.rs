use super::{app_storage, inventory, missing};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const ASSETS: &str = "/System/Library/AssetsV2";
const CORE_IMAGES: &str = "/Library/Developer/CoreSimulator/Images";

#[derive(Default)]
pub(super) struct Images {
    files: BTreeMap<(u64, u64), u64>,
    pub incomplete: bool,
    pub path_present: bool,
}

impl Images {
    pub fn has_image(&self) -> bool {
        !self.files.is_empty()
    }

    pub fn bytes(&self) -> Option<u64> {
        self.has_image().then(|| {
            self.files
                .values()
                .fold(0u64, |sum, size| sum.saturating_add(*size))
        })
    }

    fn record(&mut self, metadata: &fs::Metadata) {
        self.files.insert(
            (metadata.dev(), metadata.ino()),
            metadata.blocks().saturating_mul(512),
        );
    }

    fn read_images_in(&mut self, path: &Path) {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return,
            Err(_) => {
                self.incomplete = true;
                return;
            }
        };
        if !metadata.is_dir() || inventory::is_link(&metadata) {
            self.incomplete = true;
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            self.incomplete = true;
            return;
        };
        for entry in entries {
            match entry {
                Ok(entry) if is_image(&entry.path()) => match entry.path().symlink_metadata() {
                    Ok(meta) if meta.is_file() && !inventory::is_link(&meta) => self.record(&meta),
                    _ => self.incomplete = true,
                },
                Ok(_) => {}
                Err(_) => self.incomplete = true,
            }
        }
    }

    fn read_known_path(&mut self, path: &Path) {
        // Check the parent before reading metadata through it.
        let Some(parent) = path.parent() else {
            self.incomplete = true;
            return;
        };
        if let Err(error) = inventory::safe_dir(parent) {
            self.incomplete |= !missing(&error);
            return;
        }
        match path.symlink_metadata() {
            Ok(meta) if meta.is_file() && !inventory::is_link(&meta) && is_image(path) => {
                self.path_present = true;
                self.record(&meta);
            }
            Ok(meta) if meta.is_dir() && !inventory::is_link(&meta) => {
                self.path_present = true;
                self.read_images_in(path);
                self.read_images_in(&path.join("Restore"));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            _ => self.incomplete = true,
        }
    }

    fn read_assets(&mut self, root: &Path, build: &str) {
        if let Err(error) = inventory::safe_dir(root) {
            self.incomplete |= !missing(&error);
            return;
        }
        let Ok(entries) = fs::read_dir(root) else {
            self.incomplete = true;
            return;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                self.incomplete = true;
                continue;
            };
            let asset = entry.path();
            if asset.extension().is_none_or(|ext| ext != "asset") {
                continue;
            }
            let plist = asset.join("Info.plist");
            if !entry.file_type().is_ok_and(|ty| ty.is_dir())
                || !plist
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.len() <= 1024 * 1024)
            {
                self.incomplete = true;
                continue;
            }
            let asset_build = app_storage::command_output(
                Command::new("/usr/bin/plutil")
                    .args([
                        "-extract",
                        "MobileAssetProperties.Build",
                        "raw",
                        "-o",
                        "-",
                        "--",
                    ])
                    .arg(plist),
                Duration::from_secs(2),
            );
            match asset_build {
                Ok(value) if value.trim() == build => {
                    self.read_known_path(&asset.join("AssetData"))
                }
                Ok(_) => {}
                Err(_) => self.incomplete = true,
            }
        }
    }
}

pub(super) fn probe(platform: &str, build: &str, installed_path: Option<&Path>) -> Images {
    let mut images = Images::default();
    let Some(root) = asset_root(platform) else {
        images.incomplete = true;
        return images;
    };
    images.read_assets(&root, build);
    if let Some(path) = installed_path {
        if (path.starts_with(&root) || path.starts_with(CORE_IMAGES))
            && !path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            images.read_known_path(path);
        } else {
            images.incomplete = true;
        }
    }
    images
}

fn asset_root(platform: &str) -> Option<PathBuf> {
    let name = match platform {
        "iOS" => "iOS",
        "watchOS" => "watchOS",
        "tvOS" => "tvOS",
        "visionOS" => "xrOS",
        _ => return None,
    };
    Some(Path::new(ASSETS).join(format!("com_apple_MobileAsset_{name}SimulatorRuntime")))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "dmg" || ext == "img")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::sdk_cleanup::{classify_removal, Removal};
    use anyhow::Result;

    #[test]
    fn installed_image_gone_but_matching_download_still_counts_as_partial() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let mut target = None;
        for build in ["23S303", "23S999"] {
            let asset = root.join(format!("{build}.asset"));
            let restore = asset.join("AssetData/Restore");
            fs::create_dir_all(&restore)?;
            fs::write(
                asset.join("Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>MobileAssetProperties</key><dict><key>Build</key><string>{build}</string></dict></dict></plist>"#
                ),
            )?;
            let file = restore.join("runtime.dmg");
            fs::write(&file, vec![1u8; 16384])?;
            if build == "23S303" {
                target = Some(file);
            }
        }
        let target = target.unwrap();
        let mut images = Images::default();
        images.read_assets(&root, "23S303");
        images.read_known_path(&target);
        let bytes = target.metadata()?.blocks() * 512;
        assert_eq!(
            classify_removal(Ok(false), images),
            Removal::Partial {
                image_bytes: Some(bytes)
            }
        );
        assert_eq!(
            classify_removal(Ok(true), Images::default()),
            Removal::Partial { image_bytes: None }
        );
        assert_eq!(
            classify_removal(
                Ok(false),
                Images {
                    incomplete: true,
                    ..Images::default()
                }
            ),
            Removal::Unverified
        );
        assert_eq!(
            classify_removal(Ok(false), Images::default()),
            Removal::Removed
        );
        Ok(())
    }
}
