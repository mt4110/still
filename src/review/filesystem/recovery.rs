//! Explicit recovery for an old review stage. A stage name alone is never
//! authority to recursively delete its payload.
use super::{finder, stat_identity, Identity};
use crate::review::{artifacts, inventory};
use anyhow::{ensure, Context, Result};
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, Mode, Stat};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory(Identity, BTreeMap<OsString, Entry>),
    File(Identity, i64, i64, i64),
}

impl Entry {
    fn file(stat: &Stat) -> Self {
        Self::File(
            stat_identity(stat),
            stat.st_ctime,
            stat.st_ctime_nsec,
            stat.st_size,
        )
    }
}

struct Stage {
    parent: OwnedFd,
    name: OsString,
    dir: OwnedFd,
}

impl Stage {
    fn open(cleanup_root: &Path, path: &Path) -> Result<Self> {
        let root = inventory::safe_dir(cleanup_root)?;
        let path = inventory::safe_dir(path)?;
        let parent_path = path.parent().context("No stage parent")?;
        ensure!(
            parent_path.starts_with(&root) && !parent_path.ancestors().any(artifacts::protected),
            "Stage must be inside the selected cleanup root and outside protected folders"
        );
        let name = path.file_name().context("Missing stage name")?;
        let valid_name = name
            .to_str()
            .and_then(|n| n.strip_prefix(".still-stage-"))
            .is_some_and(|n| {
                uuid::Uuid::parse_str(n)
                    .is_ok_and(|uuid| uuid.get_version_num() == 4 && uuid.to_string() == n)
            });
        ensure!(
            valid_name,
            "Expected a still stage with its original UUID name"
        );
        let mut parent = fs::open("/", finder::DIRECTORY, Mode::empty())?;
        for part in parent_path.components() {
            if let std::path::Component::Normal(name) = part {
                parent = fs::openat(&parent, name, finder::DIRECTORY, Mode::empty())?;
            }
        }
        let dir = fs::openat(&parent, name, finder::DIRECTORY, Mode::empty())?;
        let stat = fs::fstat(&dir)?;
        // SAFETY: geteuid takes no arguments and always returns the effective uid.
        ensure!(
            stat.st_uid == unsafe { libc::geteuid() }
                && stat.st_mode & 0o077 == 0
                && stat.st_dev == fs::fstat(&parent)?.st_dev,
            "Expected a private stage owned by this user"
        );
        Ok(Self {
            parent,
            name: name.to_owned(),
            dir,
        })
    }

    fn review(&self, stage_path: &Path) -> Result<BTreeMap<OsString, Entry>> {
        let mut tree = BTreeMap::new();
        let mut count = 0;
        for name in finder::children(&self.dir)? {
            let item = if name == "original-path.txt" {
                let mut note = finder::open_regular(&self.dir, &name, 16 * 1024)?;
                let mut value = String::new();
                (&mut note).take(16 * 1024 + 1).read_to_string(&mut value)?;
                let original = Path::new(
                    value
                        .strip_suffix('\n')
                        .context("Unrecognized stage note")?,
                );
                ensure!(
                    original.parent() == stage_path.parent()
                        && original.file_name().is_some()
                        && !artifacts::protected(original)
                        && inventory::display(original) == original.to_string_lossy(),
                    "Original path in stage note does not name a sibling"
                );
                Entry::file(&fs::fstat(note)?)
            } else if name == "payload" {
                inspect_directory(&self.dir, &name, &mut count, 0)?
            } else {
                Entry::file(&finder::inspect(&self.dir, &name)?)
            };
            tree.insert(name, item);
        }
        ensure!(
            !tree.contains_key(OsStr::new("payload"))
                || tree.contains_key(OsStr::new("original-path.txt")),
            "Original path note missing from payload stage"
        );
        Ok(tree)
    }

    fn remove(&self, mut review: BTreeMap<OsString, Entry>) -> Result<()> {
        let note = review.remove(OsStr::new("original-path.txt"));
        for (name, item) in review {
            remove_entry(&self.dir, &name, item, false)?;
        }
        if let Some(entry) = note {
            remove_entry(&self.dir, OsStr::new("original-path.txt"), entry, true)?;
        }
        finder::finish(&self.parent, &self.name, &self.dir)
    }
}

/// The stage record is user-controlled evidence, not proof of provenance or completeness.
pub(crate) struct RestoreStage {
    root: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) original: PathBuf,
    stage_id: Identity,
    payload_id: Identity,
    note: Entry,
}

