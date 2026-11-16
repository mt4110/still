use super::{filesystem, git, inventory};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Producer {
    Cargo,
    NextJs,
    Vite,
    DeclaredOutput,
}

impl Producer {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Cargo => "Cargo manifest and build metadata",
            Self::NextJs => "Next.js dependency and build output",
            Self::Vite => "Vite dependency and bundled assets",
            Self::DeclaredOutput => "Dedicated output declared with --output",
        }
    }
}

pub struct Policy {
    pub roots: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    pub docker_context: Option<String>,
}

impl Policy {
    fn permits(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| path != root && path.starts_with(root))
            && !path.ancestors().any(protected)
    }

    pub fn producer(&self, path: &Path) -> Option<Producer> {
        if !self.permits(path) {
            return None;
        }
        if self.outputs.iter().any(|output| path == output) {
            return Some(Producer::DeclaredOutput);
        }
        let parent = path.parent()?;
        match path.file_name()?.to_str()? {
            "target"
                if regular_file(&parent.join("Cargo.toml"))
                    && (regular_file(&path.join(".rustc_info.json"))
                        || regular_file(&path.join("CACHEDIR.TAG")))
                    && (directory(&path.join("debug"))
                        || directory(&path.join("release"))
                        || directory(&path.join("doc"))) =>
            {
                Some(Producer::Cargo)
            }
            ".next"
                if has_dependency(parent, "next")
                    && (regular_file(&path.join("BUILD_ID"))
                        || (directory(&path.join("server")) && directory(&path.join("cache")))) =>
            {
                Some(Producer::NextJs)
            }
            "dist"
                if has_dependency(parent, "vite")
                    && regular_file(&path.join("index.html"))
                    && directory(&path.join("assets")) =>
            {
                Some(Producer::Vite)
            }
            _ => None,
        }
    }

    pub fn prepare(&self, path: &Path) -> Result<Prepared> {
        let path = inventory::safe_dir(path)?;
        let producer = self
            .producer(&path)
            .context("No verified output producer")?;
        if std::env::current_dir()?.starts_with(&path) {
            bail!("The current working directory is inside this output");
        }
        git::protect_tracked(&path, producer == Producer::DeclaredOutput)?;
        let tree = stamp_tree(&path)?;
        Ok(Prepared {
            path,
            producer,
            tree,
        })
    }

    pub fn remove(&self, approved: &Prepared) -> Result<()> {
        let current = self.prepare(&approved.path)?;
        if current.producer != approved.producer || current.tree != approved.tree {
            bail!("The output changed after review. Refresh and review it again.");
        }
        if let Some(context) = &self.docker_context {
            super::docker::protect_bind_mounts(context, &current.path)?;
        }
        filesystem::ensure_not_open(&current.path)?;
        filesystem::remove_output(&current.path, &current.tree)
    }
}

pub struct Prepared {
    pub path: PathBuf,
    pub producer: Producer,
    tree: Tree,
}

impl Prepared {
    pub fn files(&self) -> usize {
        self.tree
            .entries
            .values()
            .filter(|stamp| !stamp.is_dir)
            .count()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Tree {
    pub parent: filesystem::Identity,
    pub entries: BTreeMap<PathBuf, Stamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub identity: filesystem::Identity,
    pub is_dir: bool,
    pub len: u64,
    pub modified: SystemTime,
    pub changed: (i64, i64),
}

pub fn stamp_tree(path: &Path) -> Result<Tree> {
    let parent = path.parent().context("Cannot stage a filesystem root")?;
    let parent_identity = filesystem::identity(parent, &fs::symlink_metadata(parent)?)?;
    let root_metadata = fs::symlink_metadata(path)?;
    let mut entries = BTreeMap::new();
    let mut remaining = vec![path.to_owned()];
    while let Some(entry) = remaining.pop() {
        let relative = entry.strip_prefix(path)?;
        let metadata = fs::symlink_metadata(&entry)?;
        if inventory::is_link(&metadata)
            || !inventory::same_device(&root_metadata, &metadata)
            || (!metadata.is_file() && !metadata.is_dir())
            || (!relative.as_os_str().is_empty() && protected(&entry))
        {
            bail!(
                "Protected or unsupported entry: {}",
                inventory::display(&entry)
            );
        }
        let identity = filesystem::identity(&entry, &metadata)?;
        if identity.device != parent_identity.device {
            bail!("Output crosses a filesystem boundary: {entry:?}");
        }
        if metadata.is_dir() {
            for child in fs::read_dir(&entry)? {
                remaining.push(child?.path());
            }
        }
        entries.insert(
            relative.to_owned(),
            Stamp {
                identity,
                is_dir: metadata.is_dir(),
                len: metadata.len(),
                modified: metadata.modified()?,
                changed: filesystem::changed(&metadata),
            },
        );
    }
    Ok(Tree {
        parent: parent_identity,
        entries,
    })
}

pub fn verify_staged(tree: &Tree, payload: &Path) -> Result<()> {
    let mut expected_entries = tree.entries.clone();
    let mut staged = stamp_tree(payload)?.entries;
    // Same-volume rename can change the directory's ctime, while child stamps must agree.
    let root = Path::new("");
    if let (Some(expected), Some(actual)) = (expected_entries.get_mut(root), staged.get_mut(root)) {
        expected.changed = (0, 0);
        actual.changed = (0, 0);
    }
    if expected_entries != staged {
        bail!("The staged output differs from what was reviewed");
    }
    Ok(())
}

pub fn protected(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    matches!(
        name.as_str(),
        ".git"
            | ".env"
            | ".envrc"
            | ".ssh"
            | ".still-active"
            | ".keep"
            | ".gitkeep"
            | "id_rsa"
            | "id_ed25519"
            | ".docker"
            | ".colima"
            | "docker.raw"
            | "docker.qcow2"
    ) || name.starts_with(".env.")
        || name.starts_with(".still-stage-")
        || matches!(
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("pem" | "key" | "p12" | "sqlite" | "sqlite3" | "db" | "dump")
        )
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !inventory::is_link(&m))
}

fn directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !inventory::is_link(&m))
}

