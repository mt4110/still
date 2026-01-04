use serde::{Serialize, Deserialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScopeSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub safety: String, // "SAFE" or "UNSAFE"
    pub default: bool,
    pub description: String,
    pub roots: Vec<String>,
    pub apply_compatible_by_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub fn all_scopes() -> Vec<ScopeSpec> {
    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~"));

    let library_safe_roots: Vec<String> = crate::policy::DEFAULT_ALLOWED_SCOPES_SUBDIRS
        .iter()
        .map(|sub| home.join("Library").join(sub).to_string_lossy().to_string())
        .collect();

    vec![
        ScopeSpec {
            name: "library-safe".to_string(),
            aliases: vec!["library".to_string()],
            safety: "SAFE".to_string(),
            default: true,
            description: "Safe subset of ~/Library suitable for weekly use.".to_string(),
            roots: library_safe_roots,
            apply_compatible_by_default: true,
            warning: None,
        },
        ScopeSpec {
            name: "library-unsafe".to_string(),
            aliases: vec![],
            safety: "UNSAFE".to_string(),
            default: false,
            description: "Scans entire ~/Library (advanced).".to_string(),
            roots: vec![home.join("Library").to_string_lossy().to_string()],
            apply_compatible_by_default: false,
            warning: Some("Compare reasons in TUI before applying. This scope scans the full ~/Library and may surface sensitive paths.".to_string()),
        },
    ]
}