impl RestoreStage {
    pub(crate) fn inspect(root: &Path, path: &Path) -> Result<Self> {
        let path = inventory::safe_dir(path)?;
        let stage = Stage::open(root, &path)?;
        let mut note =
            finder::open_regular(&stage.dir, OsStr::new("original-path.txt"), 16 * 1024)?;
        let mut value = String::new();
        (&mut note).take(16 * 1024 + 1).read_to_string(&mut value)?;
        let original = PathBuf::from(
            value
                .strip_suffix('\n')
                .context("Unrecognized stage note")?,
        );
        ensure!(
            original.parent() == path.parent()
                && original.file_name().is_some()
                && !artifacts::protected(&original)
                && inventory::display(&original) == original.to_string_lossy(),
            "Original path in stage note does not name an unprotected sibling"
        );
        let payload = fs::openat(&stage.dir, "payload", finder::DIRECTORY, Mode::empty())?;
        let payload_stat = fs::fstat(&payload)?;
        ensure!(
            payload_stat.st_dev == fs::fstat(&stage.dir)?.st_dev,
            "Payload crosses a filesystem boundary"
        );
        Ok(Self {
            root: inventory::safe_dir(root)?,
            path,
            original,
            stage_id: stat_identity(&fs::fstat(&stage.dir)?),
            payload_id: stat_identity(&payload_stat),
            note: Entry::file(&fs::fstat(note)?),
        })
    }

    pub(crate) fn restore(self) -> Result<()> {
        super::ensure_not_open(&self.path)?;
        let current = Self::inspect(&self.root, &self.path)?;
        ensure!(
            current.stage_id == self.stage_id
                && current.payload_id == self.payload_id
                && current.note == self.note
                && current.original == self.original,
            "Stage or recorded destination changed during review"
        );
        let stage = Stage::open(&self.root, &self.path)?;
        ensure!(
            stat_identity(&fs::fstat(&stage.dir)?) == self.stage_id,
            "Stage changed during review"
        );
        let payload = fs::openat(&stage.dir, "payload", finder::DIRECTORY, Mode::empty())?;
        ensure!(
            stat_identity(&fs::fstat(&payload)?) == self.payload_id,
            "Payload changed during review"
        );
        let payload_path_stat = fs::statat(&stage.dir, "payload", AtFlags::SYMLINK_NOFOLLOW)?;
        ensure!(
            FileType::from_raw_mode(payload_path_stat.st_mode) == FileType::Directory
                && stat_identity(&payload_path_stat) == self.payload_id,
            "Payload path changed during review"
        );
        fs::renameat_with(
            &stage.dir,
            "payload",
            &stage.parent,
            self.original.file_name().context("Missing destination")?,
            fs::RenameFlags::NOREPLACE,
        )
        .context("Could not restore payload without overwriting the destination")?;
        Ok(())
    }
}

fn inspect_directory(
    parent: &OwnedFd,
    name: &OsStr,
    count: &mut usize,
    depth: usize,
) -> Result<Entry> {
    ensure!(depth < 64, "Stage too deep for metadata recovery");
    let dir = fs::openat(parent, name, finder::DIRECTORY, Mode::empty())?;
    let stat = fs::fstat(&dir)?;
    ensure!(
        stat.st_dev == fs::fstat(parent)?.st_dev,
        "Stage crosses a filesystem boundary"
    );
    let mut items = BTreeMap::new();
    for child in finder::children(&dir)? {
        *count += 1;
        ensure!(*count <= 16_384, "Stage too large for metadata recovery");
        let info = fs::statat(&dir, &child, AtFlags::SYMLINK_NOFOLLOW)?;
        let item = if FileType::from_raw_mode(info.st_mode) == FileType::Directory {
            ensure!(
                !artifacts::protected(Path::new(&child)),
                "Protected folder in stage: {child:?}"
            );
            inspect_directory(&dir, &child, count, depth + 1)?
        } else {
            Entry::file(
                &finder::inspect(&dir, &child)
                    .with_context(|| format!("Staged data still present: {child:?}"))?,
            )
        };
        items.insert(child, item);
    }
    Ok(Entry::Directory(stat_identity(&stat), items))
}

fn remove_entry(parent: &OwnedFd, name: &OsStr, entry: Entry, note: bool) -> Result<()> {
    match entry {
        Entry::Directory(identity, items) => {
            let dir = fs::openat(parent, name, finder::DIRECTORY, Mode::empty())?;
            ensure!(
                stat_identity(&fs::fstat(&dir)?) == identity,
                "Directory changed during recovery"
            );
            for (child, entry) in items {
                remove_entry(&dir, &child, entry, false)?;
            }
            finder::finish(parent, name, &dir)
        }
        expected => {
            let current = if note {
                fs::fstat(finder::open_regular(parent, name, 16 * 1024)?)?
            } else {
                finder::inspect(parent, name)?
            };
            ensure!(
                Entry::file(&current) == expected,
                "Stage changed during recovery"
            );
            finder::unlink_checked(parent, name, &current)
        }
    }
}

