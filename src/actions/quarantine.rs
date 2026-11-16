use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub struct Vault {
    root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VaultMeta {
    pub original_path: PathBuf,
    pub quarantined_at: DateTime<Utc>,
    pub original_name: String,
    pub batch_id: String,
    /// The name of the stored file/dir inside the vault directory.
    pub stored_name: String,
    /// Expiration time (default: +30 days)
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Vault {
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("com", "antigravity", "still")
            .or_else(|| directories::ProjectDirs::from("com", "user", "still"))
            .ok_or(anyhow::anyhow!("Could not determine data directory"))?;

        let root = dirs.data_local_dir().join("vault");
        Self::new_with_root(root)
    }

    pub fn new_with_root(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn quarantine(&self, path: &Path) -> Result<PathBuf> {
        if !path.exists() {
            return Err(anyhow::anyhow!("File not found: {:?}", path));
        }

        let uuid = Uuid::new_v4();
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();

        // Simpler for MVP: vault/uuid/filename
        let file_name = path.file_name().unwrap_or_default();
        let vault_dir = self.root.join(uuid.to_string());
        fs::create_dir_all(&vault_dir)?;

        let target_path = vault_dir.join(file_name);

        // Move (rename) with EXDEV fallback (cross-device)
        move_path(path, &target_path)
            .with_context(|| format!("Failed to move {:?} to {:?}", path, target_path))?;

        let meta = VaultMeta {
            original_path: path.to_path_buf(),
            quarantined_at: Utc::now(),
            original_name: file_name.to_string_lossy().to_string(),
            batch_id: timestamp,
            stored_name: file_name.to_string_lossy().to_string(),
            // Default TTL from policy
            expires_at: Some(
                Utc::now() + chrono::Duration::days(crate::policy::DEFAULT_VAULT_TTL_DAYS),
            ),
        };

        let meta_path = vault_dir.join("still_meta.json");
        let f = fs::File::create(meta_path)?;
        serde_json::to_writer_pretty(f, &meta)?;

        Ok(target_path)
    }

    pub fn list(&self) -> Result<Vec<(String, VaultMeta)>> {
        let mut out = Vec::new();
        // Check if root exists (it might not if no items yet)
        if !self.root.exists() {
            return Ok(out);
        }

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let meta_path = path.join("still_meta.json");
            if !meta_path.exists() {
                continue;
            }
            let f = fs::File::open(meta_path)?;
            let meta: VaultMeta = serde_json::from_reader(f)
                .with_context(|| format!("Failed to parse meta for vault item: {}", id))?;
            out.push((id, meta));
        }
        out.sort_by_key(|item| std::cmp::Reverse(item.1.quarantined_at));
        Ok(out)
    }

    pub fn restore(&self, id: &str) -> Result<PathBuf> {
        let vault_dir = self.root.join(id);
        let meta_path = vault_dir.join("still_meta.json");
        if !meta_path.exists() {
            return Err(anyhow::anyhow!(
                "Vault item not found or missing meta: {id}"
            ));
        }
        let f = fs::File::open(&meta_path)?;
        let meta: VaultMeta = serde_json::from_reader(f)?;

        let stored = vault_dir.join(&meta.stored_name);
        if !stored.exists() {
            return Err(anyhow::anyhow!(
                "Stored content missing for vault item: {id}"
            ));
        }

        let dest = meta.original_path;
        if dest.exists() {
            return Err(anyhow::anyhow!(
                "Refusing to overwrite existing path: {:?} (move it away, then restore)",
                dest
            ));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        // Move back
        move_path(&stored, &dest)
            .with_context(|| format!("Failed to restore {:?} -> {:?}", stored, dest))?;

        // Remove empty vault dir if possible
        let _ = fs::remove_file(&meta_path);
        let _ = fs::remove_dir(&vault_dir);

        Ok(dest)
    }

    pub fn purge(&self, id: &str) -> Result<()> {
        let vault_dir = self.root.join(id);
        if !vault_dir.exists() {
            return Err(anyhow::anyhow!("Vault item not found: {id}"));
        }
        fs::remove_dir_all(&vault_dir)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn purge_expired(&self) -> Result<Vec<String>> {
        let mut purged = Vec::new();
        let now = Utc::now();

        // Iterate all items, check expiry
        for (id, meta) in self.list()? {
            if let Some(expires) = meta.expires_at {
                if now > expires {
                    self.purge(&id)?;
                    purged.push(id);
                }
            }
        }
        Ok(purged)
    }
}

fn move_path(src: &Path, dest: &Path) -> Result<()> {
    match fs::rename(src, dest) {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            // EXDEV: copy then remove
            let md = fs::symlink_metadata(src)?;
            if md.is_symlink() {
                copy_symlink(src, dest)?;
                fs::remove_file(src)?;
            } else if md.is_dir() {
                copy_dir_recursive(src, dest)?;
                fs::remove_dir_all(src)?;
            } else {
                fs::copy(src, dest)?;
                if let Ok(f) = fs::File::open(dest) {
                    let _ = f.sync_all();
                }
                fs::remove_file(src)?;
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            copy_symlink(&path, &target)?;
        } else if ft.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
            if let Ok(f) = fs::File::open(&target) {
                let _ = f.sync_all();
            }
        }
    }
    // Sync the directory itself to ensure entries are persisted?
    if let Ok(d) = fs::File::open(dest) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target = fs::read_link(src)?;
    std::os::unix::fs::symlink(target, dest)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(_src: &Path, _dest: &Path) -> Result<()> {
    anyhow::bail!("Symlink copy not supported on non-unix")
}
