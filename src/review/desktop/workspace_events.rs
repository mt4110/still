use eframe::egui;
use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex};

type Ref = *mut c_void;
type ConstRef = *const c_void;
type Callback = unsafe extern "C" fn(ConstRef, Ref, usize, Ref, *const u32, *const u64);

#[repr(C)]
struct Context {
    version: isize,
    info: Ref,
    retain: Option<unsafe extern "C" fn(ConstRef) -> ConstRef>,
    release: Option<unsafe extern "C" fn(ConstRef)>,
    description: Option<unsafe extern "C" fn(ConstRef) -> ConstRef>,
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        allocator: ConstRef,
        callback: Callback,
        context: *const Context,
        paths: ConstRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> Ref;
    fn FSEventStreamSetDispatchQueue(stream: Ref, queue: Ref);
    fn FSEventStreamStart(stream: Ref) -> u8;
    fn FSEventStreamStop(stream: Ref);
    fn FSEventStreamInvalidate(stream: Ref);
    fn FSEventStreamRelease(stream: Ref);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithFileSystemRepresentation(allocator: ConstRef, path: *const c_char) -> Ref;
    fn CFArrayCreate(
        allocator: ConstRef,
        values: *const ConstRef,
        count: isize,
        callbacks: ConstRef,
    ) -> Ref;
    fn CFRelease(value: ConstRef);
    static kCFTypeArrayCallBacks: u8;
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: ConstRef) -> Ref;
    fn dispatch_release(queue: Ref);
}

const WATCH_ROOT: u32 = 0x04;
const FILE_EVENTS: u32 = 0x10;
const HISTORY_DONE: u32 = 0x10;
const RESCAN_REQUIRED: u32 = 0x0f | 0xe0;
const ITEM_IS_DIR: u32 = 0x0002_0000;
const ITEM_IS_HARDLINK: u32 = 0x0010_0000;
const CREATED: u32 = 0x100;
const RENAMED: u32 = 0x800;
const REMOVED_OR_RENAMED: u32 = 0x200 | RENAMED;
const MODIFIED: u32 = 0x1000;
const MAX_DIRS: usize = 2048;

#[derive(Default)]
pub(super) struct Changes {
    pub directories: BTreeSet<PathBuf>,
    pub rescan: bool,
    pub renamed_or_created: BTreeSet<PathBuf>,
}

struct CallbackState {
    root: PathBuf,
    changes: Mutex<Changes>,
    ctx: egui::Context,
}

pub(super) struct Watch {
    stream: Ref,
    state: Arc<CallbackState>,
}

impl Watch {
    pub fn start(root: PathBuf, ctx: egui::Context) -> Option<Self> {
        let encoded = CString::new(root.as_os_str().as_bytes()).ok()?;
        let state = Arc::new(CallbackState {
            root,
            changes: Mutex::default(),
            ctx,
        });
        let context = Context {
            version: 0,
            info: Arc::as_ptr(&state).cast_mut().cast(),
            retain: Some(retain_state),
            release: Some(release_state),
            description: None,
        };
        // FSEvents owns a reference to the callback state independently of Watch.
        unsafe {
            let path = CFStringCreateWithFileSystemRepresentation(ptr::null(), encoded.as_ptr());
            if path.is_null() {
                return None;
            }
            let paths = CFArrayCreate(
                ptr::null(),
                &path.cast_const(),
                1,
                ptr::addr_of!(kCFTypeArrayCallBacks).cast(),
            );
            CFRelease(path);
            if paths.is_null() {
                return None;
            }
            let stream = FSEventStreamCreate(
                ptr::null(),
                receive,
                &context,
                paths,
                u64::MAX,
                0.35,
                WATCH_ROOT | FILE_EVENTS,
            );
            CFRelease(paths);
            if stream.is_null() {
                return None;
            }
            let queue =
                dispatch_queue_create(c"com.mt4110.still.workspace-events".as_ptr(), ptr::null());
            FSEventStreamSetDispatchQueue(stream, queue);
            dispatch_release(queue);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                return None;
            }
            Some(Self { stream, state })
        }
    }

    pub fn take(&self) -> Changes {
        std::mem::take(&mut *self.state.changes.lock().expect("workspace changes"))
    }

    pub fn root(&self) -> &Path {
        &self.state.root
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
        }
    }
}

unsafe extern "C" fn retain_state(state: ConstRef) -> ConstRef {
    unsafe { Arc::increment_strong_count(state.cast::<CallbackState>()) };
    state
}

unsafe extern "C" fn release_state(state: ConstRef) {
    unsafe { Arc::decrement_strong_count(state.cast::<CallbackState>()) };
}

