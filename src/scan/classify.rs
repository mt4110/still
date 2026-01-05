use crate::plan::EntryType;
use std::fs::Metadata;
use std::path::Path;

pub fn determine_entry_type(path: &Path, metadata: &Metadata) -> EntryType {
    if metadata.is_symlink() {
        return EntryType::Symlink;
    }
    if metadata.is_dir() {
        // Simple heuristic for bundle: ends with .app, .framework, .bundle
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy();
            if ext_str == "app"
                || ext_str == "framework"
                || ext_str == "bundle"
                || ext_str == "plugin"
            {
                return EntryType::Bundle;
            }
        }
        return EntryType::Directory;
    }
    EntryType::File
}
