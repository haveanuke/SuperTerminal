# Companion Preview Gallery — Design

Date: 2026-08-24
Status: draft for review
Reviewed: Codex round 1 findings incorporated (resource isolation, file
confinement, bounded processing, cache identity, phase-2 protocol).

## Goal

Phase 1: a "previews" screen in the phone companion showing images from a
watched folder on the Mac (Blender renders and any other exports), so work
can be checked from the phone. Phase 2 (later): a live Blender viewport
tile in the same screen, fed through the same store — designed for now,
not built.

## Non-goals

- No interactive 3D viewing, no model-format parsing (images only).
- No uploads from the phone.
- No Mac-side gallery UI (the Mac has Finder/Quick Look).
- No viewport-rate streaming in phase 1's polling model.

## Settings

`preview_dir: Option<PathBuf>` in `Settings`, default
`$HOME/Pictures/SuperTerminal` (resolved via `HOME` — `~` is never stored;
`PathBuf` does not expand it). Editable in the settings overlay next to
font/theme. Changing it swaps the catalog atomically; the server itself
does not restart (routes read the catalog through the hub-adjacent store,
never a captured path).

## Catalog (file confinement)

A `PreviewCatalog` owns the mapping from opaque ids to files. Codex's
confinement rules, verbatim into the design:

- Only **direct children** of the watched dir are eligible; subdirectories
  are not walked in phase 1.
- Only **regular files** (no symlinks, sockets, devices). Files are opened
  relative to a held directory descriptor with `openat` + `O_NOFOLLOW`
  (small libc wrapper, same pattern as the existing `getifaddrs` FFI in
  `companion/net.rs`); after open, `fstat` re-verifies regular-file type,
  device+inode, and size before any byte is served. TOCTOU replacement of
  the file or the watched dir therefore fails closed.
- Extension prefilter (`png jpg jpeg webp gif`) then **magic-byte
  validation** on open; mismatches are rejected, `Content-Type` is set
  from the sniffed type, and `X-Content-Type-Options: nosniff` is sent.
- Ids are opaque (`p<counter>` per scan generation); clients never send
  paths. Each entry carries `revision = hash(dev, inode, size, mtime)` so
  a replaced file is a new revision, never a stale cache hit.

Scanning is bounded: at most 512 directory entries inspected per refresh,
newest-by-mtime 64 kept, snapshot cached for 2s so a 5s-polling phone
never triggers back-to-back scans. A missing or unreadable folder is a
distinct catalog state (`unavailable`), not an empty list.

## Server routes

Both behind the existing token gate (checked before routing, 404 on
failure), Host check, and CSP — which gains exactly `img-src 'self'`.
Image `<img>` tags cannot send the token header, so they use the existing
`?t=` query pattern; `Referrer-Policy: no-referrer` stays, and request
targets are never logged.

- `GET /previews?t=` → `{"state":"ok"|"unavailable","entries":[{"id","kind":"image","name","revision","modifiedAt","bytes"}]}`
  Newest first, ≤ 64 entries.
- `GET /preview/<id>?t=&rev=<revision>` → image bytes.
  `?thumb=1` serves the downscaled variant. Strong `ETag` = revision;
  `If-None-Match` → 304. Unknown id or stale revision → 404 (the phone
  re-lists).

Response bounds: source files over 64 MiB are never served full-size
(entry still listed; phone shows "too large — open on Mac"); thumbnails
are always small. Files are streamed in 64 KiB chunks with the existing
10s write deadline enforced per write, never `fs::read` of unbounded
data.

## Resource isolation

The 8-connection budget currently protects sessions and input. Images get
their own lane so a thumbnail grid cannot starve control routes:

- Image responses (`/preview/*`) admit at most **2 concurrent** via CAS,
  503 over that (phone retries lazily as tiles scroll into view).
- Thumbnail generation is a **single worker thread** with a bounded queue
  (8 jobs, drop-oldest); a request for an unbuilt thumbnail returns 202 +
  `Retry-After: 1` and the page retries — HTTP workers never block on
  generation.
- `sips` is invoked as `/usr/bin/sips` via `Command::new` with fixed
  separate args (`-Z 512`, explicit out path) — no shell — under a 10s
  timeout with kill+reap. Source dimension cap (from the image header)
  before invoking; refuse absurd pixel counts.

## Thumbnail cache

`~/Library/Caches/SuperTerminal/previews/<revision>.jpg`, written to a
temp file and atomically renamed. Keyed by revision, so replacement
invalidates naturally. Pruned on catalog refresh: max 128 MiB / 30 days,
oldest first.

## Phone UX

Third screen `#previews`, reachable from a button on the session list
header. Thumbnail grid (2-up portrait), tap → full-size view with
pinch-zoom, name + mtime caption. Polls `/previews` every 5s while
visible; DOM nodes are keyed by id and updated in place (no image element
churn, no flicker). `state:"unavailable"` renders a distinct notice with
the configured path. Vanilla JS, same file, no dependencies.

## Phase 2 slot (designed, not built)

`kind:"viewport"` entries join the same list: a publisher (the Blender
bridge) writes the latest captured frame into the store and bumps its
revision; the phone treats it as any other entry, ETag-polling its
revision. HTTP requests never drive Blender synchronously. Faster-than-
poll streaming, if ever wanted, becomes a new SSE event type carrying
`{id, revision}` invalidations — not image bytes.

## Testing

- Catalog: unit tests with a temp dir — symlink rejected, non-regular
  rejected, replacement changes revision, missing dir → `unavailable`,
  scan bounds respected.
- Routes: e2e tests in the existing harness — token gate, ETag/304, 404
  on stale revision, 503 over image concurrency, oversized file listed
  but refused, magic-byte mismatch refused.
- Thumbnails: worker tested with a real `sips` invocation on a generated
  PNG; timeout path with a stub binary.
- Page: structure test extended for the new screen's hooks.

## Open questions (for review)

1. Default folder `~/Pictures/SuperTerminal` vs `~/Pictures/` directly —
   the former is opt-in-by-saving-there, the latter shows everything.
2. GIFs animate in full view for free; cap thumbnail generation to first
   frame (sips does this implicitly) — acceptable?
