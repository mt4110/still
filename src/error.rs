use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    #[allow(dead_code)]
    Config(String),

    #[error("Rule error: {0}")]
    #[allow(dead_code)]
    Rule(String),

    #[error("Scan error: {0}")]
    Scan(String),

    #[error("Plan error: {0}")]
    Plan(String),

    #[error("Action aborted by user")]
    #[allow(dead_code)]
    Aborted,

    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}
