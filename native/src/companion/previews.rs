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

const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

#[derive(Debug, Clone)]
pub struct PreviewEntry {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub modified_at: u64,
    pub bytes: u64,
}

pub(crate) struct FileIdentity {
    pub name: String,
    pub dev: u64,
    pub inode: u64,
    pub size: u64,
    pub revision: String,
}

pub struct CatalogSnapshot {
    pub available: bool,
    pub entries: Vec<PreviewEntry>,
    /// Held open across the snapshot's lifetime: `open_verified` opens ids
    /// RELATIVE to this descriptor, so swapping the watched dir on disk
    /// cannot redirect an id to a new tree.
    pub(crate) dir_handle: Option<std::fs::File>,
    pub(crate) dir_path: Option<PathBuf>,
    pub(crate) by_id: HashMap<String, FileIdentity>,
}

struct Inner {
    dir: Option<PathBuf>,
    /// Monotonic per-store id counter; ids stay opaque and never repeat.
    next_id: u64,
    cached: Option<(Instant, Arc<CatalogSnapshot>)>,
}

pub struct PreviewStore {
    inner: Mutex<Inner>,
}

/// FNV-1a over the identity tuple: replacement (new inode/size/mtime) is a
/// new revision, so caches can never serve stale bytes for an id.
fn revision_of(dev: u64, inode: u64, size: u64, mtime_secs: u64) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for value in [dev, inode, size, mtime_secs] {
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl PreviewStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                dir,
                next_id: 1,
                cached: None,
            }),
        }
    }

    /// Swap the watched folder atomically; the next snapshot() rescans.
    pub fn set_dir(&self, dir: Option<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        inner.dir = dir;
        inner.cached = None;
    }

    pub fn snapshot(&self) -> Arc<CatalogSnapshot> {
        let mut inner = self.inner.lock().unwrap();
        if let Some((at, snap)) = &inner.cached {
            if at.elapsed() < Duration::from_millis(SNAPSHOT_TTL_MS) {
                return Arc::clone(snap);
            }
        }
        let dir = inner.dir.clone();
        let snap = Arc::new(scan(dir, &mut inner.next_id));
        inner.cached = Some((Instant::now(), Arc::clone(&snap)));
        snap
    }
}

fn unavailable() -> CatalogSnapshot {
    CatalogSnapshot {
        available: false,
        entries: Vec::new(),
        dir_handle: None,
        dir_path: None,
        by_id: HashMap::new(),
    }
}

fn scan(dir: Option<PathBuf>, next_id: &mut u64) -> CatalogSnapshot {
    let Some(dir) = dir else { return unavailable() };
    let Ok(handle) = std::fs::File::open(&dir) else {
        return unavailable();
    };
    if !handle.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return unavailable();
    }
    let Ok(read) = std::fs::read_dir(&dir) else {
        return unavailable();
    };
    let mut found: Vec<(u64, String, std::fs::Metadata)> = Vec::new();
    for entry in read.take(MAX_SCAN).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let lower = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !EXTENSIONS.contains(&lower.as_str()) {
            continue;
        }
        // lstat: symlinks and non-regular files are refused outright.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        found.push((mtime_secs(&meta), name.to_string(), meta));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    found.truncate(MAX_ENTRIES);
    let mut entries = Vec::with_capacity(found.len());
    let mut by_id = HashMap::with_capacity(found.len());
    for (mtime, name, meta) in found {
        let id = format!("p{next_id}");
        *next_id += 1;
        let revision = revision_of(meta.dev(), meta.ino(), meta.len(), mtime);
        entries.push(PreviewEntry {
            id: id.clone(),
            name: name.clone(),
            revision: revision.clone(),
            modified_at: mtime,
            bytes: meta.len(),
        });
        by_id.insert(
            id,
            FileIdentity {
                name,
                dev: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                revision,
            },
        );
    }
    CatalogSnapshot {
        available: true,
        entries,
        dir_handle: Some(handle),
        dir_path: Some(dir),
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
    /// the held dirfd, O_NOFOLLOW, then fstat re-verification of regular
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
        let dir = self.dir_handle.as_ref()?;
        let name = std::ffi::CString::new(identity.name.as_str()).ok()?;
        // SAFETY: name is a valid NUL-terminated C string; the fd is owned
        // by the File we construct immediately on success.
        let fd = unsafe {
            openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                O_RDONLY | O_NOFOLLOW | O_CLOEXEC,
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

    /// Absolute source path for the thumbnailer (sips reads a path; the
    /// name was verified this scan and the revision key bounds staleness).
    pub fn source_path(&self, id: &str) -> Option<PathBuf> {
        let identity = self.by_id.get(id)?;
        Some(self.dir_path.clone()?.join(&identity.name))
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
        assert!(snap.entries[0].id.starts_with('p'), "{}", snap.entries[0].id);
        assert_ne!(snap.entries[0].id, snap.entries[1].id);
    }

    #[test]
    fn symlinks_and_subdirs_are_ignored() {
        let dir = scratch("links");
        write_png(&dir, "real.png", 0);
        std::fs::create_dir(dir.join("sub")).unwrap();
        write_png(&dir.join("sub"), "nested.png", 0);
        std::os::unix::fs::symlink(dir.join("real.png"), dir.join("link.png")).unwrap();
        let snap = PreviewStore::new(Some(dir)).snapshot();
        let names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["real.png"], "no symlink, no recursion");
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
}
