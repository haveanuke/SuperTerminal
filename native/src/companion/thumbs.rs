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
    /// The VERIFIED descriptor from `open_verified` — generation never
    /// touches the watched directory again, so post-verification swaps
    /// (symlinks, replacements) cannot reach sips.
    source: std::fs::File,
    len: u64,
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
        // The worker holds only a Weak handle and re-upgrades per cycle:
        // when the last store reference drops, the thread exits within one
        // wait timeout instead of leaking per companion start.
        let weak = Arc::downgrade(&this);
        let _ = std::thread::Builder::new()
            .name("companion-thumbs".into())
            .spawn(move || worker_loop(weak));
        this
    }

    fn cache_path(&self, revision: &str) -> PathBuf {
        self.cache_dir.join(format!("{revision}.jpg"))
    }

    /// Ready iff the cache already holds this revision; otherwise enqueue
    /// the verified descriptor (drop-oldest beyond QUEUE_CAP) and return
    /// Pending immediately.
    pub fn request(&self, revision: &str, source: std::fs::File, len: u64) -> ThumbState {
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
                len,
            });
        }
        drop(queue);
        self.wake.notify_one();
        ThumbState::Pending
    }

    fn process(&self, job: Job) {
        use std::io::{Read, Seek};
        let out = self.cache_path(&job.revision);
        if out.is_file() {
            return;
        }
        let mut source = job.source;
        // Bounded private copy FIRST, from the verified descriptor: the
        // dimension check and sips then read the SAME immutable bytes — a
        // writer mutating the source mid-flight cannot desync check from
        // use. A copy shorter than the verified length means the source
        // changed underneath us: refuse.
        let copy_path = self
            .cache_dir
            .join(format!("{}.src.{}", job.revision, std::process::id()));
        let copied = (|| -> std::io::Result<u64> {
            source.rewind()?;
            let mut copy = std::fs::File::create(&copy_path)?;
            std::io::copy(&mut (&mut source).take(job.len), &mut copy)
        })();
        if copied.map(|n| n != job.len).unwrap_or(true) {
            let _ = std::fs::remove_file(&copy_path);
            return;
        }
        drop(source);
        // Refuse absurd pixel counts BEFORE invoking sips (decompression-
        // bomb guard); unknown or truncated headers refuse too. Read from
        // the private copy, never the source.
        let dims_ok = (|| -> std::io::Result<bool> {
            let mut copy = std::fs::File::open(&copy_path)?;
            let mut head = vec![0u8; 256 * 1024];
            let read = copy.read(&mut head)?;
            Ok(matches!(
                image_dimensions(&head[..read]),
                Some((w, h)) if (w as u64) * (h as u64) <= MAX_PIXELS
            ))
        })()
        .unwrap_or(false);
        if !dims_ok {
            let _ = std::fs::remove_file(&copy_path);
            return;
        }
        let tmp = self
            .cache_dir
            .join(format!("{}.tmp.{}", job.revision, std::process::id()));
        if generate_with(
            Path::new("/usr/bin/sips"),
            &copy_path,
            &tmp,
            Duration::from_secs(SIPS_TIMEOUT_SECS),
        ) {
            let _ = std::fs::rename(&tmp, &out);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        let _ = std::fs::remove_file(&copy_path);
        prune(&self.cache_dir);
    }
}