fn has_dependency(project: &Path, package: &str) -> bool {
    let manifest = project.join("package.json");
    if !regular_file(&manifest) {
        return false;
    }
    let mut bytes = Vec::new();
    if fs::File::open(manifest)
        .and_then(|file| file.take(1024 * 1024).read_to_end(&mut bytes))
        .is_err()
    {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    ["dependencies", "devDependencies"]
        .iter()
        .any(|field| value[field][package].is_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Policy) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let output = root.join("target");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("build.bin"), b"generated").unwrap();
        let policy = Policy {
            roots: vec![root],
            outputs: vec![output.clone()],
            docker_context: None,
        };
        (temporary, output, policy)
    }

    #[test]
    fn mutation_after_selection_invalidates_approval() {
        let (_temporary, output, policy) = fixture();
        let approved = policy.prepare(&output).unwrap();
        fs::write(output.join("build.bin"), b"new bytes").unwrap();
        assert!(policy.remove(&approved).is_err());
        assert_eq!(fs::read(output.join("build.bin")).unwrap(), b"new bytes");
    }

    #[test]
    fn protects_credentials_and_nested_repos() {
        let (_temporary, output, policy) = fixture();
        fs::create_dir(output.join(".git")).unwrap();
        assert!(policy.prepare(&output).is_err());
        let (_temporary, output, policy) = fixture();
        fs::write(output.join(".env"), "").unwrap();
        assert!(policy.prepare(&output).is_err());
    }

    #[test]
    fn preserves_tracked_files_when_output_spelling_changes_case() {
        let (_temporary, output, policy) = fixture();
        let repo = output.parent().unwrap();
        let original = repo.join("TARGET-original");
        fs::rename(&output, &original).unwrap();
        let uppercase = repo.join("TARGET");
        fs::rename(&original, &uppercase).unwrap();
        git::check(git::run(repo, ["init", "-q"]).unwrap(), "init").unwrap();
        git::check(
            git::run(repo, ["add", "--force", "--", "TARGET/build.bin"]).unwrap(),
            "add",
        )
        .unwrap();
        fs::rename(&uppercase, &original).unwrap();
        fs::rename(&original, &output).unwrap();

        let error = policy.prepare(&output).err().expect("tracked output");
        assert!(error.to_string().contains("tracked by Git"));
        assert!(output.join("build.bin").is_file());
    }

    #[test]
    fn only_removes_the_confirmed_output() {
        let (_temporary, output, policy) = fixture();
        let keep = output.with_file_name("keep");
        fs::write(&keep, "keep").unwrap();
        #[cfg(unix)]
        fs::hard_link(output.join("build.bin"), output.join("linked.bin")).unwrap();
        let approved = policy.prepare(&output).unwrap();
        policy.remove(&approved).unwrap();
        assert!(!output.exists());
        assert_eq!(fs::read_to_string(&keep).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_cannot_redirect_removal() {
        use std::os::unix::fs::symlink;
        let (_temporary, output, mut policy) = fixture();
        let nested = output.join("dist");
        fs::create_dir(&nested).unwrap();
        policy.outputs.push(nested.clone());
        let approved = policy.prepare(&nested).unwrap();
        let moved = output.with_file_name("moved");
        fs::rename(&output, &moved).unwrap();
        symlink(&moved, &output).unwrap();
        assert!(policy.remove(&approved).is_err());
        assert!(moved.join("dist").is_dir());
    }
}
