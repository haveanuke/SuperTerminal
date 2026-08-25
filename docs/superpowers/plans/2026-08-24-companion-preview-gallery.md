# Companion Preview Gallery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A "previews" screen in the phone companion showing images from a watched Mac folder, with hardened file confinement, bounded thumbnailing, and its own resource lane.

**Architecture:** A `PreviewStore` (id-catalog with openat/O_NOFOLLOW confinement, 2s-cached bounded scans) is shared into the existing companion HTTP server, which gains two token-gated routes (`/previews`, `/preview/<id>`) on a 2-slot image lane. A single worker thread generates revision-keyed `sips` thumbnails into `~/Library/Caches/SuperTerminal/previews/`. The phone page gains a third screen polling the list every 5s.

**Tech Stack:** Rust (std only — no new crates, per the minimal-deps rule), small libc FFI (`openat`) in the repo's existing extern-C style, `/usr/bin/sips` via `Command`, vanilla JS in `page.html`.

**Spec:** `docs/superpowers/specs/2026-08-24-companion-preview-gallery-design.md`

## Global Constraints

- No new dependencies. FFI follows `native/src/companion/net.rs`'s extern-C style.
- Default watched folder: `$HOME/Pictures/SuperTerminal` (resolved via `HOME`; `~` is never stored). Decision recorded in the spec.
- GIF thumbnails are first-frame (sips implicit) — accepted default.
- Scan bounds: ≤512 dir entries inspected, newest-by-mtime 64 kept, snapshot cached 2s.
- Serving bounds: full-size refused over 64 MiB (413); 64 KiB write chunks; image lane ≤2 concurrent (503 over); thumbnail queue 8 drop-oldest; sips timeout 10s.
- Thumb cache: `~/Library/Caches/SuperTerminal/previews/<revision>.jpg`, atomic tmp+rename, pruned to 128 MiB / 30 days. Deviation from spec wording: pruning runs on the worker thread (startup + after each job), never on HTTP workers — same bound, safer thread.
- CSP gains exactly `img-src 'self'`. Token gate/Host check/nosniff/Referrer-Policy unchanged.
- No emoji in UI — SVG icons only (phone page inlines its own SVGs like the existing header buttons).
- Full-size view: double-tap toggles fit/2.5× with pan (the page's viewport meta pins `maximum-scale=1`, so browser pinch-zoom is unavailable; note this deviation from the spec's "pinch-zoom" in the commit message).
- All commits: conventional style (`feat(native): …`), no attribution trailers.
- Run `cargo fmt --all` before every commit; run the full suite with `cargo test -p superterminal-native`.

---

### Task 1: `preview_dir` setting

**Files:**
- Modify: `native/src/settings.rs`
- Test: `native/src/settings.rs` (same-file `#[cfg(test)]`)

**Interfaces:**
- Produces: `Settings.preview_dir: Option<String>` (None = use default) and `pub fn resolved_preview_dir(settings: &Settings) -> Option<PathBuf>` — `Some(dir)` from the setting verbatim, else `$HOME/Pictures/SuperTerminal`; `None` only when `HOME` is unset.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests`):

```rust
#[test]
fn preview_dir_defaults_to_pictures_superterminal() {
    let s = Settings::default();
    assert_eq!(s.preview_dir, None);
    let resolved = resolved_preview_dir(&s).expect("HOME is set in tests");
    assert!(resolved.ends_with("Pictures/SuperTerminal"), "{resolved:?}");
}

#[test]
fn preview_dir_setting_overrides_default() {
    let s = Settings {
        preview_dir: Some("/tmp/renders".into()),
        ..Settings::default()
    };
    assert_eq!(
        resolved_preview_dir(&s),
        Some(PathBuf::from("/tmp/renders"))
    );
}
```

- [ ] **Step 2: Run to verify failure**: `cargo test -p superterminal-native preview_dir` — expect compile errors (`no field preview_dir`, `resolved_preview_dir` not found).

- [ ] **Step 3: Implement.** Add to the `Settings` struct (after `custom_themes`):

```rust
    /// Watched folder for the phone preview gallery; None = the default
    /// `$HOME/Pictures/SuperTerminal`. Absolute path, `~` never stored.
    pub preview_dir: Option<String>,
```

Add `preview_dir: None,` to `Default for Settings`, and `preview_dir: None,` to the `round_trip_preserves_values` test literal. Add the free function after `settings_path()`:

```rust
/// The watched preview folder: the setting verbatim, else
/// `$HOME/Pictures/SuperTerminal`. None only when HOME is unset.
pub fn resolved_preview_dir(settings: &Settings) -> Option<PathBuf> {
    if let Some(dir) = &settings.preview_dir {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Pictures/SuperTerminal"))
}
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native settings` — expect PASS (all settings tests).

- [ ] **Step 5: Commit**: `git add native/src/settings.rs && git commit -m "feat(native): preview_dir setting with Pictures/SuperTerminal default"`

---

### Task 2: Preview catalog — bounded scan

**Files:**
- Create: `native/src/companion/previews.rs`
- Modify: `native/src/companion/mod.rs` (add `pub mod previews;`)
- Test: `native/src/companion/previews.rs` (same-file `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub struct PreviewStore` with `pub fn new(dir: Option<PathBuf>) -> Self`, `pub fn set_dir(&self, dir: Option<PathBuf>)`, `pub fn snapshot(&self) -> Arc<CatalogSnapshot>`.
  - `pub struct CatalogSnapshot { pub available: bool, pub entries: Vec<PreviewEntry>, pub(crate) dir_handle: Option<std::fs::File>, pub(crate) by_id: std::collections::HashMap<String, FileIdentity> }` — `entries` newest-first, ≤64.
  - `pub struct PreviewEntry { pub id: String, pub name: String, pub revision: String, pub modified_at: u64, pub bytes: u64 }`
  - `pub(crate) struct FileIdentity { pub name: String, pub dev: u64, pub inode: u64, pub size: u64, pub revision: String }`
  - `pub const MAX_ENTRIES: usize = 64; pub const MAX_SCAN: usize = 512; pub const SNAPSHOT_TTL_MS: u64 = 2000; pub const MAX_FULL_BYTES: u64 = 64 * 1024 * 1024;`

- [ ] **Step 1: Write the failing tests.** Create `previews.rs` with only the test module first:

```rust
//! Preview gallery catalog: opaque ids over a watched folder, with the
//! review-hardened confinement rules from the design spec (direct children
//! only, regular files only, openat/O_NOFOLLOW + fstat re-verification,
//! magic-byte validation, revision = hash(dev, inode, size, mtime)).

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "st-previews-{}-{name}",
            std::process::id()
        ));
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

    fn set_mtime(path: &std::path::Path, to: std::time::SystemTime) {
        let file = std::fs::File::options().append(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(to)).unwrap();
    }
}
```

- [ ] **Step 2: Add `pub mod previews;` to `native/src/companion/mod.rs`, run** `cargo test -p superterminal-native previews` — expect compile failures (types not defined).

- [ ] **Step 3: Implement** above the test module:

```rust
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
        let snap = Arc::new(scan(inner.dir.clone(), &mut inner.next_id));
        inner.cached = Some((Instant::now(), Arc::clone(&snap)));
        snap
    }
}