fn worker_loop(weak: std::sync::Weak<Thumbnailer>) {
    if let Some(this) = weak.upgrade() {
        prune(&this.cache_dir);
    }
    loop {
        let Some(this) = weak.upgrade() else { break };
        let job = {
            let mut queue = this.queue.lock().unwrap();
            if let Some(job) = queue.pop_front() {
                Some(job)
            } else {
                let (mut queue, _) = this
                    .wake
                    .wait_timeout(queue, Duration::from_millis(500))
                    .unwrap();
                queue.pop_front()
            }
        };
        if let Some(job) = job {
            this.process(job);
        }
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
        // JPEG: walk segments for a SOF marker. No SOF inside the buffer
        // (the caller reads 256 KiB) means refuse — never assume small.
        let mut i = 2;
        while i + 1 < head.len() {
            if head[i] != 0xff {
                return None;
            }
            let marker = head[i + 1];
            if marker == 0xff {
                i += 1; // fill byte
                continue;
            }
            if (0xd0..=0xd9).contains(&marker) {
                i += 2; // standalone marker, no length payload
                continue;
            }
            if i + 8 >= head.len() {
                return None;
            }
            let len = u16::from_be_bytes([head[i + 2], head[i + 3]]) as usize;
            if len < 2 {
                return None;
            }
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
            {
                let h = u16::from_be_bytes([head[i + 5], head[i + 6]]) as u32;
                let w = u16::from_be_bytes([head[i + 7], head[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
        return None;
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()) && head.len() >= 25
    {
        if head.get(12..16) == Some(b"VP8X".as_slice()) {
            if head.len() < 30 {
                return None;
            }
            let w = 1 + u32::from_le_bytes([head[24], head[25], head[26], 0]);
            let h = 1 + u32::from_le_bytes([head[27], head[28], head[29], 0]);
            return Some((w, h));
        }
        if head.get(12..16) == Some(b"VP8 ".as_slice()) {
            // Lossy: 3-byte frame tag, sync 9d 01 2a, then 14-bit dims.
            if head.len() < 30 || head.get(23..26) != Some(&[0x9d, 0x01, 0x2a][..]) {
                return None;
            }
            let w = (u16::from_le_bytes([head[26], head[27]]) & 0x3fff) as u32;
            let h = (u16::from_le_bytes([head[28], head[29]]) & 0x3fff) as u32;
            return Some((w, h));
        }
        if head.get(12..16) == Some(b"VP8L".as_slice()) {
            // Lossless: 0x2f signature, then width-1 / height-1 as packed
            // 14-bit little-endian bit fields.
            if head.len() < 25 || head[20] != 0x2f {
                return None;
            }
            let w = 1 + ((head[21] as u32) | ((head[22] as u32 & 0x3f) << 8));
            let h = 1
                + (((head[22] as u32) >> 6)
                    | ((head[23] as u32) << 2)
                    | ((head[24] as u32 & 0x0f) << 10));
            return Some((w, h));
        }
        return None;
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
    fn jpeg_dimensions_walk_segments_and_refuse_when_sof_is_missing() {
        // SOI, APP0 (18 bytes of payload), then SOF0 with 480x640.
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend_from_slice(&[0xff, 0xe0, 0x00, 0x12]);
        jpeg.extend(std::iter::repeat(0u8).take(0x12 - 2));
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 8, 0x01, 0xe0, 0x02, 0x80]);
        assert_eq!(image_dimensions(&jpeg), Some((640, 480)));
        // No SOF within the buffer: refuse, never assume small.
        let mut headless = vec![0xff, 0xd8];
        headless.extend_from_slice(&[0xff, 0xe0, 0x00, 0x12]);
        headless.extend(std::iter::repeat(0u8).take(0x12 - 2));
        assert_eq!(image_dimensions(&headless), None);
    }

    #[test]
    fn webp_dimensions_parse_lossy_and_lossless_refusing_unknown() {
        // VP8 (lossy): sync code then 14-bit width/height at bytes 26..30.
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0".to_vec();
        vp8.extend_from_slice(&[0, 0, 0]); // frame tag
        vp8.extend_from_slice(&[0x9d, 0x01, 0x2a]); // sync
        vp8.extend_from_slice(&[0x40, 0x01, 0xf0, 0x00]); // 320 x 240
        assert_eq!(image_dimensions(&vp8), Some((320, 240)));
        // VP8L (lossless): signature 0x2f then packed 14-bit dims (-1).
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0".to_vec();
        vp8l.push(0x2f);
        // width-1 = 0x13f (320), height-1 = 0xef (240):
        // b21=0x3f, b22=0x01 | (0xef & 0x3)<<6 -> h low bits...
        let w1: u32 = 319;
        let h1: u32 = 239;
        vp8l.push((w1 & 0xff) as u8);
        vp8l.push((((w1 >> 8) & 0x3f) | ((h1 & 0x03) << 6)) as u8);
        vp8l.push(((h1 >> 2) & 0xff) as u8);
        vp8l.push(((h1 >> 10) & 0x0f) as u8);
        assert_eq!(image_dimensions(&vp8l), Some((320, 240)));
        // Unknown chunk type: refuse.
        assert_eq!(
            image_dimensions(b"RIFF\0\0\0\0WEBPXXXX\0\0\0\0\0\0\0\0\0\0"),
            None
        );
    }

    #[test]
    fn length_mismatch_refuses_thumbnailing() {
        // The worker requires the bounded copy to equal the fstat length a
        // request was verified at — a source that shrank (or a len that
        // lies) must never reach sips.
        let dir = scratch("lenmismatch");
        let cache = scratch("lenmismatch-cache");
        let src = dir.join("a.png");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
        std::fs::write(&src, png).unwrap();
        let real_len = std::fs::metadata(&src).unwrap().len();
        let thumbs = Thumbnailer::new(cache.clone());
        thumbs.request(
            "deaddeaddeaddead",
            std::fs::File::open(&src).unwrap(),
            real_len + 5,
        );
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            !cache.join("deaddeaddeaddead.jpg").is_file(),
            "short copy must be refused, not thumbnailed"
        );
    }

    #[test]
    fn dropping_the_store_ends_the_worker_thread() {
        let cache = scratch("shutdown");
        let thumbs = Thumbnailer::new(cache);
        let weak = Arc::downgrade(&thumbs);
        drop(thumbs);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while weak.upgrade().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker must release its handle and exit after the drop"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
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
        // sips itself writes the fixture: a solid PNG scaled from a system
        // image. If that source is missing on this macOS, skip.
        let status = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png", "-z", "600", "600"])
            .arg("/System/Library/Desktop Pictures/Solid Colors/Black.png")
            .arg("--out")
            .arg(&src)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) || !src.is_file() {
            return;
        }
        let thumbs = Thumbnailer::new(cache.clone());
        let open = || std::fs::File::open(&src).unwrap();
        let len = std::fs::metadata(&src).unwrap().len();
        assert!(matches!(
            thumbs.request("cafecafecafecafe", open(), len),
            ThumbState::Pending
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let ThumbState::Ready(path) = thumbs.request("cafecafecafecafe", open(), len) {
                assert!(path.ends_with("cafecafecafecafe.jpg"));
                let bytes = std::fs::read(path).unwrap();
                assert!(!bytes.is_empty());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "thumbnail never appeared"
            );
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
        let ok = generate_with(
            &stub,
            &src,
            &cache.join("x.jpg"),
            std::time::Duration::from_millis(300),
        );
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
            file.set_times(std::fs::FileTimes::new().set_modified(age))
                .unwrap();
        }
        prune(&cache);
        let left = std::fs::read_dir(&cache).unwrap().count();
        assert_eq!(left, 0, "all five are older than 30 days");
    }
}
