//! Preview gallery catalog: opaque ids over a watched folder, with the
//! review-hardened confinement rules from the design spec (direct children
//! only, regular files only, openat/O_NOFOLLOW + fstat re-verification,
//! magic-byte validation, revision = hash(dev, inode, size, mtime)).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const MAX_ENTRIES: usize = 64;
pub const MAX_SCAN: usize = 512;
pub const SNAPSHOT_TTL_MS: u64 = 2000;
pub const MAX_FULL_BYTES: u64 = 64 * 1024 * 1024;
/// Category for files sitting directly in the watched folder — a flat
/// gallery keeps working untouched, it just has one bucket.
pub const UNSORTED: &str = "Unsorted";
/// Direct subfolders are projects; more than this and the phone's chip row
/// stops being navigation. Bounds the per-rescan directory work too.
pub const MAX_CATEGORIES: usize = 16;
/// Internal descriptor key for files sitting at the gallery root. The empty
/// string is not a legal directory name, so it can never collide with a real
/// project — which leaves the DISPLAY name "Unsorted" free to also be a
/// folder someone actually made.
const ROOT_BUCKET: &str = "";
/// The root pass has to find PROJECTS as well as loose images, and a single
/// shared budget lets a pile of loose renders hide every folder behind it.
/// Files and folders get independent budgets, bounded overall by this: the
/// walk stops as soon as both are satisfied, so a tidy gallery pays nothing.
const MAX_ROOT_SCAN: usize = 8192;

/// The live Blender viewport's synthetic entry id (spec phase 2): one slot,
/// served from memory, revision-bumped per captured frame.
pub const VIEWPORT_ID: &str = "live";
/// A frame older than this stops being listed — the tile disappears rather
/// than showing a frozen viewport as if it were live.
pub const VIEWPORT_FRESH_MS: u64 = 15_000;
/// A /previews list within this window counts as "someone is watching";
/// the bridge captures only then, so Blender is never polled for nobody.
pub const DEMAND_WINDOW_MS: u64 = 15_000;

fn viewport_fresh(age: Duration) -> bool {
    age <= Duration::from_millis(VIEWPORT_FRESH_MS)
}

const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

#[derive(Debug, Clone)]
pub struct PreviewEntry {
    pub id: String,
    pub name: String,
    /// Owning subfolder, or UNSORTED for a file at the gallery root.
    pub category: String,
    pub revision: String,
    pub modified_at: u64,
    pub bytes: u64,
}

pub(crate) struct FileIdentity {
    pub name: String,
    /// Descriptor key, NOT the display label — see ROOT_BUCKET.
    pub bucket: String,
    pub dev: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_nanos: u64,
    pub revision: String,
}

pub struct CatalogSnapshot {
    pub available: bool,
    pub entries: Vec<PreviewEntry>,
    /// One descriptor per category, held open across the snapshot's
    /// lifetime: `open_verified` opens ids RELATIVE to the descriptor of
    /// their OWN category, always by bare name. No path is ever joined, so
    /// swapping a directory on disk cannot redirect an id to a new tree and
    /// an id can never escape the category it was listed in.
    pub(crate) dirs: HashMap<String, std::fs::File>,
    pub(crate) by_id: HashMap<String, FileIdentity>,
}

struct Inner {
    dir: Option<PathBuf>,
    /// Monotonic per-store id counter; ids stay opaque and never repeat.
    next_id: u64,
    /// (dev, inode, category, name) -> id: unchanged files keep their id across
    /// rescans so the phone's id-keyed tiles never churn on a poll, while
    /// hard links (same inode, different names) stay distinct entries.
    /// Pruned to the current scan's survivors each pass (bounded by
    /// MAX_ENTRIES).
    ids: HashMap<(u64, u64, String, String), String>,
    cached: Option<(Instant, Arc<CatalogSnapshot>)>,
}

struct Viewport {
    bytes: Arc<Vec<u8>>,
    revision: String,
    seq: u64,
    at: Instant,
}

pub struct PreviewStore {
    inner: Mutex<Inner>,
    viewport: Mutex<Option<Viewport>>,
    viewport_enabled: std::sync::atomic::AtomicBool,
    last_demand: Mutex<Option<Instant>>,
}