unsafe extern "C" fn receive(
    _: ConstRef,
    state: Ref,
    count: usize,
    paths: Ref,
    flags: *const u32,
    _: *const u64,
) {
    if count == 0 {
        return;
    }
    let state = unsafe { &*state.cast::<CallbackState>() };
    let paths = unsafe { std::slice::from_raw_parts(paths.cast::<*const c_char>(), count) };
    let flags = unsafe { std::slice::from_raw_parts(flags, count) };
    let Ok(mut changes) = state.changes.lock() else {
        return;
    };
    for (&path, &flags) in paths.iter().zip(flags) {
        let path = Path::new(OsStr::from_bytes(
            unsafe { CStr::from_ptr(path) }.to_bytes(),
        ));
        changes.record(&state.root, path, flags);
    }
    if changes.rescan || !changes.directories.is_empty() {
        state.ctx.request_repaint();
    }
}

impl Changes {
    fn record(&mut self, root: &Path, path: &Path, flags: u32) {
        if self.rescan || flags & HISTORY_DONE != 0 {
            return;
        }
        if flags & RESCAN_REQUIRED != 0
            || flags & ITEM_IS_HARDLINK != 0 && flags & (CREATED | MODIFIED) != 0
        {
            self.rescan = true;
            self.directories.clear();
            self.renamed_or_created.clear();
            return;
        }
        if !path.starts_with(root) {
            return;
        }
        let is_dir = flags & ITEM_IS_DIR != 0 || path == root;
        if is_dir {
            self.directories.insert(path.to_owned());
            if path != root && flags & (CREATED | RENAMED) != 0 {
                self.renamed_or_created.insert(path.to_owned());
            }
        }
        if !is_dir || flags & (CREATED | REMOVED_OR_RENAMED) != 0 {
            if let Some(parent) = path.parent().filter(|parent| parent.starts_with(root)) {
                self.directories.insert(parent.to_owned());
            }
        }
        if self.directories.len() > MAX_DIRS {
            self.rescan = true;
            self.directories.clear();
            self.renamed_or_created.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_notification_updates_a_candidate_and_its_parent_without_a_full_scan() -> anyhow::Result<()>
    {
        use super::super::scan::Scan;
        use crate::review::{artifacts, inventory};
        use std::fs;
        use std::time::{Duration, Instant};

        let fixture = tempfile::tempdir()?;
        let parent = fixture.path().canonicalize()?;
        let root = parent.join("workspace");
        let output = root.join("project/generated");
        fs::create_dir_all(&output)?;
        fs::write(output.join("artifact.bin"), [1; 8192])?;
        let watch = Watch::start(root.clone(), egui::Context::default())
            .ok_or_else(|| anyhow::anyhow!("FSEvents could not start"))?;
        let (initial, snapshot) = inventory::live::Snapshot::scan(&root, 2, None)?;
        let policy = artifacts::Policy {
            roots: vec![root.clone()],
            outputs: vec![output.clone()],
            docker_context: None,
        };
        let previous = artifacts::file_candidates(&initial, &policy);
        assert_eq!(previous.len(), 1);
        fs::rename(&output, parent.join("moved-outside-the-workspace"))?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut changed = BTreeSet::new();
        let mut replaced = BTreeSet::new();
        while Instant::now() < deadline && !changed.contains(&output) {
            let events = watch.take();
            assert!(!events.rescan, "ordinary rename requested a full scan");
            changed.extend(events.directories);
            replaced.extend(events.renamed_or_created);
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            changed.contains(&output),
            "the OS did not report the rename"
        );
        let rules = super::super::keep::KeepRules::default();
        let changes = Changes {
            directories: changed,
            renamed_or_created: replaced,
            rescan: false,
        };
        let update = Scan::reconcile(
            &snapshot,
            &root,
            &changes,
            &policy,
            &previous,
            Some(&rules),
            vec![],
        )?;
        assert!(update.files.is_empty());
        assert!(update.directories.row(&output).is_none());
        assert_eq!(
            update.directories.row(&root).unwrap().usage.logical_bytes,
            0
        );
        Ok(())
    }

    #[test]
    fn file_events_refresh_the_parent_and_directory_removal_refreshes_both() {
        let root = Path::new("/workspace");
        let mut changes = Changes::default();
        changes.record(root, Path::new("/workspace/first/file"), REMOVED_OR_RENAMED);
        changes.record(
            root,
            Path::new("/workspace/second"),
            ITEM_IS_DIR | REMOVED_OR_RENAMED,
        );
        assert_eq!(
            changes.directories,
            BTreeSet::from([root.to_owned(), root.join("first"), root.join("second")])
        );
        changes.record(root, root, 1);
        assert!(changes.rescan);
        assert!(changes.directories.is_empty());
    }
}
