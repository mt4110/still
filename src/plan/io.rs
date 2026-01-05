use crate::error::AppError;
use crate::plan::Plan;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

pub fn save_plan(plan: &Plan, path: &Path) -> Result<(), AppError> {
    let file = File::create(path).map_err(AppError::Io)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, plan).map_err(|e| AppError::Plan(e.to_string()))?;
    Ok(())
}

pub fn load_plan(path: &Path) -> Result<Plan, AppError> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Plan file not found at {:?}. Run 'still scan' first.", path),
            ))
        } else {
            AppError::Io(e)
        }
    })?;

    if file.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(AppError::Plan(
            "Plan file is empty. Please run 'still scan' first.".to_string(),
        ));
    }

    let reader = BufReader::new(file);
    let plan: Plan = serde_json::from_reader(reader).map_err(|e| AppError::Plan(e.to_string()))?;
    Ok(plan)
}
