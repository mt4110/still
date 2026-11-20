//! The desktop keeps directory totals available for drill-down between scans.
//! Store one component per directory. Siblings occupy a contiguous range, so
//! opening one folder does not traverse or reconstruct unrelated paths.
use crate::review::inventory::{Inventory, Row, Usage};
use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub(super) struct Directories {
    root: PathBuf,
    entries: Vec<Entry>,
    #[cfg(target_os = "macos")]
    pub(super) stage_paths: Vec<PathBuf>,
}

struct Entry {
    name: Box<OsStr>,
    children: Range<usize>,
    usage: Usage,
}

impl Directories {
    pub(super) fn from_inventory(root: &Path, inventory: Inventory) -> Self {
        let mut rows = inventory.directories;
        // Parent groups appear in path order, with the root group first.
        // Thus each parent is already indexed when its children are appended.
        rows.sort_unstable_by(|a, b| {
            a.path
                .parent()
                .cmp(&b.path.parent())
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        let mut entries = Vec::with_capacity(rows.len().max(1));
        entries.push(Entry {
            name: OsStr::new("").into(),
            children: 0..0,
            usage: Default::default(),
        });
        let mut index = Self {
            root: root.to_owned(),
            entries,
            #[cfg(target_os = "macos")]
            stage_paths: Vec::new(),
        };
        let mut rows = rows.into_iter().peekable();
        if rows.peek().is_some_and(|row| row.path == root) {
            index.entries[0].usage = rows.next().expect("root row").usage;
        }
        while let Some(first) = rows.next() {
            let parent = first
                .path
                .parent()
                .expect("directory below scan root")
                .to_owned();
            let parent_id = index
                .find(&parent)
                .expect("inventory includes all ancestors");
            let start = index.entries.len();
            index.append(first);
            while rows
                .peek()
                .is_some_and(|row| row.path.parent() == Some(parent.as_path()))
            {
                index.append(rows.next().expect("peeked sibling"));
            }
            index.entries[parent_id].children = start..index.entries.len();
        }
        index
    }

    fn append(&mut self, row: Row) {
        let name = row.path.file_name().expect("child directory name");
        #[cfg(target_os = "macos")]
        if name.as_encoded_bytes().starts_with(b".still-stage-") {
            self.stage_paths.push(row.path.clone());
        }
        self.entries.push(Entry {
            name: name.into(),
            children: 0..0,
            usage: row.usage,
        });
    }

    fn find(&self, path: &Path) -> Option<usize> {
        let mut index = 0;
        for component in path.strip_prefix(&self.root).ok()?.iter() {
            let range = self.entries[index].children.clone();
            let offset = self.entries[range.clone()]
                .binary_search_by(|e| e.name.as_ref().cmp(component))
                .ok()?;
            index = range.start + offset;
        }
        Some(index)
    }

    pub(super) fn row(&self, path: &Path) -> Option<Row> {
        Some(Row {
            path: path.to_owned(),
            is_dir: true,
            usage: self.entries[self.find(path)?].usage.clone(),
        })
    }

    pub(super) fn children<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = Row> + 'a {
        let range = self
            .find(path)
            .map_or(0..0, |id| self.entries[id].children.clone());
        self.entries[range].iter().map(move |e| Row {
            path: path.join(e.name.as_ref()),
            is_dir: true,
            usage: e.usage.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::inventory;

    #[test]
    fn compact_folders_retain_all_nested_rows_direct_children_and_native_names(
    ) -> anyhow::Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().canonicalize()?;
        for p in ["a/deeper/child", "ab", "second/sub", "a/another"] {
            std::fs::create_dir_all(root.join(p))?;
        }
        std::fs::write(root.join("a/deeper/child/book"), [9; 123])?;
        let mut inventory = inventory::scan(&root, 2, None)?;
        #[cfg(unix)]
        {
            // APFS rejects some byte sequences even though Unix Path supports
            // them. Exercise the index representation without requiring the
            // local filesystem to accept this particular component.
            use std::os::unix::ffi::OsStrExt;
            inventory.directories.push(Row {
                path: root.join(OsStr::from_bytes(b"odd-\xff")),
                is_dir: true,
                usage: Usage::default(),
            });
        }
        inventory
            .directories
            .sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let reference = inventory.directories.clone();
        let index = Directories::from_inventory(&root, inventory);
        for row in &reference {
            let actual = index.row(&row.path).unwrap();
            assert_eq!(actual.usage.logical_bytes, row.usage.logical_bytes);
            assert_eq!(actual.usage.allocated_bytes, row.usage.allocated_bytes);
            assert_eq!(actual.usage.files, row.usage.files);
            let children: Vec<_> = index.children(&row.path).map(|r| r.path).collect();
            let expected: Vec<_> = reference
                .iter()
                .filter(|r| r.path.parent() == Some(&row.path))
                .map(|r| r.path.clone())
                .collect();
            assert_eq!(children, expected);
        }
        assert!(index.row(&root.join("missing")).is_none());
        assert_eq!(index.children(&root.join("missing")).count(), 0);
        Ok(())
    }
}
