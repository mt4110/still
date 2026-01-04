use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

pub mod io;
pub mod model;
pub mod validator;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Plan {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_generated_by")]
    pub generated_by: String,
    pub created_at: SystemTime,
    pub entries: Vec<Entry>,
}

fn default_schema_version() -> String { "2.0".to_string() }
fn default_generated_by() -> String { "still".to_string() }

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
    pub entry_type: EntryType,
    #[serde(default)]
    pub origin: PlanOrigin,
    pub recommended_action: ActionType,
    pub reason: String,
    // Chosen action by user (mutable in TUI)
    pub selected_action: ActionType,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub enum PlanOrigin {
    #[default]
    Scan,
    Vault,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum EntryType {
    File,
    Directory,
    Symlink,
    Bundle, // .app, .framework, etc.
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionType {
    Keep,
    Quarantine,
    Trash,
    Purge,
}
