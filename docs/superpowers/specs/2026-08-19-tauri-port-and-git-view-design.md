# SuperTerminal: Tauri Port + Git Source-Control View — Design

**Date:** 2026-08-19 (rev 6, after five external review rounds by Codex; rev 6 amendments: bulk git operations enumerate explicit actionable paths instead of `add -A`/bare `reset` so non-UTF-8 entries are consistently excluded; v1 toast errors are capped messages with full stderr in the console rather than expandable)
**Status:** Draft — pending user approval
**Author:** Claude (brainstormed with Tomas; reviewed by Codex CLI)

## Goal

Two sequential deliverables:

1. **Port SuperTerminal from Electron to Tauri 2** with feature parity (one deliberate exception: webview `localStorage` settings reset once — see "Settings migration"), eliminating the bundled Chromium/Node runtime.
2. **Add a VS Code-style source-control sidebar**: git graph, staged/unstaged changes, merge conflicts, branch + ahead/behind sync status, with interactive actions (stage, unstage, discard, commit, push, pull, fetch).

The port lands and is verified **before** git-view work begins.

## Context

Current app: Electron 41, React 18 + xterm.js renderer (~5,200 lines), thin Electron main process (~500 lines):

- `src/main/pty-manager.ts` — node-pty wrapper (create/write/writeBroadcast/resize/dispose, data/exit events)
- `src/main/session-manager.ts` — JSON session persistence in `userData/sessions` with schema validation
- `src/main/buddy-agent.ts` — spawns a configurable agent CLI, `{prompt}` substitution, timeout, ANSI-strip, login-shell PATH resolution
- `src/main/index.ts` — window config (hiddenInset title bar, traffic lights at 12,14, bg `#1a1b26`), IPC handlers, image-file dialog, external-link handling
- `src/main/preload.ts` — exposes `window.superTerminal` (pty/session/dialog/buddy namespaces)