/// FNV-1a over the identity tuple: replacement (new inode/size/mtime) is a
/// new revision, so caches can never serve stale bytes for an id. The mtime
/// participates at nanosecond resolution — a same-size in-place rewrite
/// within one second still moves the revision.
fn revision_of(dev: u64, inode: u64, size: u64, mtime_nanos: u64) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for value in [dev, inode, size, mtime_nanos] {
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

impl PreviewStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                dir,
                next_id: 1,
                ids: HashMap::new(),
                cached: None,
            }),
            viewport: Mutex::new(None),
            viewport_enabled: std::sync::atomic::AtomicBool::new(false),
            last_demand: Mutex::new(None),
        }
    }

    /// Publish the latest captured Blender frame; each call is a new
    /// revision, so ETag polling sees every frame exactly once.
    pub fn set_viewport_frame(&self, bytes: Vec<u8>) {
        let mut slot = self.viewport.lock().unwrap();
        let seq = slot.as_ref().map(|v| v.seq + 1).unwrap_or(1);
        *slot = Some(Viewport {
            bytes: Arc::new(bytes),
            revision: format!("v{seq}"),
            seq,
            at: Instant::now(),
        });
    }

    /// The synthetic gallery entry for the live tile — Some only while a
    /// frame is fresh, independent of the watched dir's availability.
    pub fn viewport_entry(&self) -> Option<PreviewEntry> {
        let slot = self.viewport.lock().unwrap();
        let viewport = slot.as_ref()?;
        viewport_fresh(viewport.at.elapsed()).then(|| PreviewEntry {
            id: VIEWPORT_ID.into(),
            name: "blender viewport".into(),
            // The live tile belongs to no project: the phone pins it above
            // every filter, so this value never groups anything.
            category: UNSORTED.into(),
            revision: viewport.revision.clone(),
            modified_at: 0,
            bytes: viewport.bytes.len() as u64,
        })
    }

    /// Frame bytes for an exact revision; None = stale (phone re-lists).
    pub fn viewport_frame(&self, revision: &str) -> Option<Arc<Vec<u8>>> {
        let slot = self.viewport.lock().unwrap();
        let viewport = slot.as_ref()?;
        (viewport.revision == revision).then(|| Arc::clone(&viewport.bytes))
    }

    pub fn set_viewport_enabled(&self, on: bool) {
        self.viewport_enabled
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// True when the bridge should capture: the feature is on AND a phone
    /// listed the gallery within the demand window.
    pub fn viewport_wanted(&self) -> bool {
        self.viewport_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .last_demand
                .lock()
                .unwrap()
                .is_some_and(|at| at.elapsed() < Duration::from_millis(DEMAND_WINDOW_MS))
    }

    /// Swap the watched folder atomically; the next snapshot() rescans.
    pub fn set_dir(&self, dir: Option<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        inner.dir = dir;
        inner.cached = None;
    }

    pub fn snapshot(&self) -> Arc<CatalogSnapshot> {
        *self.last_demand.lock().unwrap() = Some(Instant::now());
        let mut inner = self.inner.lock().unwrap();
        if let Some((at, snap)) = &inner.cached {
            if at.elapsed() < Duration::from_millis(SNAPSHOT_TTL_MS) {
                return Arc::clone(snap);
            }
        }
        let dir = inner.dir.clone();
        let inner = &mut *inner;
        let snap = Arc::new(scan(dir, &mut inner.next_id, &mut inner.ids));
        inner.cached = Some((Instant::now(), Arc::clone(&snap)));
        snap
    }
}

fn unavailable() -> CatalogSnapshot {
    CatalogSnapshot {
        available: false,
        entries: Vec::new(),
        dirs: HashMap::new(),
        by_id: HashMap::new(),
    }
}

fn has_image_ext(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// A candidate file, tagged with the category whose descriptor will open it.
struct Candidate {
    mtime: u64,
    /// Descriptor key: which open directory this name is opened relative to.
    bucket: String,
    /// What the phone shows; root files borrow the UNSORTED label.
    category: String,
    name: String,
    meta: std::fs::Metadata,
}

/// List one directory's images. readdir supplies NAMES only; every stat
/// goes through `stat_at` on the pinned descriptor, so a directory swapped
/// for a symlink between opening it and listing it cannot feed foreign
/// names, sizes or mtimes into the catalog — a name that is not really in
/// the opened directory simply fails to stat and is dropped.
fn collect_images(
    handle: &std::fs::File,
    path: &std::path::Path,
    bucket: &str,
    category: &str,
    into: &mut Vec<Candidate>,
) {
    let Ok(read) = std::fs::read_dir(path) else {
        return;
    };
    for entry in read.take(MAX_SCAN).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !has_image_ext(name) {
            continue;
        }
        let Some(meta) = stat_at(handle, name) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        into.push(Candidate {
            mtime: mtime_nanos(&meta),
            bucket: bucket.to_string(),
            category: category.to_string(),
            name: name.to_string(),
            meta,
        });
    }
}

