//! Keep persistent data discoverable across releases, independently of the app's signing ID.
pub(crate) fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("com", "antigravity", "still")
}
