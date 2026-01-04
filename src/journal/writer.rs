use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use std::sync::Mutex;
use uuid;

lazy_static::lazy_static! {
    static ref JOURNAL_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
    static ref RUN_ID: String = uuid::Uuid::new_v4().to_string();
}

pub fn init() -> Result<()> {
    let dirs = directories::ProjectDirs::from("com", "antigravity", "still")
        .or_else(|| directories::ProjectDirs::from("com", "user", "still"))
        .ok_or(anyhow::anyhow!("Could not determine data directory"))?;
    let path = dirs.data_local_dir().join("journal.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    
    let mut lock = JOURNAL_PATH.lock().unwrap();
    *lock = Some(path);
    // Force RUN_ID initialization or just let it happen on first log
    Ok(())
}

#[derive(Serialize)]
struct LogEntry<T> {
    run_id: String,
    timestamp: String,
    action: String,
    data: T,
}

pub fn log<T: Serialize>(action: &str, data: T) {
    let path_opt = JOURNAL_PATH.lock().unwrap();
    if let Some(path) = &*path_opt {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut file) => {
                let entry = LogEntry {
                    run_id: RUN_ID.clone(),
                    timestamp: Utc::now().to_rfc3339(),
                    action: action.to_string(),
                    data,
                };
                if let Ok(json) = serde_json::to_string(&entry) {
                    let _ = writeln!(file, "{}", json);
                }
            }
            Err(e) => eprintln!("Failed to write to journal: {}", e),
        }
    } else {
        // Journal not initialized?
        eprintln!("Journal not initialized, dropping log: {} {:?}", action, std::any::type_name::<T>());
    }
}
