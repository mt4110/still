use anyhow::{Result, Context};
use std::path::Path;

pub fn trash_path(path: &Path) -> Result<()> {
    trash::delete(path).with_context(|| format!("Failed to move to trash: {:?}", path))?;
    Ok(())
}
