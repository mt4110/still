use serde::{Serialize, Serializer};
use std::fmt;

/// An actionable review message with a stable English representation for CLI output.
#[derive(Clone, Debug)]
pub(super) struct Message {
    english: String,
    japanese: String,
}

impl Message {
    pub fn new(english: impl Into<String>, japanese: impl Into<String>) -> Self {
        Self {
            english: english.into(),
            japanese: japanese.into(),
        }
    }

    pub fn from_error(error: &anyhow::Error) -> Self {
        let english = format!("{error:#}");
        let japanese = match error.downcast_ref::<Self>() {
            Some(reason) => reason.japanese.clone(),
            None => format!("安全に削除できるか確認できませんでした。詳細: {english}"),
        };
        Self { english, japanese }
    }

    pub fn with_prefix(&self, prefix: &str) -> Self {
        Self::new(
            format!("{prefix} · {}", self.english),
            format!("{prefix} · {}", self.japanese),
        )
    }

    #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
    pub fn text(&self, japanese: bool) -> &str {
        if japanese {
            &self.japanese
        } else {
            &self.english
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.english)
    }
}

impl std::error::Error for Message {}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.english)
    }
}