/// Newest-first within a category, name as the tiebreaker, capped at
/// MAX_ENTRIES *per category* — a burst of renders in one project can never
/// push another project's previews out of the list.
fn cap_per_category(mut found: Vec<Candidate>) -> Vec<Candidate> {
    found.sort_by(|a, b| {
        a.bucket
            .cmp(&b.bucket)
            .then(b.mtime.cmp(&a.mtime))
            .then(a.name.cmp(&b.name))
    });
    let mut kept: Vec<Candidate> = Vec::new();
    let mut run = 0usize;
    for candidate in found {
        match kept.last() {
            Some(previous) if previous.bucket == candidate.bucket => run += 1,
            _ => run = 0,
        }
        if run < MAX_ENTRIES {
            kept.push(candidate);
        }
    }
    kept.sort_by(|a, b| b.mtime.cmp(&a.mtime).then(a.name.cmp(&b.name)));
    kept
}

fn scan(
    dir: Option<PathBuf>,
    next_id: &mut u64,
    ids: &mut HashMap<(u64, u64, String, String), String>,
) -> CatalogSnapshot {
    let Some(dir) = dir else { return unavailable() };
    let Ok(root) = std::fs::File::open(&dir) else {
        return unavailable();
    };
    if !root.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return unavailable();
    }
    let Ok(read) = std::fs::read_dir(&dir) else {
        return unavailable();
    };

    // Root pass: loose images are UNSORTED, direct subfolders are projects.
    // The two have SEPARATE budgets so neither can starve the other: once
    // the file budget is spent the walk keeps going for folders, because a
    // pile of loose renders must never hide a project behind it.
    //
    // The dir/file split comes from readdir's own d_type where the volume
    // supplies it (file_type() never follows symlinks, so a symlinked
    // "folder" is neither dir nor file and falls through) — that keeps the
    // longer walk cheap, and only real image candidates pay for a stat.
    let mut found: Vec<Candidate> = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    let mut files_seen = 0usize;
    for entry in read.take(MAX_ROOT_SCAN).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            categories.push(name.to_string());
            continue;
        }
        if !kind.is_file() || !has_image_ext(name) || files_seen >= MAX_SCAN {
            continue;
        }
        files_seen += 1;
        // Authoritative stat through the descriptor, never the path.
        let Some(meta) = stat_at(&root, name) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        found.push(Candidate {
            mtime: mtime_nanos(&meta),
            bucket: ROOT_BUCKET.to_string(),
            category: UNSORTED.to_string(),
            name: name.to_string(),
            meta,
        });
    }
    // Sort before truncating so WHICH categories survive the cap is
    // deterministic rather than readdir order.
    categories.sort();
    categories.truncate(MAX_CATEGORIES);

    let mut dirs: HashMap<String, std::fs::File> = HashMap::new();
    for category in categories {
        let Some(handle) = open_subdir(&root, &category) else {
            continue;
        };
        collect_images(
            &handle,
            &dir.join(&category),
            &category,
            &category,
            &mut found,
        );
        dirs.insert(category, handle);
    }
    dirs.insert(ROOT_BUCKET.to_string(), root);

    let found = cap_per_category(found);
    let mut entries = Vec::with_capacity(found.len());
    let mut by_id = HashMap::with_capacity(found.len());
    let mut seen: std::collections::HashSet<(u64, u64, String, String)> =
        std::collections::HashSet::new();
    for candidate in found {
        let Candidate {
            mtime,
            bucket,
            category,
            name,
            meta,
        } = candidate;
        let identity_key = (meta.dev(), meta.ino(), bucket.clone(), name.clone());
        seen.insert(identity_key.clone());
        let id = ids
            .entry(identity_key)
            .or_insert_with(|| {
                let id = format!("p{next_id}");
                *next_id += 1;
                id
            })
            .clone();
        let revision = revision_of(meta.dev(), meta.ino(), meta.len(), mtime);
        entries.push(PreviewEntry {
            id: id.clone(),
            name: name.clone(),
            category: category.clone(),
            revision: revision.clone(),
            modified_at: mtime / 1_000_000_000,
            bytes: meta.len(),
        });
        by_id.insert(
            id,
            FileIdentity {
                name,
                bucket,
                dev: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                mtime_nanos: mtime,
                revision,
            },
        );
    }
    // Bound the id map to this scan's survivors.
    ids.retain(|key, _| seen.contains(key));
    CatalogSnapshot {
        available: true,
        entries,
        dirs,
        by_id,
    }
}