The renderer reaches the backend **only** through `window.superTerminal`, with two known exceptions that need renderer-side changes in the port:
- `App.tsx:35` builds `file://${backgroundImage}` URLs directly
- `xterm-web-links.ts:56` calls `window.open()` (relies on Electron's window-open handler)

PTY data has **two** string consumers — xterm (`xterm-registry.ts:140`) and the buddy event watcher (`use-event-watcher.ts:137`, regex over text) — so the `onData(string)` contract must be preserved and fanned out.

macOS arm64 is the only build target. Rust toolchain installed (cargo 1.97.1). CI (`.github/workflows/ci.yml`) and release (`.github/workflows/release.yml`) currently build Electron.

## Decisions Made

| Decision | Choice | Why |
|---|---|---|
| Framework | Tauri 2 (over Wails) | Stable major version, channels for streaming, npm-native tooling, larger ecosystem |
| Git backend | Shell out to `git` CLI, parse porcelain output | What VS Code does; respects user credentials/hooks/config; zero new deps (project preference: minimal dependencies) |
| Git view interactivity | Full interactive | Stage/unstage/discard/commit/push/pull/fetch |
| Repo selection | Follow active terminal's cwd (best-effort) | Auto-resolve focused pane's shell cwd → nearest enclosing repo |
| UI placement | Toggleable left sidebar | VS Code-style, alongside terminals |
| Diff viewer | **Out of scope v1** | File lists show status letters; diffs stay in the terminal. Destructive actions are guarded by confirm dialogs instead (double-confirm for irreversible ones). Future work. |
| Settings migration | **Not migrated** (one-time reset) | Theme/font/buddy/UI settings live in webview `localStorage`; Electron and WKWebView store it in different, incompatible locations. Parsing Electron's LevelDB from Rust is disproportionate for a 0.x app. Users re-pick settings once. Terminal *sessions* (JSON files) **are** migrated. |

## Part 1: Tauri Port

### Port strategy

- All work on a feature branch (`feat/tauri-port`).
- `src-tauri/` is added alongside the existing Electron main process; Electron stays runnable until Tauri reaches parity.
- Final cleanup commit removes Electron deps (`electron`, `electron-builder`, `@electron/rebuild`, `node-pty`), `src/main/`, Electron build scripts, and updates CI/release workflows and README.

### Rust backend (`src-tauri/src/`)

**`pty.rs`** — PTY management via `portable-pty`.

- `pty_create(id, cols, rows, cwd, events: Channel)` — spawns the user's login shell (`$SHELL`, fallback `/bin/zsh`), `TERM=xterm-256color`, env captured once from a login shell (`$SHELL -ilc env`, 5s timeout, fallback to process env). Idempotent: existing id is a no-op (matches current behavior). Recreating an id after dispose is allowed.
- **One ordered channel per terminal; transport is bytes; decoding is streaming.** PTY output is not guaranteed valid UTF-8 and reads can split multibyte sequences. A **single** `Channel<InvokeResponseBody>` per terminal carries both data (`InvokeResponseBody::Raw(bytes)` — a plain `Channel<Vec<u8>>` would JSON-serialize each byte) and, as its **final message**, a tagged JSON exit event (`{"exit": code}`). One channel means Tauri's ordering guarantee applies across data *and* exit — exit can never overtake trailing output. The bridge decodes data frames with one **persistent streaming `TextDecoder` per terminal** (`{stream: true}`, so split multibyte sequences reassemble correctly) and delivers strings — preserving the existing `onData(string)` contract for both consumers (xterm and buddy watcher). On the exit frame the bridge flushes the decoder (`decode()` final call), emits any last text, then notifies exit subscribers. Invalid UTF-8 degrades to replacement chars, matching current node-pty behavior.
- One reader thread per PTY pumps output into the channel until EOF; a waiter thread collects the exit code and hands it to the reader path, which sends the exit frame **after** the last data frame — **exactly one** exit event per terminal, kill and natural exit both routed through it.
- Record ownership rules: the PTY map mutex is **never held across I/O, `wait()`, `kill()`, resize, or channel sends** — lock, clone/take handles, unlock, then act. The slave end is dropped immediately after spawn. Worker threads are detached but hold only weak access to shared state; channel send failures (webview gone) terminate the reader loop silently.
- `pty_write(id, data)`, `pty_write_broadcast(ids, data)`, `pty_resize(id, cols, rows)`, `pty_dispose(id)`. Dispose kills the child, drops handles, removes the record; racing reader/waiter threads exit via closed handles.
- **Shutdown:** all PTYs are killed when the **last window is destroyed** (parity with Electron's `window-all-closed`, which fires on macOS too even though the app keeps running) *and* on app exit (`RunEvent::Exit`) as a backstop. Dock-reactivation after last-window close therefore opens a fresh window with fresh state — same as today.
- `pty_cwd(id) -> Option<String>` — **best-effort** cwd of the spawned shell process via libproc (`PROC_PIDVNODEPATHINFO`), macOS-only. Reports the root shell's cwd — nested shells/tmux/ssh are not tracked (documented limitation; acceptable for repo-following). PID is taken from the spawned child at creation and invalidated when the PTY exits (no PID-reuse window).

**`session.rs`** — same JSON schema and validation semantics (reject dangerous keys, validate pane-tree shape), stored in Tauri's app-data dir under `sessions/`.
- Commands: `session_save`, `session_load`, `session_list`, `session_delete`. Name sanitization identical (`[^a-zA-Z0-9_-]` → `_`). Load errors distinguish `invalid_json` vs `schema_mismatch`.
- `session_load` is **API parity only** — the current UI lists/saves/deletes but has no restore flow; no new UI is added by the port.
- **Migration:** on startup, if marker file `sessions/.migrated` is absent, copy `*.json` from the old Electron dir (`~/Library/Application Support/SuperTerminal/sessions`) — skipping any name that already exists — then write the marker. Idempotent, never clobbers.

**`buddy.rs`** — port of `runBuddyAgent`: same guards (≤16 args, ≤4096 chars each), `{prompt}` substitution, no shell interpretation, login-shell PATH + `NO_COLOR=1` + `FORCE_COLOR=0`, null stdin, kill on timeout (default 25s), ANSI-strip stdout, first paragraph capped at 280 chars.

**`main.rs` / `tauri.conf.json`** — window + app config:
- `titleBarStyle: "Overlay"`, `trafficLightPosition: (12, 14)`, `decorations: true`, background `#1a1b26`, min 600×400, default 1200×800, app icon. **Drag region:** the tab-bar/toolbar strip gets an explicit drag region (`data-tauri-drag-region`) — audited against the current Electron drag behavior; this is functional, not polish.
- macOS lifecycle parity: last window closing does not quit; dock-icon reactivation (`RunEvent::Reopen`) recreates the window; app exit kills all PTYs.
- **External links:** `@tauri-apps/plugin-opener` with capability restricted to `http`/`https`; `xterm-web-links.ts` changes `window.open()` → `openUrl()` (small renderer edit).
- **Image dialog + background image:** dialog plugin picks the image (same filters); the file is **copied into app-data** (`background/` dir) and the renderer stores/uses `convertFileSrc` URL of the copy. Asset protocol enabled with scope limited to the app-data dir; CSP `img-src` updated accordingly. No broad filesystem scope. (`App.tsx` `file://` usage replaced.)
- Dev: `tauri dev` against Vite on :5173; prod: bundled assets, `tauri build` → arm64 `.app`/`.dmg`.

### Frontend bridge (`src/renderer/lib/tauri-bridge.ts`)

Implements the existing `window.superTerminal` interface over `invoke` + Channels, assigned before React mounts. Renderer changes are limited to this module plus the two files listed in Context — `onData` still delivers strings, so xterm and the buddy watcher are untouched.

- The channel must be passed *into* `pty_create`, but the renderer attaches `onData`/`onExit` *after* `create` resolves — so the bridge creates the (single) channel inside `create()`, retains it by id, and holds **independent delivery state per event kind**: decoded text buffers until the first *data* subscriber attaches (bounded at 1MB; overflow drops oldest and logs a warning), and an exit event **latches** until an *exit* subscriber attaches — `onData` attaching first must not cause a fast exit to be dropped before `onExit` registers. This also fixes a latent race the Electron version has.
- Bridge lifecycle rules: `onData`/`onExit` support **multiple subscribers** (fan-out; each gets every chunk; unsubscribe functions preserved). `create()` for an id the bridge already tracks live is a **no-op** (existing channels kept — never silently replaced, matching the backend's idempotent create). A failed `create` invoke removes the bridge record. `dispose()` clears the record, buffers, decoder, and subscribers; a later `create(id)` starts clean.
- Throughput note: Tauri channels are ordered but provide no application backpressure; the reader thread reads in large chunks (≥64KB buffer) which naturally coalesces under load. The `yes`-flood smoke test is the gate — if the UI can't keep up, add reader-side coalescing (merge pending chunks per tick) before proceeding.

### Riskiest assumptions (verify in a spike before building everything)

1. PTY throughput through a byte channel (`yes`, huge `cat`, `find /`) with acceptable UI latency.
2. xterm.js **default (DOM/canvas) renderer** in WKWebView — the app does not use the WebGL addon. Test: clipboard shortcuts, IME/dead keys, Retina scaling, large scrollback, font measurement, and the custom key interception in `xterm-registry.ts:76`.
3. `speechSynthesis` (buddy TTS) in WKWebView.
4. Overlay title bar + traffic-light position + drag region matches current feel.

### Testing (port)

- Rust `cargo test`: session validation/sanitization + migration marker logic (port of `session-manager.test.ts`), buddy guards/output cleaning (port of `buddy-agent.test.ts`).
- Existing vitest renderer tests keep running.
- Manual smoke checklist: throughput flood, resize, splits, tabs, broadcast input, session save/list/delete + migration, buddy reaction, TTS, external links, background image (pick + persist across restart), traffic lights/drag, dock reactivation, quit-kills-shells.
- CI: workflows updated — Rust toolchain + Cargo cache, `cargo test` (libproc-dependent tests gated to macOS runners), vitest, `tauri build` on macOS runner for release artifacts (paths from `src-tauri/target/`), README updated.

## Part 2: Git Source-Control View

Build order within Part 2: repo resolution + status (read-only sidebar) → stage/unstage → commit → push/pull/fetch → discard (with backend validation) → graph last.

### Process hygiene (every git invocation)

`git -C <repo>` with env `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, `GIT_LITERAL_PATHSPECS=1` (pathspec magic like leading `:` must never expand — `--` alone does not disable it), `LC_ALL=C`; **stdin null**; stdout/stderr captured with size caps (1MB/64KB) — **exceeding a cap kills the process group and returns an explicit "output too large" error; truncated output is never parsed as valid**; per-operation timeout (10s local, 120s network ops) with **process-group kill** on expiry. `GIT_TERMINAL_PROMPT=0` reduces—but does not eliminate—interactive blocking (askpass helpers, pinentry, hooks can still stall); the timeout is the actual guarantee. Auth/hook/signing failures surface as readable errors (capped stderr) — including `commit.gpgSign` pinentry failures; we honor the user's signing config and report failure rather than overriding it.

**Concurrency:** one operation at a time per repo (busy flag); the UI disables action buttons while busy. Status refreshes are also serialized per repo — at most one in flight.

### Repo handles & path safety

- `git_resolve_repo(cwd) -> Option<{repo_id, display_name, root}>` — backend canonicalizes `rev-parse --show-toplevel` and returns an opaque `repo_id`; the canonical root lives only backend-side in a registry that **interns by canonical root** — resolving the same repository always yields the same `repo_id`, so the per-repo busy/serialization guarantees can't be bypassed via duplicate handles. All subsequent commands take `repo_id`, not raw paths. (Threat-model note: the renderer already has arbitrary exec via `pty_write`, so this is correctness hygiene against stale/garbled UI state more than a security boundary — but it costs little.)
- Action paths are repo-relative and **re-validated against a fresh `git status` snapshot backend-side** before any destructive command (discard/clean); paths no longer in the expected state are rejected with a "state changed, refresh" error.
- **Non-UTF-8 paths:** git `-z` output is raw bytes; entries whose paths aren't valid UTF-8 are shown (lossy display) but **all actions on them are rejected backend-side** — explicit v1 limitation.

### Read commands

- `git_status(repo_id) -> StatusReport` — `git status --porcelain=v2 --branch -z --untracked-files=all`. Model preserves porcelain v2 faithfully:
  - `StatusEntry { kind: ordinary|rename_copy|unmerged|untracked, index_status, worktree_status, path, orig_path?, submodule? }`
  - A file with both index and worktree changes appears in **both** Staged and Changes lists (as in VS Code).
  - Report carries branch (or detached-HEAD short sha), upstream name, ahead/behind counts, plus a flag for unborn HEAD (no commits yet).
- `git_graph(repo_id, limit) -> Vec<CommitNode>` — `git log --all --topo-order -z` with hash, parents, refs (`%D`), author, unix date, subject. Default limit 300.
  - **"Load more" re-runs the full log + layout with a larger limit** (no incremental page-stitching — offset pagination over mutable history is unstable, and lanes can't restart per page; a full re-layout of ≤1000 commits is cheap and always consistent).

### Action commands

Each returns a fresh `StatusReport` on success; errors carry capped stderr.

- Stage: `git add -- <paths>` (bulk = the explicit list of all actionable paths, never `-A` — non-UTF-8 entries stay excluded). Unstage: `git restore --staged -- <paths>`; **unborn HEAD** uses `git rm --cached -r -f -- <paths>` instead (no HEAD to restore from; `-f` because `rm --cached` refuses when staged content differs from the worktree, and `--cached` still leaves the worktree untouched). Bulk unstage-all: `git reset` (or `rm --cached -r -f .` when unborn). **Rename entries always expand to both paths** before the git call: staging a worktree rename adds `orig_path` *and* `path`; unstaging a staged rename restores both — acting on one path alone would leave half the rename staged. **Copy entries act on the destination only** (for stage, unstage, and discard alike): a copy's `orig_path` is a live source file, and including it would stage or revert unrelated modifications. Every rename/copy expansion dispatches on the porcelain R/C marker.
- Discard (after renderer confirm; double-confirm for untracked): tracked → `git restore -- <paths>`; untracked files **and directories** → `git clean -fd -- <paths>`. **Worktree renames** (porcelain type-2, worktree status R) discard as a pair: restore the source (`orig_path`) and clean the destination (`path`) — restoring the destination alone is impossible since it's absent from the index. **Worktree copies** (worktree status C) clean the destination **only** — `orig_path` is a live source file whose own modifications must not be touched. Dispatch on the R/C marker; the confirm dialog shows exactly which paths will be affected. Unmerged (conflicted) entries are **excluded from plain discard** — v1 offers only "stage as resolved" (`git add`) for conflicts; pick-ours/theirs is terminal territory. **Submodule entries:** stage/unstage act on the gitlink (normal `git add`/`restore --staged` behavior), but **discard is rejected backend-side for submodules** — `git restore` without `--recurse-submodules` silently does nothing to the submodule worktree, and with it can destroy nested local changes; v1 directs users to the terminal instead.
- Commit: `git commit -m <message>` (single argv entry, multi-line OK; disabled when nothing staged).
- Network: `git push` / `git pull --ff-only` / `git fetch --prune`. Divergent pull → clear error directing to the terminal. Push with no upstream → renderer confirms, then `git push -u origin <branch>`. **Sync button** = pull then push, sequential, with per-step progress and partial-failure reporting (pull may succeed and push fail; the toast says exactly which).

### Follow-active-terminal model

- Focus change → `pty_cwd(focusedId)` → `git_resolve_repo` → sidebar targets that repo. Cwd unresolvable (shell exited, non-macOS info) → keep last-known repo. Not a repo → empty state with the path shown.
- Refresh triggers: focus change, after each action, and a 3s status poll **only while the sidebar is open** (graph refreshes only on action/branch-change/manual refresh; heavy-repo note: poll interval backs off ×2 up to 15s when a status call exceeds 500ms).
- **Race protection:** every refresh carries a generation number and the `repo_id` it was issued for; results are dropped unless both still match the store's current target. One in-flight status request per repo.

### UI (`src/renderer/components/git/`)

Toggleable left sidebar (toolbar button + `Cmd+Shift+G`), width-draggable, themed via the existing theme system. Top→bottom: header (repo name, branch — display-only, ahead/behind `↑2 ↓1`, sync/fetch/refresh buttons) → commit box (multiline + Commit button) → Merge conflicts section (when present; stage-as-resolved) → Staged (unstage per-file/all) → Changes (stage per-file/all, discard with confirm) → Graph (custom SVG lanes, ref badges, subject/author/relative-time rows, virtualized, "Load more").

State: `git-store.ts` on the existing hand-rolled store pattern (`create-store.ts`); no state library. Errors → existing `Toasts.tsx`, expandable stderr detail.

### Graph layout (custom, no deps)

Standard lane assignment over topo-ordered commits (walk newest→oldest; commit takes leftmost lane expecting it or opens one; first parent inherits the lane, other parents open lanes; lanes close on merge). Colors cycle per lane from theme palette. ~150 lines, pure function `Vec<CommitNode> → rows/edges`, fully re-run on any input change (see "load more" above). Unit-tested against fixture histories: linear, branch+merge, octopus, orphan branches.

### Testing (git view)

- Rust: porcelain-v2 parser fixtures covering spaces/tabs/newlines/Unicode in paths, rename/copy, staged+unstaged same file, submodules, all unmerged XY combos, untracked dirs; log parser fixtures; graph layout fixtures; action round-trips against throwaway repos in temp dirs (stage/unstage/commit; unborn-HEAD unstage including the stage-then-modify-again case; rename stage/unstage round-trips (both paths); discard incl. untracked dir, worktree-rename pair, and copy (destination only — source modifications preserved); copy stage touches destination only; submodule discard rejected; literal-pathspec handling of `:`-prefixed filenames; path revalidation rejects stale requests; over-cap output returns an explicit error).
- Renderer: git-store tests (vitest, mocked bridge) for focus-follow, generation-based race dropping, busy-state gating.
- Manual: real repo with staged+unstaged+untracked+conflicts; push/pull against a real remote; no-upstream push; detached HEAD; unborn repo; repo with no remote; large repo poll backoff.

## Non-goals (v1)

- Diff viewer — future work (first candidate after v1).
- Branch create/switch/merge UI, stash, history filtering, multi-repo simultaneous views, interactive rebase, pick-ours/theirs conflict resolution — terminal territory.
- Non-UTF-8 path actions (displayed, not actionable).
- Settings (`localStorage`) migration from the Electron build — deliberate one-time reset.
- Windows/Linux support.

## Rollout

1. `feat/tauri-port`: scaffold → **spike: byte-channel PTY + WKWebView xterm smoke (risks 1–2)** → full backend → bridge + the two renderer edits → parity smoke checklist → CI/release/README updates → remove Electron → merge.
2. `feat/git-view` (after port merges): process-hygiene layer + repo registry → status/read-only sidebar → stage/unstage → commit → network actions → discard w/ validation → graph → polish → merge.
