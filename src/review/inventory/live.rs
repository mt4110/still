use super::*;
use std::collections::BTreeSet;

pub(in crate::review) struct Snapshot {
    raw: RawDirectories,
    identity: crate::review::filesystem::Identity,
}

impl Snapshot {
    pub(in crate::review) fn scan(
        root: &Path,
        threads: u16,
        progress: Option<&ScanProgress>,
    ) -> Result<(Inventory, Self)> {
        let identity = super::super::filesystem::identity(root, &fs::symlink_metadata(root)?)?;
        let (inventory, raw) = scan_impl(root, threads, None, progress, true)?;
        Ok((
            inventory,
            Self {
                raw: raw.expect("retained inventory"),
                identity,
            },
        ))
    }

    pub(in crate::review) fn reconcile(
        &self,
        root: &Path,
        changed: &BTreeSet<PathBuf>,
        replaced: &BTreeSet<PathBuf>,
    ) -> Result<(Inventory, Self)> {
        let root_metadata = fs::symlink_metadata(safe_dir(root)?)?;
        if super::super::filesystem::identity(root, &root_metadata)? != self.identity {
            bail!("The scanned folder was replaced.");
        }
        let mut raw = self.raw.clone();
        let was_replaced = |path: &Path| path.ancestors().any(|dir| replaced.contains(dir));
        if !replaced.is_empty() {
            raw.directories.retain(|path, _| !was_replaced(path));
            raw.cargo_marker_dirs.retain(|path| !was_replaced(path));
            raw.hardlinks.retain(|(_, path, _)| !was_replaced(path));
        }
        let prior_links = raw.hardlinks.len();
        let now = chrono::Utc::now();
        let cutoff = now - chrono::TimeDelta::days(7);
        let mut queue = changed.clone();
        let mut refreshed = HashSet::new();
        let mut removed = HashSet::new();
        let mut cached_children: HashMap<&Path, HashSet<&Path>> = HashMap::new();
        for path in self.raw.directories.keys() {
            if let Some(parent) = path.parent().filter(|p| changed.contains(*p)) {
                cached_children.entry(parent).or_default().insert(path);
            }
        }

        while let Some(path) = queue.pop_first() {
            if removed.iter().any(|prefix| path.starts_with(prefix)) {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.is_dir()
                        && !is_link(&metadata)
                        && same_device(&root_metadata, &metadata) =>
                {
                    metadata
                }
                Ok(_) => {
                    removed.insert(path);
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    removed.insert(path);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            // Validate intermediate components before reading a dirty directory.
            safe_dir(&path)?;
            let entries = fs::read_dir(&path).with_context(|| display(&path))?;
            refreshed.insert(path.clone());
            raw.cargo_marker_dirs.remove(&path);
            raw.directories.insert(
                path.clone(),
                Usage {
                    modified_at: metadata.modified().ok().map(Into::into),
                    ..Usage::default()
                },
            );
            let mut absent = cached_children.remove(path.as_path()).unwrap_or_default();
            for entry in entries {
                let Ok(entry) = entry else {
                    raw.directories
                        .get_mut(&path)
                        .expect("refresh parent")
                        .skipped += 1;
                    continue;
                };
                let child = entry.path();
                match fs::symlink_metadata(&child) {
                    Ok(md) if md.is_dir() && !is_link(&md) && same_device(&root_metadata, &md) => {
                        absent.remove(child.as_path());
                        // A newly created or renamed directory may already contain a subtree.
                        if !raw.directories.contains_key(&child) {
                            queue.insert(child);
                        }
                    }
                    Ok(md) if md.is_file() && same_device(&root_metadata, &md) => {
                        record_file(&mut raw, &child, &path, &md, cutoff, now);
                    }
                    _ => {
                        raw.directories
                            .get_mut(&path)
                            .expect("refresh parent")
                            .skipped += 1
                    }
                }
            }
            removed.extend(absent.into_iter().map(Path::to_path_buf));
        }

        let was_removed = |path: &Path| path.ancestors().any(|dir| removed.contains(dir));
        if !removed.is_empty() {
            raw.directories.retain(|path, _| !was_removed(path));
            raw.cargo_marker_dirs.retain(|path| !was_removed(path));
        }
        // Ignore previous allocations from refreshed folders. Reattribute a
        // removed hard link's blocks to the next surviving link in scan order.
        let mut fresh_links = raw.hardlinks.split_off(prior_links);
        raw.hardlinks.retain(|(_, path, _)| {
            !was_removed(path) && !path.parent().is_some_and(|p| refreshed.contains(p))
        });
        raw.hardlinks.append(&mut fresh_links);
        let (directories, cargo_marker_dirs) = raw.clone().aggregate(root);
        Ok((
            Inventory {
                directories,
                cargo_marker_dirs,
                matches: vec![],
                omitted_matches: 0,
            },
            Self {
                raw,
                identity: self.identity,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_removal_matches_a_full_inventory_and_keeps_other_branches() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().canonicalize()?;
        fs::create_dir_all(root.join("a/deep"))?;
        fs::create_dir(root.join("b"))?;
        fs::write(root.join("a/deep/file"), vec![1; 8192])?;
        fs::hard_link(root.join("a/deep/file"), root.join("b/link"))?;
        fs::write(root.join("a/loose"), b"largest")?;
        let (_, cached) = Snapshot::scan(&root, 2, None)?;
        fs::remove_file(root.join("a/deep/file"))?;
        fs::remove_dir(root.join("a/deep"))?;
        fs::remove_file(root.join("a/loose"))?;
        let (actual, _) = cached.reconcile(
            &root,
            &BTreeSet::from([root.join("a"), root.join("a/deep")]),
            &BTreeSet::new(),
        )?;
        let expected = super::super::scan(&root, 2, None)?;
        assert_eq!(
            serde_json::to_value(actual)?,
            serde_json::to_value(expected)?
        );
        Ok(())
    }
}