/// Only the evidence crosses the UI confirmation boundary. Holding a directory
/// handle open across the lsof check would block recovery of our own stage.
pub(crate) struct ReviewedStage {
    cleanup_root: std::path::PathBuf,
    pub(crate) path: std::path::PathBuf,
    id: Identity,
    tree: BTreeMap<OsString, Entry>,
}

impl ReviewedStage {
    pub(crate) fn inspect(root: &Path, path: &Path) -> Result<Self> {
        let path = inventory::safe_dir(path)?;
        let stage = Stage::open(root, &path)?;
        Ok(Self {
            cleanup_root: inventory::safe_dir(root)?,
            tree: stage.review(&path)?,
            id: stat_identity(&fs::fstat(&stage.dir)?),
            path,
        })
    }

    pub(crate) fn remove(self) -> Result<()> {
        super::ensure_not_open(&self.path)?;
        let stage = Stage::open(&self.cleanup_root, &self.path)?;
        ensure!(
            stat_identity(&fs::fstat(&stage.dir)?) == self.id
                && stage.review(&self.path)? == self.tree,
            "Stage changed during review"
        );
        stage.remove(self.tree)
    }
}

pub(crate) fn run(args: &crate::cli::StageCleanupArgs) -> Result<()> {
    inventory::safe_workspace_dir(&args.cleanup_root)?;
    run_review(args)
}