// openat is the one call std does not surface: opening RELATIVE to the held
// directory descriptor (with O_NOFOLLOW) is what pins ids inside the watched
// dir. Same hand-rolled FFI style as companion/net.rs's getifaddrs.
extern "C" {
    fn openat(
        dirfd: std::os::raw::c_int,
        path: *const std::os::raw::c_char,
        flags: std::os::raw::c_int,
    ) -> std::os::raw::c_int;
}
const O_RDONLY: std::os::raw::c_int = 0x0000;
const O_NOFOLLOW: std::os::raw::c_int = 0x0100;
const O_CLOEXEC: std::os::raw::c_int = 0x0100_0000;
const O_DIRECTORY: std::os::raw::c_int = 0x0010_0000;
/// Opening a FIFO read-only BLOCKS until a writer appears. A file named
/// `x.png` that is really a fifo would wedge the scan while it holds the
/// store mutex, taking later gallery reads, settings updates and shutdown
/// with it. O_NONBLOCK makes the open return immediately; the regular-file
/// check right after it is what actually rejects the fifo.
const O_NONBLOCK: std::os::raw::c_int = 0x0004;

/// Stat a bare name INSIDE an opened directory, refusing symlinks. A
/// path-based lstat would follow a directory that was swapped for a symlink
/// after we opened it; this cannot, because the name is resolved against the
/// descriptor we pinned. std does the fstat for us on the open file, so no
/// `struct stat` layout has to be hand-declared.
fn stat_at(dir: &std::fs::File, name: &str) -> Option<std::fs::Metadata> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY: cname is a valid NUL-terminated C string; the fd is owned by
    // the File we construct immediately on success.
    let fd = unsafe {
        openat(
            dir.as_raw_fd(),
            cname.as_ptr(),
            O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    unsafe { std::fs::File::from_raw_fd(fd) }.metadata().ok()
}

/// Open a category subfolder RELATIVE to the gallery root descriptor. The
/// name comes straight from readdir so it can never contain a slash, and
/// O_NOFOLLOW|O_DIRECTORY closes the lstat-then-open race: a name swapped
/// for a symlink between the two fails the open rather than escaping.
fn open_subdir(root: &std::fs::File, name: &str) -> Option<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: name is a valid NUL-terminated C string; the fd is owned by
    // the File we construct immediately on success.
    let fd = unsafe {
        openat(
            root.as_raw_fd(),
            name.as_ptr(),
            O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    Some(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub struct VerifiedFile {
    pub file: std::fs::File,
    pub len: u64,
    pub content_type: &'static str,
}

pub(crate) fn sniff_content_type(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some("image/webp");
    }
    None
}

impl CatalogSnapshot {
    /// Open an id's bytes with the confinement contract: openat relative to
    /// the descriptor of the id's OWN category, O_NOFOLLOW, then fstat
    /// re-verification of regular
    /// type + dev + inode + size, then magic-byte validation. Any mismatch
    /// (including a revision the client learned from an older list) fails
    /// closed with None.
    pub fn open_verified(&self, id: &str, revision: &str) -> Option<VerifiedFile> {
        use std::io::{Read, Seek};
        use std::os::fd::{AsRawFd, FromRawFd};
        let identity = self.by_id.get(id)?;
        if identity.revision != revision {
            return None;
        }
        let dir = self.dirs.get(&identity.bucket)?;
        let name = std::ffi::CString::new(identity.name.as_str()).ok()?;
        // SAFETY: name is a valid NUL-terminated C string; the fd is owned
        // by the File we construct immediately on success.
        let fd = unsafe {
            openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC,
            )
        };
        if fd < 0 {
            return None;
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let meta = file.metadata().ok()?; // fstat on the OPEN descriptor
        if !meta.is_file()
            || meta.dev() != identity.dev
            || meta.ino() != identity.inode
            || meta.len() != identity.size
            || mtime_nanos(&meta) != identity.mtime_nanos
        {
            return None;
        }
        let mut head = [0u8; 12];
        let read = file.read(&mut head).ok()?;
        let content_type = sniff_content_type(&head[..read])?;
        file.rewind().ok()?;
        Some(VerifiedFile {
            file,
            len: meta.len(),
            content_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("st-previews-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Smallest valid PNG header bytes (magic + fake IHDR): enough for the
    // extension prefilter and magic sniff; sips never sees these files.
    fn write_png(dir: &std::path::Path, name: &str, extra: usize) {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        bytes.extend_from_slice(&[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
        bytes.extend(std::iter::repeat(0u8).take(extra));
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn set_mtime(path: &std::path::Path, to: std::time::SystemTime) {
        let file = std::fs::File::options().append(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(to))
            .unwrap();
    }

    #[test]
    fn missing_dir_is_unavailable_not_empty() {
        let store = PreviewStore::new(Some(PathBuf::from("/nonexistent/st-previews")));
        let snap = store.snapshot();
        assert!(!snap.available);
        assert!(snap.entries.is_empty());
        let none = PreviewStore::new(None);
        assert!(!none.snapshot().available);
    }

    #[test]
    fn scan_lists_images_newest_first_with_opaque_ids() {
        let dir = scratch("scan");
        write_png(&dir, "old.png", 10);
        write_png(&dir, "new.png", 20);
        let newer = std::time::SystemTime::now();
        let older = newer - std::time::Duration::from_secs(60);
        set_mtime(&dir.join("old.png"), older);
        set_mtime(&dir.join("new.png"), newer);
        std::fs::write(dir.join("notes.txt"), b"not an image").unwrap();
        let store = PreviewStore::new(Some(dir));
        let snap = store.snapshot();
        assert!(snap.available);
        assert_eq!(snap.entries.len(), 2, "txt is filtered by extension");
        assert_eq!(snap.entries[0].name, "new.png");
        assert!(
            snap.entries[0].id.starts_with('p'),
            "{}",
            snap.entries[0].id
        );
        assert_ne!(snap.entries[0].id, snap.entries[1].id);
    }

    #[test]
    fn symlinked_files_are_ignored_at_every_level() {
        let dir = scratch("links");
        write_png(&dir, "real.png", 0);
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        write_png(&dir.join("Hollowward"), "inside.png", 0);
        std::os::unix::fs::symlink(dir.join("real.png"), dir.join("link.png")).unwrap();
        std::os::unix::fs::symlink(dir.join("real.png"), dir.join("Hollowward/link.png")).unwrap();
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let mut names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["inside.png", "real.png"],
            "no symlink at either level"
        );
    }

    #[test]
    fn a_symlinked_folder_is_not_a_category() {
        let dir = scratch("linkdir");
        let outside = scratch("linkdir-outside");
        write_png(&outside, "secret.png", 0);
        std::os::unix::fs::symlink(&outside, dir.join("Elsewhere")).unwrap();
        let snap = PreviewStore::new(Some(dir)).snapshot();
        assert!(
            snap.entries.is_empty(),
            "a symlinked dir cannot smuggle a tree in"
        );
        assert!(!snap.dirs.contains_key("Elsewhere"));
    }

    #[test]
    fn nesting_stops_at_one_level() {
        let dir = scratch("deep");
        std::fs::create_dir_all(dir.join("Hollowward/props")).unwrap();
        write_png(&dir.join("Hollowward"), "shallow.png", 0);
        write_png(&dir.join("Hollowward/props"), "deep.png", 0);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["shallow.png"], "one level only, no recursion");
    }

    #[test]
    fn an_id_cannot_be_opened_against_another_category() {
        let dir = scratch("crosscat");
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        std::fs::create_dir(dir.join("Vanguard")).unwrap();
        // Same file NAME in both projects: only the category descriptor
        // keeps them apart.
        write_png(&dir.join("Hollowward"), "hero.png", 1);
        write_png(&dir.join("Vanguard"), "hero.png", 2);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let hollow = snap
            .entries
            .iter()
            .find(|e| e.category == "Hollowward")
            .unwrap();
        let van = snap
            .entries
            .iter()
            .find(|e| e.category == "Vanguard")
            .unwrap();
        assert_ne!(hollow.id, van.id);
        // Each id serves its OWN bytes; the sizes differ, so a descriptor
        // mix-up would show up as the wrong length.
        let a = snap.open_verified(&hollow.id, &hollow.revision).unwrap();
        let b = snap.open_verified(&van.id, &van.revision).unwrap();
        assert_ne!(a.len, b.len);
        assert_eq!(a.len, hollow.bytes);
        assert_eq!(b.len, van.bytes);
        // And an id never answers to the other's revision.
        assert!(snap.open_verified(&hollow.id, &van.revision).is_none());
    }

    #[test]
    fn a_file_moved_between_projects_fails_closed_on_its_old_id() {
        let dir = scratch("moved");
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        std::fs::create_dir(dir.join("Vanguard")).unwrap();
        write_png(&dir.join("Hollowward"), "hero.png", 3);
        let store = PreviewStore::new(Some(dir.clone()));
        let first = store.snapshot();
        let old = first.entries[0].clone();
        assert_eq!(old.category, "Hollowward");
        std::fs::rename(
            dir.join("Hollowward/hero.png"),
            dir.join("Vanguard/hero.png"),
        )
        .unwrap();
        store.set_dir(Some(dir)); // swap clears the 2s snapshot cache
        let second = store.snapshot();
        assert_eq!(second.entries[0].category, "Vanguard");
        assert_ne!(second.entries[0].id, old.id, "a move re-files the preview");
        assert!(second.open_verified(&old.id, &old.revision).is_none());
    }

    #[test]
    fn one_busy_project_cannot_starve_another() {
        let dir = scratch("starve");
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        std::fs::create_dir(dir.join("Vanguard")).unwrap();
        for i in 0..(MAX_ENTRIES + 20) {
            write_png(&dir.join("Hollowward"), &format!("h{i:03}.png"), i);
        }
        write_png(&dir.join("Vanguard"), "lonely.png", 1);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let hollow = snap
            .entries
            .iter()
            .filter(|e| e.category == "Hollowward")
            .count();
        let van = snap
            .entries
            .iter()
            .filter(|e| e.category == "Vanguard")
            .count();
        assert_eq!(hollow, MAX_ENTRIES, "capped per category");
        assert_eq!(van, 1, "and the quiet project survives the burst");
    }

    #[test]
    fn categories_are_capped_deterministically() {
        let dir = scratch("manycats");
        for i in 0..(MAX_CATEGORIES + 4) {
            let name = format!("proj{i:02}");
            std::fs::create_dir(dir.join(&name)).unwrap();
            write_png(&dir.join(&name), "a.png", i);
        }
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let mut cats: Vec<&str> = snap.entries.iter().map(|e| e.category.as_str()).collect();
        cats.sort();
        cats.dedup();
        assert_eq!(cats.len(), MAX_CATEGORIES);
        assert_eq!(cats[0], "proj00", "sorted, not readdir order");
    }

    #[test]
    fn a_real_folder_named_unsorted_keeps_its_images() {
        // The root bucket's DISPLAY name must not squat on a folder someone
        // actually made; its images vanished silently before.
        let dir = scratch("unsortedfolder");
        write_png(&dir, "loose.png", 1);
        std::fs::create_dir(dir.join(UNSORTED)).unwrap();
        write_png(&dir.join(UNSORTED), "inside.png", 2);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let mut names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["inside.png", "loose.png"], "no images are dropped");
        // Both display as Unsorted, but each opens through its own
        // descriptor.
        for entry in &snap.entries {
            assert_eq!(entry.category, UNSORTED);
            assert!(snap.open_verified(&entry.id, &entry.revision).is_some());
        }
    }

    #[test]
    fn stat_at_refuses_symlinks_and_reads_through_the_descriptor() {
        // Enumeration stats children RELATIVE to the pinned descriptor: a
        // directory swapped for a symlink after it is opened cannot feed
        // foreign names, sizes or mtimes into the listing.
        let dir = scratch("statat");
        write_png(&dir, "real.png", 7);
        std::os::unix::fs::symlink(dir.join("real.png"), dir.join("link.png")).unwrap();
        let handle = std::fs::File::open(&dir).unwrap();
        let real = stat_at(&handle, "real.png").expect("a regular file stats");
        assert_eq!(
            real.len(),
            std::fs::metadata(dir.join("real.png")).unwrap().len()
        );
        assert!(
            stat_at(&handle, "link.png").is_none(),
            "symlinks are refused"
        );
        assert!(
            stat_at(&handle, "missing.png").is_none(),
            "absent names are refused"
        );
    }

    #[test]
    fn enumeration_never_trusts_the_listed_path() {
        // The swap, made deterministic: the descriptor stays pinned to the
        // real folder while the PATH now resolves elsewhere. Only names that
        // really live in the pinned directory may produce entries, and their
        // metadata must come from there too.
        let real = scratch("swap-real");
        let fake = scratch("swap-fake");
        write_png(&real, "shared.png", 1);
        write_png(&fake, "shared.png", 999); // same name, different size
        write_png(&fake, "foreign.png", 5);
        let handle = std::fs::File::open(&real).unwrap();
        let mut found = Vec::new();
        collect_images(&handle, &fake, "Hollowward", "Hollowward", &mut found);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["shared.png"],
            "a foreign name cannot enter the catalog"
        );
        assert_eq!(
            found[0].meta.len(),
            std::fs::metadata(real.join("shared.png")).unwrap().len(),
            "and its size comes from the pinned directory, not the swapped path"
        );
    }

    #[test]
    fn loose_files_cannot_hide_a_project() {
        // One readdir budget shared by files and folders means a big pile of
        // loose renders can push every project folder out of discovery --
        // exactly the starvation the per-bucket cap exists to prevent.
        // Many projects behind a large pile of loose files: with one shared
        // budget only the handful that land in the readdir window survive,
        // so asserting on ALL of them is not a coin flip.
        let dir = scratch("starvecats");
        for i in 0..(MAX_SCAN * 8) {
            write_png(&dir, &format!("loose{i:04}.png"), 0);
        }
        for i in 0..MAX_CATEGORIES {
            let name = format!("proj{i:02}");
            std::fs::create_dir(dir.join(&name)).unwrap();
            write_png(&dir.join(&name), "hero.png", i);
        }
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let mut found: Vec<&str> = snap
            .entries
            .iter()
            .map(|e| e.category.as_str())
            .filter(|c| *c != UNSORTED)
            .collect();
        found.sort();
        found.dedup();
        assert_eq!(
            found.len(),
            MAX_CATEGORIES,
            "every project must stay discoverable behind any number of loose files"
        );
    }

    #[test]
    fn a_fifo_named_like_an_image_cannot_stall_the_scan() {
        // openat(O_RDONLY) on a FIFO blocks until a writer appears -- and the
        // scan holds the store mutex, so this would wedge the gallery, later
        // settings updates, and shutdown.
        // Inside a CATEGORY: the root pass still lstats first, but category
        // listing goes straight to stat_at.
        let dir = scratch("fifo");
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        write_png(&dir.join("Hollowward"), "real.png", 1);
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("Hollowward/trap.png"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "test needs a fifo");
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["real.png"], "the fifo is skipped, not opened");
    }

    #[test]
    fn a_flat_gallery_is_all_unsorted() {
        let dir = scratch("flat");
        write_png(&dir, "a.png", 1);
        write_png(&dir, "b.png", 2);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        assert_eq!(snap.entries.len(), 2);
        assert!(snap.entries.iter().all(|e| e.category == UNSORTED));
    }

    #[test]
    fn replacing_a_file_changes_its_revision() {
        let dir = scratch("rev");
        write_png(&dir, "a.png", 10);
        let store = PreviewStore::new(Some(dir.clone()));
        let first = store.snapshot().entries[0].revision.clone();
        write_png(&dir, "a.png", 999); // different size => different revision
        store.set_dir(Some(dir)); // swap clears the 2s snapshot cache
        let second = store.snapshot().entries[0].revision.clone();
        assert_ne!(first, second);
    }

    #[test]
    fn scan_keeps_newest_64_of_at_most_512() {
        let dir = scratch("bounds");
        for i in 0..70 {
            write_png(&dir, &format!("f{i:03}.png"), i);
        }
        let snap = PreviewStore::new(Some(dir)).snapshot();
        assert_eq!(snap.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn open_verified_serves_only_matching_id_and_revision() {
        let dir = scratch("open");
        write_png(&dir, "a.png", 5);
        let store = PreviewStore::new(Some(dir));
        let snap = store.snapshot();
        let entry = &snap.entries[0];
        let ok = snap.open_verified(&entry.id, &entry.revision).unwrap();
        assert_eq!(ok.content_type, "image/png");
        assert_eq!(ok.len, entry.bytes);
        assert!(snap.open_verified(&entry.id, "0000000000000000").is_none());
        assert!(snap.open_verified("p999999", &entry.revision).is_none());
    }

    #[test]
    fn replaced_file_fails_closed_on_old_snapshot() {
        let dir = scratch("toctou");
        write_png(&dir, "a.png", 5);
        let store = PreviewStore::new(Some(dir.clone()));
        let snap = store.snapshot();
        let entry = snap.entries[0].clone();
        std::fs::remove_file(dir.join("a.png")).unwrap();
        write_png(&dir, "a.png", 999); // new inode/size under the same name
        assert!(
            snap.open_verified(&entry.id, &entry.revision).is_none(),
            "identity re-verification must reject the swapped file"
        );
    }

    #[test]
    fn magic_byte_mismatch_is_refused() {
        let dir = scratch("magic");
        std::fs::write(dir.join("fake.png"), b"MZ not a png at all........").unwrap();
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let entry = &snap.entries[0]; // extension prefilter lists it...
        assert!(
            snap.open_verified(&entry.id, &entry.revision).is_none(),
            "...but the magic sniff refuses the bytes"
        );
    }

    #[test]
    fn sniffer_knows_the_four_formats() {
        assert_eq!(
            sniff_content_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]),
            Some("image/png")
        );
        assert_eq!(
            sniff_content_type(&[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_content_type(b"GIF89a\0\0\0\0\0\0"), Some("image/gif"));
        assert_eq!(sniff_content_type(b"RIFF\0\0\0\0WEBP"), Some("image/webp"));
        assert_eq!(sniff_content_type(b"MZ\0\0\0\0\0\0\0\0\0\0"), None);
    }

    #[test]
    fn same_size_in_place_rewrite_changes_revision() {
        let dir = scratch("nanos");
        write_png(&dir, "a.png", 16);
        let store = PreviewStore::new(Some(dir.clone()));
        let first = store.snapshot().entries[0].revision.clone();
        // Same byte count, different content, mtime bumped by one
        // millisecond — a whole-seconds revision would collide here.
        write_png(&dir, "a.png", 16);
        set_mtime(
            &dir.join("a.png"),
            std::time::SystemTime::now() + std::time::Duration::from_millis(1),
        );
        store.set_dir(Some(dir));
        let second = store.snapshot().entries[0].revision.clone();
        assert_ne!(first, second, "nanosecond mtime must move the revision");
    }

    #[test]
    fn ids_are_stable_across_rescans() {
        let dir = scratch("stable");
        write_png(&dir, "keep.png", 4);
        let store = PreviewStore::new(Some(dir.clone()));
        let first = store.snapshot().entries[0].id.clone();
        store.set_dir(Some(dir.clone())); // force a rescan
        write_png(&dir, "later.png", 8);
        let snap = store.snapshot();
        let keep = snap.entries.iter().find(|e| e.name == "keep.png").unwrap();
        let later = snap.entries.iter().find(|e| e.name == "later.png").unwrap();
        assert_eq!(keep.id, first, "unchanged files keep their id");
        assert_ne!(later.id, first);
    }

    #[test]
    fn open_verified_rejects_in_place_mtime_change() {
        let dir = scratch("mtime");
        write_png(&dir, "a.png", 4);
        let store = PreviewStore::new(Some(dir.clone()));
        let snap = store.snapshot();
        let entry = snap.entries[0].clone();
        // Same inode, same size, newer mtime: the old snapshot's identity
        // no longer matches the bytes on disk.
        set_mtime(
            &dir.join("a.png"),
            std::time::SystemTime::now() + std::time::Duration::from_secs(2),
        );
        assert!(
            snap.open_verified(&entry.id, &entry.revision).is_none(),
            "stale mtime must fail closed"
        );
    }

    #[test]
    fn viewport_frames_ride_a_synthetic_entry_with_moving_revisions() {
        let store = PreviewStore::new(None);
        assert!(store.viewport_entry().is_none(), "no frame yet");
        store.set_viewport_frame(vec![1, 2, 3]);
        let first = store.viewport_entry().expect("fresh frame is listed");
        assert_eq!(first.id, VIEWPORT_ID);
        assert_eq!(first.bytes, 3);
        assert_eq!(
            store.viewport_frame(&first.revision).map(|b| b.to_vec()),
            Some(vec![1, 2, 3])
        );
        assert!(
            store.viewport_frame("v999999").is_none(),
            "stale revisions 404"
        );
        store.set_viewport_frame(vec![4, 5]);
        let second = store.viewport_entry().unwrap();
        assert_ne!(
            first.revision, second.revision,
            "each frame is a new revision"
        );
        // The gallery entry appears even when the folder is unavailable —
        // the live tile does not depend on the watched dir.
        assert!(!store.snapshot().available);
    }

    #[test]
    fn viewport_freshness_window_expires_stale_frames() {
        assert!(viewport_fresh(Duration::from_secs(2)));
        assert!(!viewport_fresh(Duration::from_millis(
            VIEWPORT_FRESH_MS + 1
        )));
    }

    #[test]
    fn viewport_capture_is_demand_and_toggle_gated() {
        let store = PreviewStore::new(None);
        assert!(!store.viewport_wanted(), "off by default, no demand yet");
        store.set_viewport_enabled(true);
        assert!(!store.viewport_wanted(), "enabled but nobody is looking");
        let _ = store.snapshot(); // a phone listed the gallery
        assert!(store.viewport_wanted());
        store.set_viewport_enabled(false);
        assert!(!store.viewport_wanted(), "toggle wins over demand");
    }

    #[test]
    fn hard_links_get_distinct_ids_and_entries() {
        let dir = scratch("hardlink");
        write_png(&dir, "a.png", 4);
        std::fs::hard_link(dir.join("a.png"), dir.join("b.png")).unwrap();
        let snap = PreviewStore::new(Some(dir)).snapshot();
        assert_eq!(snap.entries.len(), 2, "both names are listed");
        assert_ne!(
            snap.entries[0].id, snap.entries[1].id,
            "same inode under two names must not collide on one id"
        );
        assert_eq!(snap.by_id.len(), 2, "both identities are addressable");
    }

    #[test]
    fn snapshot_is_cached_briefly() {
        let dir = scratch("cache");
        write_png(&dir, "a.png", 0);
        let store = PreviewStore::new(Some(dir.clone()));
        let first = store.snapshot();
        write_png(&dir, "b.png", 0);
        let second = store.snapshot();
        assert_eq!(
            first.entries.len(),
            second.entries.len(),
            "within the TTL the snapshot must not rescan"
        );
    }

    #[test]
    fn one_level_subfolders_become_categories() {
        let dir = scratch("cats");
        write_png(&dir, "loose.png", 1);
        std::fs::create_dir(dir.join("Hollowward")).unwrap();
        write_png(&dir.join("Hollowward"), "b1_lair.png", 2);
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let mut seen: Vec<(&str, &str)> = snap
            .entries
            .iter()
            .map(|e| (e.category.as_str(), e.name.as_str()))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            [("Hollowward", "b1_lair.png"), (UNSORTED, "loose.png")]
        );
    }
}