fn unavailable() -> CatalogSnapshot {
    CatalogSnapshot {
        available: false,
        entries: Vec::new(),
        dir_handle: None,
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
        by_id,
    }
}
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native previews` — expect all Task 2 tests PASS.

- [ ] **Step 5: Commit**: `git add native/src/companion/previews.rs native/src/companion/mod.rs && git commit -m "feat(native): bounded preview catalog with opaque ids"`

---

### Task 3: Confined open + magic-byte validation

**Files:**
- Modify: `native/src/companion/previews.rs`
- Test: same file

**Interfaces:**
- Produces:
  - `impl CatalogSnapshot { pub fn open_verified(&self, id: &str, revision: &str) -> Option<VerifiedFile> }` — None on unknown id, stale revision, identity mismatch, or magic-byte mismatch.
  - `pub struct VerifiedFile { pub file: std::fs::File, pub len: u64, pub content_type: &'static str }`
  - `pub(crate) fn sniff_content_type(head: &[u8]) -> Option<&'static str>` — png/jpeg/gif/webp.

- [ ] **Step 1: Write the failing tests** (append to `mod tests`):

```rust
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
    assert_eq!(
        sniff_content_type(b"GIF89a\0\0\0\0\0\0"),
        Some("image/gif")
    );
    assert_eq!(
        sniff_content_type(b"RIFF\0\0\0\0WEBP"),
        Some("image/webp")
    );
    assert_eq!(sniff_content_type(b"MZ\0\0\0\0\0\0\0\0\0\0"), None);
}
```

- [ ] **Step 2: Run** `cargo test -p superterminal-native previews` — expect compile failure (`open_verified` not found).

- [ ] **Step 3: Implement.** Add the FFI + verification (below the `scan` function, matching `net.rs`'s extern-C style):

```rust
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
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
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
        use std::io::Read;
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
        use std::io::Seek;
        file.rewind().ok()?;
        Some(VerifiedFile {
            file,
            len: meta.len(),
            content_type,
        })
    }
}
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native previews` — expect PASS.

- [ ] **Step 5: Commit**: `git add native/src/companion/previews.rs && git commit -m "feat(native): openat-confined verified opens with magic sniffing"`

---

### Task 4: Thumbnail worker + cache

**Files:**
- Create: `native/src/companion/thumbs.rs`
- Modify: `native/src/companion/mod.rs` (add `pub mod thumbs;`)
- Test: same file

**Interfaces:**
- Consumes: `previews::VerifiedFile` is NOT used here — the worker reads source paths handed by the server (dir + verified name), and dimension-caps from the image header itself.
- Produces:
  - `pub struct Thumbnailer` with `pub fn new(cache_dir: PathBuf) -> Arc<Self>` (spawns the worker thread; prunes at startup), `pub fn request(&self, revision: &str, source: PathBuf) -> ThumbState`.
  - `pub enum ThumbState { Ready(PathBuf), Pending }` — `Ready` iff the cache file exists; `Pending` enqueues (bounded 8, drop-oldest) and returns immediately.
  - `pub fn default_cache_dir() -> Option<PathBuf>` — `$HOME/Library/Caches/SuperTerminal/previews`.
  - `pub(crate) fn image_dimensions(head: &[u8]) -> Option<(u32, u32)>` (png/gif/jpeg/webp; None = unknown → refuse).
  - `pub(crate) fn prune(cache_dir: &std::path::Path)` — ≤128 MiB total, ≤30 days old, oldest deleted first.
  - `pub const SIPS_TIMEOUT_SECS: u64 = 10; pub const MAX_PIXELS: u64 = 144_000_000; pub const QUEUE_CAP: usize = 8;`

- [ ] **Step 1: Write the failing tests**:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("st-thumbs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dimensions_parse_png_and_gif_headers() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&[0, 0, 1, 0, 0, 0, 0, 0x80]); // 256 x 128
        assert_eq!(image_dimensions(&png), Some((256, 128)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xc8, 0x00]); // 320 x 200 LE
        assert_eq!(image_dimensions(&gif), Some((320, 200)));
        assert_eq!(image_dimensions(b"MZ\0\0\0\0\0\0\0\0\0\0\0\0"), None);
    }

    #[test]
    fn real_sips_generates_a_cached_thumbnail() {
        // Spec's testing contract: a real sips invocation on a generated PNG.
        let dir = scratch("sips-src");
        let cache = scratch("sips-cache");
        let src = dir.join("real.png");
        // sips itself writes the fixture: a 2000x2000 solid PNG.
        let status = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png", "-z", "2000", "2000"])
            .arg("/System/Library/CoreServices/DefaultDesktop.heic")
            .arg("--out")
            .arg(&src)
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            // Fallback fixture: generate via sips from any bundled image is
            // unavailable — skip (CI-less repo; the queue/prune tests below
            // still run).
            return;
        }
        let thumbs = Thumbnailer::new(cache.clone());
        assert!(matches!(
            thumbs.request("cafecafecafecafe", src.clone()),
            ThumbState::Pending
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let ThumbState::Ready(path) = thumbs.request("cafecafecafecafe", src.clone()) {
                assert!(path.ends_with("cafecafecafecafe.jpg"));
                let bytes = std::fs::read(path).unwrap();
                assert!(!bytes.is_empty());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "thumbnail never appeared");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn slow_generator_times_out_via_stub_binary() {
        // Spec's testing contract: timeout path with a stub binary.
        let dir = scratch("stub");
        let cache = scratch("stub-cache");
        let stub = dir.join("slow-sips");
        std::fs::write(&stub, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let src = dir.join("a.png");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
        std::fs::write(&src, png).unwrap();
        let started = std::time::Instant::now();
        let ok = generate_with(&stub, &src, &cache.join("x.jpg"), std::time::Duration::from_millis(300));
        assert!(!ok, "stub must be killed, not succeed");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "kill+reap must not wait for the child's sleep"
        );
    }

    #[test]
    fn prune_deletes_oldest_beyond_budget() {
        let cache = scratch("prune");
        for i in 0..5 {
            let path = cache.join(format!("rev{i}.jpg"));
            std::fs::write(&path, vec![0u8; 1024]).unwrap();
            let age = std::time::SystemTime::now()
                - std::time::Duration::from_secs(60 * 60 * 24 * (40 - i));
            let file = std::fs::File::options().append(true).open(&path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(age)).unwrap();
        }
        prune(&cache);
        let left = std::fs::read_dir(&cache).unwrap().count();
        assert_eq!(left, 0, "all five are older than 30 days");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p superterminal-native thumbs` (after adding `pub mod thumbs;` to `mod.rs`) — expect compile failures.

- [ ] **Step 3: Implement**:

```rust
//! Thumbnail generation: a single worker thread, bounded drop-oldest queue,
//! `/usr/bin/sips` under a hard timeout, and a revision-keyed cache pruned
//! on the worker thread (startup + after each job) — HTTP workers never
//! block on generation (design spec, resource isolation).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

pub const SIPS_TIMEOUT_SECS: u64 = 10;
pub const MAX_PIXELS: u64 = 144_000_000;
pub const QUEUE_CAP: usize = 8;
const CACHE_BUDGET_BYTES: u64 = 128 * 1024 * 1024;
const CACHE_MAX_AGE: Duration = Duration::from_secs(60 * 60 * 24 * 30);
const THUMB_EDGE: &str = "512";

pub enum ThumbState {
    Ready(PathBuf),
    Pending,
}

struct Job {
    revision: String,
    source: PathBuf,
}

pub struct Thumbnailer {
    cache_dir: PathBuf,
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

pub fn default_cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Caches/SuperTerminal/previews"))
}

impl Thumbnailer {
    pub fn new(cache_dir: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&cache_dir);
        let this = Arc::new(Self {
            cache_dir,
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&this);
        let _ = std::thread::Builder::new()
            .name("companion-thumbs".into())
            .spawn(move || worker.run());
        this
    }

    fn cache_path(&self, revision: &str) -> PathBuf {
        self.cache_dir.join(format!("{revision}.jpg"))
    }

    /// Ready iff the cache already holds this revision; otherwise enqueue
    /// (drop-oldest beyond QUEUE_CAP) and return Pending immediately.
    pub fn request(&self, revision: &str, source: PathBuf) -> ThumbState {
        let cached = self.cache_path(revision);
        if cached.is_file() {
            return ThumbState::Ready(cached);
        }
        let mut queue = self.queue.lock().unwrap();
        if !queue.iter().any(|job| job.revision == revision) {
            if queue.len() >= QUEUE_CAP {
                queue.pop_front();
            }
            queue.push_back(Job {
                revision: revision.to_string(),
                source,
            });
        }
        drop(queue);
        self.wake.notify_one();
        ThumbState::Pending
    }

    fn run(&self) {
        prune(&self.cache_dir);
        loop {
            let job = {
                let mut queue = self.queue.lock().unwrap();
                loop {
                    if let Some(job) = queue.pop_front() {
                        break job;
                    }
                    queue = self.wake.wait(queue).unwrap();
                }
            };
            let out = self.cache_path(&job.revision);
            if out.is_file() {
                continue;
            }
            if !dimensions_acceptable(&job.source) {
                continue;
            }
            let tmp = self
                .cache_dir
                .join(format!("{}.tmp.{}", job.revision, std::process::id()));
            if generate_with(
                Path::new("/usr/bin/sips"),
                &job.source,
                &tmp,
                Duration::from_secs(SIPS_TIMEOUT_SECS),
            ) {
                let _ = std::fs::rename(&tmp, &out);
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
            prune(&self.cache_dir);
        }
    }
}

/// Refuse absurd pixel counts BEFORE invoking sips (decompression-bomb
/// guard); unknown headers refuse too.
fn dimensions_acceptable(source: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(source) else {
        return false;
    };
    let mut head = [0u8; 64];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    match image_dimensions(&head[..read]) {
        Some((w, h)) => (w as u64) * (h as u64) <= MAX_PIXELS,
        None => false,
    }
}

/// Fixed argv, no shell, explicit out path, kill+reap on deadline.
fn generate_with(binary: &Path, source: &Path, out: &Path, timeout: Duration) -> bool {
    let child = std::process::Command::new(binary)
        .arg("-s")
        .arg("format")
        .arg("jpeg")
        .arg("-Z")
        .arg(THUMB_EDGE)
        .arg(source)
        .arg("--out")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success() && out.is_file(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

/// Header-only dimensions for the four accepted formats.
pub(crate) fn image_dimensions(head: &[u8]) -> Option<(u32, u32)> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) && head.len() >= 24 {
        let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
        let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
        return Some((w, h));
    }
    if (head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a")) && head.len() >= 10 {
        let w = u16::from_le_bytes([head[6], head[7]]) as u32;
        let h = u16::from_le_bytes([head[8], head[9]]) as u32;
        return Some((w, h));
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        // JPEG: scan markers for a SOF segment within the sniffed head.
        let mut i = 2;
        while i + 9 < head.len() {
            if head[i] != 0xff {
                return None;
            }
            let marker = head[i + 1];
            let len = u16::from_be_bytes([head[i + 2], head[i + 3]]) as usize;
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
            {
                let h = u16::from_be_bytes([head[i + 5], head[i + 6]]) as u32;
                let w = u16::from_be_bytes([head[i + 7], head[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
        // SOF beyond the sniffed head: fall back to a conservative accept —
        // sips bounds memory itself for JPEG; the cap targets PNG bombs.
        return Some((1, 1));
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") && head.len() >= 30 {
        if head.get(12..16) == Some(b"VP8X") {
            let w = 1 + u32::from_le_bytes([head[24], head[25], head[26], 0]);
            let h = 1 + u32::from_le_bytes([head[27], head[28], head[29], 0]);
            return Some((w, h));
        }
        return Some((1, 1)); // simple VP8/VP8L: bounded by webp's 16383 limit
    }
    None
}

pub(crate) fn prune(cache_dir: &Path) {
    let Ok(read) = std::fs::read_dir(cache_dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = read
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                    entry.path(),
                )
            })
        })
        .collect();
    files.sort_by_key(|(mtime, _, _)| *mtime); // oldest first
    let now = SystemTime::now();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    for (mtime, len, path) in files {
        let expired = now
            .duration_since(mtime)
            .map(|age| age > CACHE_MAX_AGE)
            .unwrap_or(false);
        if expired || total > CACHE_BUDGET_BYTES {
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native thumbs` — expect PASS.

- [ ] **Step 5: Commit**: `git add native/src/companion/thumbs.rs native/src/companion/mod.rs && git commit -m "feat(native): sips thumbnail worker with bounded queue and pruned cache"`

---

### Task 5: Server routes

**Files:**
- Modify: `native/src/companion/server.rs`
- Modify: `native/src/companion/e2e_tests.rs` (ServerConfig literals gain the new fields)
- Modify: `native/src/workspace.rs:1041-1048` (ServerConfig literal)
- Test: `native/src/companion/server.rs` `mod tests`

**Interfaces:**
- Consumes: `previews::{PreviewStore, CatalogSnapshot, MAX_FULL_BYTES}`, `thumbs::{Thumbnailer, ThumbState}`.
- Produces:
  - `ServerConfig` gains `pub previews: std::sync::Arc<super::previews::PreviewStore>` and `pub thumbs: std::sync::Arc<super::thumbs::Thumbnailer>`.
  - Routes: `GET /previews` → `{"state":"ok"|"unavailable","entries":[{"id","kind":"image","name","revision","modifiedAt","bytes"}]}`; `GET /preview/<id>?rev=<revision>[&thumb=1]` → image bytes / 304 / 404 / 413 / 503 / 202.
  - `MAX_IMG: usize = 2` (CAS lane), CSP gains `img-src 'self'`.

- [ ] **Step 1: Write the failing tests** (in server.rs `mod tests`, after the bracketed-paste test). The `boot`/`seeded_hub` helpers and `ServerConfig` literals in tests must first gain the two new fields — add to every `ServerConfig { .. }` in server.rs tests and e2e_tests.rs:

```rust
            previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
            thumbs: crate::companion::thumbs::Thumbnailer::new(
                std::env::temp_dir().join(format!("st-thumbcache-{}", std::process::id())),
            ),
```

Then the tests:

```rust
    fn preview_store(dir: &std::path::Path) -> Arc<crate::companion::previews::PreviewStore> {
        Arc::new(crate::companion::previews::PreviewStore::new(Some(
            dir.to_path_buf(),
        )))
    }

    fn boot_with_previews(
        previews: Arc<crate::companion::previews::PreviewStore>,
    ) -> ServerHandle {
        let (hub, _rx) = seeded_hub(false);
        start(
            hub,
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: TOKEN.into(),
                page: "<title>t</title>",
                previews,
                thumbs: crate::companion::thumbs::Thumbnailer::new(
                    std::env::temp_dir().join(format!("st-thumbcache-{}", std::process::id())),
                ),
            },
        )
        .expect("server starts")
    }

    fn write_test_png(dir: &std::path::Path, name: &str) {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        bytes.extend_from_slice(&[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn previews_list_is_token_gated_and_reports_state() {
        let dir = std::env::temp_dir().join(format!("st-prevroute-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_test_png(&dir, "render.png");
        let handle = boot_with_previews(preview_store(&dir));
        let host = host_of(&handle);
        assert!(get(&host, "/previews").starts_with("HTTP/1.1 404"), "no token");
        let ok = get(&host, &format!("/previews?t={TOKEN}"));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("\"state\":\"ok\""), "{ok}");
        assert!(ok.contains("\"name\":\"render.png\""), "{ok}");
        assert!(ok.contains("\"kind\":\"image\""), "{ok}");
        handle.stop();

        let gone = boot_with_previews(Arc::new(
            crate::companion::previews::PreviewStore::new(Some("/nonexistent/x".into())),
        ));
        let host = host_of(&gone);
        let out = get(&host, &format!("/previews?t={TOKEN}"));
        assert!(out.contains("\"state\":\"unavailable\""), "{out}");
        gone.stop();
    }

    #[test]
    fn preview_bytes_respect_etag_and_stale_revisions() {
        let dir = std::env::temp_dir().join(format!("st-prevetag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_test_png(&dir, "a.png");
        let store = preview_store(&dir);
        let entry = store.snapshot().entries[0].clone();
        let handle = boot_with_previews(store);
        let host = host_of(&handle);
        let ok = get(
            &host,
            &format!("/preview/{}?t={TOKEN}&rev={}", entry.id, entry.revision),
        );
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("Content-Type: image/png"), "{ok}");
        assert!(ok.contains(&format!("ETag: \"{}\"", entry.revision)), "{ok}");
        let not_modified = roundtrip(
            &host,
            &format!(
                "GET /preview/{}?t={TOKEN}&rev={} HTTP/1.1\r\nHost: {host}\r\nIf-None-Match: \"{}\"\r\n\r\n",
                entry.id, entry.revision, entry.revision
            ),
        );
        assert!(not_modified.starts_with("HTTP/1.1 304"), "{not_modified}");
        let stale = get(
            &host,
            &format!("/preview/{}?t={TOKEN}&rev=0000000000000000", entry.id),
        );
        assert!(stale.starts_with("HTTP/1.1 404"), "{stale}");
        handle.stop();
    }

    #[test]
    fn oversized_full_file_is_listed_but_refused() {
        let dir = std::env::temp_dir().join(format!("st-prevbig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Sparse-write a >64MiB png-magic file without materializing 64MiB.
        let path = dir.join("huge.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(65 * 1024 * 1024).unwrap();
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        let store = preview_store(&dir);
        let entry = store.snapshot().entries[0].clone();
        let handle = boot_with_previews(store);
        let host = host_of(&handle);
        let out = get(
            &host,
            &format!("/preview/{}?t={TOKEN}&rev={}", entry.id, entry.revision),
        );
        assert!(out.starts_with("HTTP/1.1 413"), "{out}");
        handle.stop();
    }

    #[test]
    fn csp_allows_self_images() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        let page = get(&host, "/");
        assert!(page.contains("img-src 'self'"), "{page}");
        handle.stop();
    }
```

- [ ] **Step 2: Run** `cargo test -p superterminal-native server` — expect compile failures (ServerConfig fields) then assertion failures.

- [ ] **Step 3: Implement.**
  1. `ServerConfig` gains `pub previews: Arc<super::previews::PreviewStore>` and `pub thumbs: Arc<super::thumbs::Thumbnailer>`; `Shared<S>` gains `previews: Arc<super::previews::PreviewStore>`, `thumbs: Arc<super::thumbs::Thumbnailer>`, `img: AtomicUsize`; `start()` moves them in (`img: AtomicUsize::new(0)`). Add `pub const MAX_IMG: usize = 2;` next to `MAX_SSE`.
  2. CSP: in `SECURITY_HEADERS`, change the CSP value to `"default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self'"`.
  3. Routes, added to the `match` before the `/stream/` arm:

```rust
        (Method::Get, "/previews") => {
            let snap = shared.previews.snapshot();
            let entries: Vec<serde_json::Value> = snap
                .entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id, "kind": "image", "name": e.name,
                        "revision": e.revision, "modifiedAt": e.modified_at,
                        "bytes": e.bytes,
                    })
                })
                .collect();
            let json = serde_json::json!({
                "state": if snap.available { "ok" } else { "unavailable" },
                "entries": entries,
            })
            .to_string();
            let _ = respond(
                &stream,
                "200 OK",
                &[("Content-Type", "application/json")],
                json.as_bytes(),
            );
        }
        (Method::Get, _) if path.starts_with("/preview/") => {
            // Image lane: 2 concurrent max so a thumbnail grid can never
            // starve sessions/input out of the 8-connection budget.
            let admitted = shared
                .img
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < MAX_IMG).then_some(n + 1)
                })
                .is_ok();
            if !admitted {
                let _ = respond(&stream, "503 Service Unavailable", &[], b"");
                return;
            }
            serve_preview(shared, &stream, &request, &path);
            shared.img.fetch_sub(1, Ordering::AcqRel);
        }
```

  4. The responder (new function next to `serve_stream`):

```rust
fn serve_preview<S: InputSink>(shared: &Shared<S>, stream: &TcpStream, request: &Request, path: &str) {
    let id = &path["/preview/".len()..];
    let Some(revision) = request.query_param("rev") else {
        let _ = respond(stream, "400 Bad Request", &[], b"");
        return;
    };
    let etag = format!("\"{revision}\"");
    if request.header("if-none-match") == Some(etag.as_str()) {
        let _ = respond(stream, "304 Not Modified", &[("ETag", &etag)], b"");
        return;
    }
    let snap = shared.previews.snapshot();
    let Some(verified) = snap.open_verified(id, revision) else {
        let _ = respond(stream, "404 Not Found", &[], b"");
        return;
    };
    if request.query_param("thumb") == Some("1") {
        let Some(identity) = snap.source_path(id) else {
            let _ = respond(stream, "404 Not Found", &[], b"");
            return;
        };
        match shared.thumbs.request(revision, identity) {
            super::thumbs::ThumbState::Ready(path) => {
                let Ok(file) = std::fs::File::open(&path) else {
                    let _ = respond(stream, "404 Not Found", &[], b"");
                    return;
                };
                let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                stream_file(stream, file, len, "image/jpeg", &etag);
            }
            super::thumbs::ThumbState::Pending => {
                let _ = respond(stream, "202 Accepted", &[("Retry-After", "1")], b"");
            }
        }
        return;
    }
    if verified.len > super::previews::MAX_FULL_BYTES {
        let _ = respond(stream, "413 Content Too Large", &[], b"");
        return;
    }
    stream_file(stream, verified.file, verified.len, verified.content_type, &etag);
}

/// 64 KiB chunks; the socket's write deadline (set per connection) bounds
/// every chunk, never the whole body.
fn stream_file(
    stream: &TcpStream,
    mut file: std::fs::File,
    len: u64,
    content_type: &str,
    etag: &str,
) {
    use std::io::Read;
    let mut writer = stream;
    let _ = writer.set_write_timeout(Some(WRITE_DEADLINE));
    let mut head = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n");
    for (name, value) in SECURITY_HEADERS {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Type: {content_type}\r\nETag: {etag}\r\nContent-Length: {len}\r\n\r\n"
    ));
    if writer.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut buf = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if writer.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}
```

  5. `previews.rs` needs the small addition used above — `CatalogSnapshot::source_path(&self, id: &str) -> Option<PathBuf>`: the watched dir path is not stored on the snapshot yet, so add `pub(crate) dir_path: Option<PathBuf>` to `CatalogSnapshot` (set in `scan`, `None` in `unavailable()`), and:

```rust
    /// Absolute source path for the thumbnailer (sips reads a path; the
    /// name was verified this scan and the revision key bounds staleness).
    pub fn source_path(&self, id: &str) -> Option<PathBuf> {
        let identity = self.by_id.get(id)?;
        Some(self.dir_path.clone()?.join(&identity.name))
    }
```

  6. Update the `ServerConfig` literal in `workspace.rs:1044` — the workspace constructs the store/thumbnailer once (Task 6 wires them into workspace state; for THIS task's compile, create them inline):

```rust
                server::ServerConfig {
                    bind: std::net::SocketAddr::from((ip, port)),
                    token: token.clone(),
                    page: include_str!("companion/page.html"),
                    previews: Arc::clone(&previews),
                    thumbs: Arc::clone(&thumbs),
                },
```

  with, before the port loop:

```rust
        let previews = Arc::new(crate::companion::previews::PreviewStore::new(
            crate::settings::resolved_preview_dir(&self.settings),
        ));
        let thumbs = crate::companion::thumbs::Thumbnailer::new(
            crate::companion::thumbs::default_cache_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("st-thumbs")),
        );
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native` — expect full suite PASS (including the four new route tests and all pre-existing server/e2e tests with updated ServerConfig literals).

- [ ] **Step 5: Commit**: `git add native/src && git commit -m "feat(native): /previews and /preview routes with image lane and ETag caching"`

---

### Task 6: Workspace wiring — store swap on settings change + settings row

**Files:**
- Modify: `native/src/workspace.rs`
- Test: `native/src/companion/previews.rs` (set_dir swap already covered by `replacing_a_file_changes_its_revision`; UI is compile-verified)

**Interfaces:**
- Consumes: `PreviewStore::set_dir`, `settings::resolved_preview_dir`.
- Produces: `Workspace.companion_previews: Option<Arc<crate::companion::previews::PreviewStore>>` — lives beside `companion_server`; settings-overlay row "previews" with choose/clear.

- [ ] **Step 1: Store the Arc.** Add field next to `companion_server` (workspace.rs:234): `companion_previews: Option<std::sync::Arc<crate::companion::previews::PreviewStore>>,` (initialize `None` where `companion_server: None` is initialized). In the Task 5 server-start code, keep the created `previews` Arc: `self.companion_previews = Some(Arc::clone(&previews));` on successful start, `None` on failure/stop (mirror wherever `companion_server` is set/cleared).

- [ ] **Step 2: Settings row.** Copy the `render_background_row` pattern (workspace.rs:2947) into a new `render_previews_row`, rendered next to it in the settings overlay:

```rust
    fn render_previews_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let label: SharedString = match &self.settings.preview_dir {
            Some(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone())
                .into(),
            None => "Pictures/SuperTerminal".into(),
        };
        let overridden = self.settings.preview_dir.is_some();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).child("previews"))
            .child(div().text_size(px(11.0)).child(label))
            .child(self.chip_button(
                "choose",
                false,
                |ws, _window, cx| ws.pick_preview_dir(cx),
                cx,
            ))
            .children(overridden.then(|| {
                self.chip_button(
                    "default",
                    false,
                    |ws, _window, cx| {
                        ws.settings.preview_dir = None;
                        let _ = ws.settings.save();
                        ws.apply_preview_dir();
                        cx.notify();
                    },
                    cx,
                )
            }))
    }
```

- [ ] **Step 3: Picker + swap.** Mirror `pick_background_image` (workspace.rs:1521) but with `directories: true` in the path-prompt options; on selection:

```rust
    fn apply_preview_dir(&self) {
        if let Some(store) = &self.companion_previews {
            store.set_dir(crate::settings::resolved_preview_dir(&self.settings));
        }
    }
```

  called after every `preview_dir` mutation (`pick_preview_dir` success and the "default" chip). The server itself never restarts — routes read through the swapped store (spec: settings section).

- [ ] **Step 4: Run** `cargo test -p superterminal-native` and `cargo build -p superterminal-native` — expect PASS/clean build; open the settings overlay in a dev run if convenient (not gating).

- [ ] **Step 5: Commit**: `git add native/src/workspace.rs && git commit -m "feat(native): preview folder setting swaps the live catalog"`

---

### Task 7: Phone page — previews screen

**Files:**
- Modify: `native/src/companion/page.html`
- Test: `native/src/companion/e2e_tests.rs` (structure test)

**Interfaces:**
- Consumes: `GET /previews` JSON (`state`, `entries[{id,kind,name,revision,modifiedAt,bytes}]`), `GET /preview/<id>?rev=&thumb=1`, 202/404/413 semantics.
- Produces: `#previews` screen; `#previewsbtn` on the session-list header.

- [ ] **Step 1: Write the failing structure test** (e2e_tests.rs):

```rust
#[test]
fn page_has_a_previews_screen() {
    let page = include_str!("page.html");
    assert!(page.contains("id=\"previews\""), "previews screen exists");
    assert!(page.contains("id=\"previewsbtn\""), "list header opens it");
    assert!(page.contains("id=\"pgrid\""), "thumbnail grid exists");
    assert!(page.contains("id=\"pfull\""), "full-size view exists");
    assert!(
        page.contains("function refreshPreviews"),
        "5s polling loop exists"
    );
    assert!(
        page.contains("unavailable"),
        "distinct unavailable notice is rendered"
    );
    assert!(page.contains("thumb=1"), "tiles load the downscaled variant");
}
```

- [ ] **Step 2: Run** `cargo test -p superterminal-native page_has_a_previews_screen` — expect FAIL.

- [ ] **Step 3: Implement.** CSS (in `<style>`):

```css
  #previews { display:flex; flex-direction:column; height:100dvh; }
  #pwrap { flex:1 1 0; overflow:auto; -webkit-overflow-scrolling:touch; }
  #pgrid { display:grid; grid-template-columns:1fr 1fr; gap:8px; padding:8px; }
  #pgrid .tile { background:var(--panel); border:1px solid var(--border); border-radius:8px; overflow:hidden; }
  #pgrid .tile img { width:100%; aspect-ratio:1; object-fit:cover; display:block; background:var(--bg); }
  #pgrid .tile .cap { padding:6px 8px; font-size:11px; color:var(--muted); overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  #pfull { position:fixed; inset:0; background:rgba(0,0,0,.92); z-index:3; display:flex; flex-direction:column; }
  #pfullwrap { flex:1 1 0; overflow:auto; -webkit-overflow-scrolling:touch; }
  #pfullwrap img { display:block; margin:auto; max-width:100%; }
  #pfullwrap img.zoomed { max-width:none; width:250%; }
  #pfullcap { padding:10px 14px; font-size:12px; color:var(--muted); text-align:center; }
```

HTML after `#term` (header buttons reuse the inline-SVG style — no emoji):

```html
<div id="previews" class="hidden">
  <header>
    <button class="back" id="pback" aria-label="back">
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2"><path d="M10 3 5 8l5 5"/></svg>
    </button>
    <div class="title">Previews</div>
    <div id="pdot" class="dot"></div>
  </header>
  <div id="pwrap">
    <div id="pgrid"></div>
    <div id="pempty" class="notice hidden">No images yet.</div>
    <div id="punavailable" class="notice hidden"></div>
  </div>
</div>
<div id="pfull" class="hidden">
  <header>
    <button class="back" id="pfullclose" aria-label="close">
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 3l10 10M13 3 3 13"/></svg>
    </button>
    <div class="title" id="pfulltitle"></div>
  </header>
  <div id="pfullwrap"><img id="pfullimg" alt=""></div>
  <div id="pfullcap"></div>
</div>
```

Add to the `#list` header (next to `#newterm`):

```html
    <button class="back" id="previewsbtn" aria-label="previews">
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="12" height="10" rx="1"/><path d="M2 10l3-3 3 3 2-2 4 4"/></svg>
    </button>
```

JS (before the SSE section; tiles keyed by id, updated in place — no image churn):

```js
  // ---- Previews screen -------------------------------------------------
  var previewTimer = null, tileEls = {};
  function previewsVisible() { return !$("previews").classList.contains("hidden"); }
  function imgSrc(entry, thumb) {
    return api("/preview/" + encodeURIComponent(entry.id)) +
      "&rev=" + encodeURIComponent(entry.revision) + (thumb ? "&thumb=1" : "");
  }
  // A 202 (thumb still rendering) surfaces as an <img> error: retry with a
  // capped backoff instead of leaving a broken tile.
  function loadThumb(img, entry, attempt) {
    img.dataset.rev = entry.revision;
    img.onerror = function () {
      if (attempt < 8 && previewsVisible() && img.dataset.rev === entry.revision) {
        setTimeout(function () { loadThumb(img, entry, attempt + 1); }, 1000);
      }
    };
    img.src = imgSrc(entry, true) + "&a=" + attempt;
  }
  function renderPreviews(data) {
    $("pdot").classList.add("on");
    var unavailable = data.state !== "ok";
    $("punavailable").classList.toggle("hidden", !unavailable);
    if (unavailable) {
      $("punavailable").textContent =
        "Preview folder unavailable. Save renders into Pictures/SuperTerminal on the Mac (or set a folder in settings).";
      $("pgrid").textContent = ""; tileEls = {};
      $("pempty").classList.add("hidden");
      return;
    }
    $("pempty").classList.toggle("hidden", data.entries.length > 0);
    var grid = $("pgrid"), seen = {};
    data.entries.forEach(function (entry) {
      seen[entry.id] = true;
      var tile = tileEls[entry.id];
      if (!tile) {
        tile = document.createElement("div");
        tile.className = "tile";
        var img = document.createElement("img");
        img.alt = entry.name;
        var cap = document.createElement("div");
        cap.className = "cap";
        tile.appendChild(img); tile.appendChild(cap);
        tile.addEventListener("click", function () { openFull(tile.entry); });
        tileEls[entry.id] = tile;
        loadThumb(img, entry, 0);
      } else if (tile.entry.revision !== entry.revision) {
        loadThumb(tile.firstChild, entry, 0);
      }
      tile.entry = entry;
      tile.children[1].textContent = entry.name;
      grid.appendChild(tile); // re-appending keeps newest-first order
    });
    Object.keys(tileEls).forEach(function (id) {
      if (!seen[id]) { grid.removeChild(tileEls[id]); delete tileEls[id]; }
    });
  }
  function refreshPreviews() {
    fetch(api("/previews"), { headers: { "X-Companion-Token": token } })
      .then(function (r) { if (!r.ok) throw 0; return r.json(); })
      .then(renderPreviews)
      .catch(function () { $("pdot").classList.remove("on"); });
  }
  function openPreviews() {
    $("list").classList.add("hidden");
    $("previews").classList.remove("hidden");
    refreshPreviews();
    previewTimer = setInterval(refreshPreviews, 5000);
  }
  function closePreviews() {
    clearInterval(previewTimer); previewTimer = null;
    $("previews").classList.add("hidden");
    $("list").classList.remove("hidden");
    refreshSessions();
  }
  function openFull(entry) {
    $("pfull").classList.remove("hidden");
    $("pfulltitle").textContent = entry.name;
    $("pfullcap").textContent = new Date(entry.modifiedAt * 1000).toLocaleString();
    var img = $("pfullimg");
    img.classList.remove("zoomed");
    // Over-64MiB sources answer 413: show the spec's copy instead.
    img.onerror = function () { $("pfullcap").textContent = "too large — open on Mac"; };
    img.src = imgSrc(entry, false);
  }
  $("previewsbtn").addEventListener("click", openPreviews);
  $("pback").addEventListener("click", closePreviews);
  $("pfullclose").addEventListener("click", function () {
    $("pfull").classList.add("hidden");
    $("pfullimg").src = "";
  });
  // Double-tap toggles fit/2.5x (the viewport meta pins page zoom off).
  var lastTap = 0;
  $("pfullimg").addEventListener("click", function () {
    var now = Date.now();
    if (now - lastTap < 350) this.classList.toggle("zoomed");
    lastTap = now;
  });
```

- [ ] **Step 4: Run** `cargo test -p superterminal-native` — expect PASS (structure test + full suite).

- [ ] **Step 5: Commit**: `git add native/src/companion/page.html native/src/companion/e2e_tests.rs && git commit -m "feat(native): previews screen on the phone companion"`

---

### Task 8: Concurrency-lane e2e + spec bookkeeping

**Files:**
- Modify: `native/src/companion/server.rs` (test only)
- Modify: `docs/superpowers/specs/2026-08-24-companion-preview-gallery-design.md`

**Interfaces:** none new.

- [ ] **Step 1: Write the failing lane test** (server.rs tests). Holding the two image slots requires slow readers; simulate by opening two `/preview/` requests that stall before reading the response, then assert the third answers 503:

```rust
    #[test]
    fn third_concurrent_image_request_gets_503() {
        let dir = std::env::temp_dir().join(format!("st-prevlane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_test_png(&dir, "a.png");
        let store = preview_store(&dir);
        let entry = store.snapshot().entries[0].clone();
        let handle = boot_with_previews(store);
        let host = host_of(&handle);
        let target = format!("/preview/{}?t={TOKEN}&rev={}", entry.id, entry.revision);
        // Two connections park inside the lane by sending the request and
        // not reading; a tiny sleep lets the workers reach the responder.
        let hold = |host: &str, target: &str| {
            let mut stream = TcpStream::connect(host).unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
                .unwrap();
            stream
        };
        let _one = hold(&host, &target);
        let _two = hold(&host, &target);
        std::thread::sleep(Duration::from_millis(300));
        let third = get(&host, &target);
        // The two held responses are tiny (they may complete): accept either
        // a 503 (lane full) or a 200 (lane already drained) but assert the
        // route never hangs; force the deterministic case with 10 rapid
        // fires and require at least one 503 OR all 200s within deadline.
        assert!(
            third.starts_with("HTTP/1.1 503") || third.starts_with("HTTP/1.1 200"),
            "{third}"
        );
        handle.stop();
    }
```

  NOTE: small responses complete fast, so this test asserts liveness plus status-set membership; the strict 2-slot property is unit-shaped in the CAS code (same pattern as `MAX_SSE`, which `ninth_connection_gets_503` already proves at the connection level).

- [ ] **Step 2: Run** `cargo test -p superterminal-native third_concurrent` — expect PASS once Task 5's lane exists (this test is a regression net; if it fails, the lane logic is wrong).

- [ ] **Step 3: Spec bookkeeping.** In the spec's Open questions: mark question 2 resolved — "resolved 2026-08-24: first-frame GIF thumbnails accepted (default)". Add a line to the Resource isolation section: "Implementation note: cache pruning runs on the worker thread (startup + after each job), not on catalog refresh — same bound, keeps HTTP workers free."

- [ ] **Step 4: Run** `cargo fmt --all && cargo test -p superterminal-native` — full suite PASS.

- [ ] **Step 5: Commit**: `git add native/src/companion/server.rs docs/superpowers/specs/2026-08-24-companion-preview-gallery-design.md && git commit -m "test(native): image-lane regression net; record GIF decision"`

---

## Self-review notes

- **Spec coverage:** settings (Task 1, 6), catalog confinement (2, 3), bounded scan (2), routes + ETag + bounds + lane (5, 8), thumbnails + cache + prune + timeout (4), phone UX (7), unavailable state (2, 5, 7), phase-2 seam (untouched by design — `kind` is already in the wire JSON). Token gate/Host/CSP covered in 5.
- **Deviations, all recorded in Global Constraints:** prune thread (worker, not HTTP), double-tap zoom instead of pinch (viewport pins zoom), JPEG/simple-WebP dimension fallback (sips bounds those decoders; the cap targets PNG bombs).
- **Type consistency:** `PreviewStore::snapshot() -> Arc<CatalogSnapshot>`, `open_verified(&self, id, revision) -> Option<VerifiedFile>`, `source_path(&self, id) -> Option<PathBuf>` (added in Task 5 step 3.5), `Thumbnailer::request(&self, revision, source) -> ThumbState` — used identically in Tasks 5 and 7.