fn run_review(args: &crate::cli::StageCleanupArgs) -> Result<()> {
    if args.restore {
        let review = RestoreStage::inspect(&args.cleanup_root, &args.stage)?;
        println!("Stage record is not proof of origin or completeness. Moving remaining payload from {} to {}.", inventory::display(&review.path), inventory::display(&review.original));
        review.restore()?;
        println!("Restored remaining payload; the stage and its note remain for review.");
        return Ok(());
    }
    let review = match ReviewedStage::inspect(&args.cleanup_root, &args.stage) {
        Ok(review) => review,
        Err(metadata_error) if !args.apply => {
            let restore = RestoreStage::inspect(&args.cleanup_root, &args.stage)
                .context(format!("Metadata cleanup unavailable: {metadata_error:#}"))?;
            println!("Remaining payload: {}\nRecorded destination: {}\nThe stage record does not prove origin or completeness. Verify both paths and contents before running with --restore; an existing destination will not be overwritten.", inventory::display(&restore.path.join("payload")), inventory::display(&restore.original));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let label = inventory::display(&review.path);
    if args.apply {
        review.remove()?;
        println!("Removed metadata-only stage: {label}");
    } else {
        println!("Metadata-only stage: {label}\nRun this command with --apply to remove it.");
        if let Ok(restore) = RestoreStage::inspect(&args.cleanup_root, &args.stage) {
            println!("Alternatively, inspect {} and recorded destination {} before using --restore. The record does not prove origin or completeness; an existing destination will not be overwritten.", inventory::display(&restore.path.join("payload")), inventory::display(&restore.original));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::StageCleanupArgs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::{fs, path::PathBuf};
    use tempfile::TempDir;

    fn fixture() -> Result<(TempDir, PathBuf, StageCleanupArgs)> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let stage = root.join(format!(".still-stage-{}", uuid::Uuid::new_v4()));
        let payload = stage.join("payload");
        fs::create_dir_all(payload.join("debug/deps"))?;
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
        fs::write(
            stage.join("original-path.txt"),
            format!("{}\n", root.join("target").display()),
        )?;
        for dir in [
            &payload,
            &payload.join("debug"),
            &payload.join("debug/deps"),
        ] {
            fs::write(dir.join(".DS_Store"), b"\0\0\0\x01Bud1metadata")?;
        }
        Ok((
            temp,
            payload,
            StageCleanupArgs {
                stage,
                cleanup_root: root,
                apply: false,
                restore: false,
            },
        ))
    }

    #[test]
    fn preview_preserves_nested_residue_and_apply_removes_it() -> Result<()> {
        let (_fixture, payload, mut args) = fixture()?;
        run_review(&args)?;
        assert!(payload.join("debug/deps/.DS_Store").is_file());
        args.apply = true;
        run_review(&args)?;
        assert!(!args.stage.exists());
        Ok(())
    }

    #[test]
    fn changing_stage_after_preview_requires_a_new_review() -> Result<()> {
        let (_fixture, payload, args) = fixture()?;
        let review = ReviewedStage::inspect(&args.cleanup_root, &args.stage)?;
        let changed = payload.join("late.txt");
        fs::write(&changed, b"keep")?;
        assert!(review.remove().is_err());
        assert_eq!(fs::read(&changed)?, b"keep");
        assert!(payload.join(".DS_Store").exists());
        Ok(())
    }

    #[test]
    fn recovery_preserves_every_entry_if_any_one_is_unrecognized() -> Result<()> {
        let (_fixture, payload, mut args) = fixture()?;
        args.apply = true;
        let user_file = payload.join("debug/deps/notes.txt");
        fs::write(&user_file, b"keep")?;
        assert!(run_review(&args).is_err());
        assert_eq!(fs::read(&user_file)?, b"keep");
        assert!(payload.join(".DS_Store").is_file());
        Ok(())
    }

    #[test]
    fn rejects_disguised_files_and_symbolic_or_hard_links() -> Result<()> {
        for kind in ["plain", "symlink", "hardlink"] {
            let (_fixture, payload, args) = fixture()?;
            let outside = args.cleanup_root.join("outside");
            fs::write(&outside, b"\0\x05\x16\x07\0\x02\0\0private")?;
            let candidate = payload.join("._file");
            match kind {
                "plain" => fs::write(&candidate, b"ordinary user content")?,
                "symlink" => symlink(&outside, &candidate)?,
                _ => fs::hard_link(&outside, &candidate)?,
            }
            assert!(ReviewedStage::inspect(&args.cleanup_root, &args.stage).is_err());
            run_review(&args)?; // Previewing restore must not remove anything.
            assert!(outside.is_file());
        }
        Ok(())
    }

    #[test]
    fn requires_original_private_stage_inside_selected_root() -> Result<()> {
        let (_fixture, _, mut args) = fixture()?;
        let other = tempfile::tempdir()?;
        args.cleanup_root = other.path().canonicalize()?;
        assert!(run_review(&args).is_err());
        args.cleanup_root = args.stage.parent().unwrap().to_owned();
        fs::set_permissions(&args.stage, fs::Permissions::from_mode(0o755))?;
        assert!(run_review(&args).is_err());
        Ok(())
    }

    #[test]
    fn restores_remaining_payload_without_deleting_stage_note() -> Result<()> {
        let (_fixture, payload, args) = fixture()?;
        fs::write(payload.join("remaining.txt"), b"remaining")?;
        let review = RestoreStage::inspect(&args.cleanup_root, &args.stage)?;
        let destination = review.original.clone();
        review.restore()?;
        assert_eq!(fs::read(destination.join("remaining.txt"))?, b"remaining");
        assert!(args.stage.join("original-path.txt").is_file());
        assert!(!payload.exists());
        Ok(())
    }

    #[test]
    fn restore_refuses_existing_destination_and_changed_note() -> Result<()> {
        let (_fixture, payload, args) = fixture()?;
        fs::write(payload.join("remaining.txt"), b"remaining")?;
        let review = RestoreStage::inspect(&args.cleanup_root, &args.stage)?;
        fs::create_dir(&review.original)?;
        fs::write(review.original.join("existing.txt"), b"existing")?;
        assert!(review.restore().is_err());
        assert_eq!(
            fs::read(args.cleanup_root.join("target/existing.txt"))?,
            b"existing"
        );
        assert_eq!(fs::read(payload.join("remaining.txt"))?, b"remaining");
        let review = RestoreStage::inspect(&args.cleanup_root, &args.stage)?;
        fs::write(
            args.stage.join("original-path.txt"),
            format!("{}\n", args.cleanup_root.join("different").display()),
        )?;
        assert!(review.restore().is_err());
        assert!(payload.exists());
        Ok(())
    }

    #[test]
    fn restore_rejects_replaced_payload_and_non_sibling_destination() -> Result<()> {
        let (_fixture, payload, args) = fixture()?;
        let review = RestoreStage::inspect(&args.cleanup_root, &args.stage)?;
        fs::rename(&payload, args.stage.join("preserved-payload"))?;
        fs::create_dir(&payload)?;
        assert!(review.restore().is_err());
        assert!(args
            .stage
            .join("preserved-payload/debug/deps/.DS_Store")
            .exists());
        fs::write(args.stage.join("original-path.txt"), "/tmp/other\n")?;
        assert!(RestoreStage::inspect(&args.cleanup_root, &args.stage).is_err());
        Ok(())
    }
}
