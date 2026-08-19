# Native Rewrite — Research Brief (workflow synthesis)

_Generated 2026-08-19 by a 7-agent research workflow; feeds the implementation plan._

# SuperTerminal Native — Merged Implementation Brief

Synthesis of 5 research briefs (gpui API, alacritty_terminal 0.26.0, Zed's integration, core extraction map, workspace plan). Feeds directly into the implementation plan. Contradictions resolved inline; unresolved ones flagged in §6.

---

## 1) Pinned versions + dependencies

| Crate | Pin | Notes / citations |
|---|---|---|
| `gpui` | `= "0.2.2"` (crates.io, released 2025-10-22) | Apache-2.0, repo `github.com/zed-industries/zed` (`crates/gpui`); docs at docs.rs/gpui/0.2.2. **Resolved contradiction:** Brief 5 assumed a git dep on zed; Brief 1 shows git `main` has split into `gpui` + unpublished `gpui_platform` with a *different bootstrap* (`gpui_platform::application()`, `font-kit` feature required for text). Decision: **pin crates.io 0.2.2** — the §2 recipe below is 0.2.2-shaped, fonts work without feature flags, and the dep tree is fetchable without a zed checkout. If a git-only API becomes necessary, pin an exact `rev` and rewrite the bootstrap; do not track `main`. |
| `alacritty_terminal` | `= "0.26.0"` | crates.io, verified Aug 2026; pairs with alacritty 0.17.x. `rust-version = 1.85.0`. Re-exports `vte 0.15.0` as `alacritty_terminal::vte` (ANSI types come from `vte::ansi`). Deps it brings: `polling 3.8`, `parking_lot 0.12`, `rustix_openpty`, `signal-hook`. |
| `superterminal-core` | `{ path = "../core" }` | New crate, §4. Deps: `serde`, `serde_json`, `portable-pty`. No tauri, no gpui. |
| channels for the event pump | `futures` (`channel::mpsc::unbounded`) | Already in gpui's tree (Zed uses `futures::channel::mpsc::unbounded` for `ZedListener`). Per the minimal-deps preference: no tokio, no crossbeam — gpui's executor is smol-based and tokio is explicitly incompatible (Brief 1 §11.2). |

Toolchain: Rust ≥ 1.85 (alacritty_terminal MSRV); macOS build requires **real Xcode** (Metal shader toolchain), not just CLT (Brief 1 §11.7).

`native/` does **not** depend on `portable-pty` — it uses `alacritty_terminal::tty` (see §3, and the flagged unification question in §6).

---

## 2) App bootstrap recipe (gpui 0.2.2)

Skeleton (adapted from `crates/gpui/examples/hello_world.rs`, verbatim API):

```rust
use gpui::{App, Application, Bounds, Context, TitlebarOptions, Window, WindowBackgroundAppearance,
           WindowBounds, WindowOptions, div, point, prelude::*, px, size};

fn main() {
    Application::new()            // .with_assets(Assets) once SVG icons exist
        .run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("SuperTerminal".into()),
                        appears_transparent: true,                       // content under titlebar
                        traffic_light_position: Some(point(px(9.), px(9.))),
                    }),
                    window_background: WindowBackgroundAppearance::Blurred, // macOS vibrancy
                    window_min_size: Some(size(px(200.), px(100.))),
                    ..Default::default()
                },
                |_window, cx| cx.new(|cx| TerminalView::new(cx)),
            ).unwrap();
            cx.activate(true);   // REQUIRED or the window opens behind other apps
        });
}
```

Key facts for the plan:
- `.run` never returns; `App` is `!Send`; all state lives in entities: `cx.new(|cx| T) -> Entity<T>`; a "view" is `Entity<V> where V: Render`. `Context<T>` derefs to `App`. (0.2.2 names — `View`/`Model`/`ViewContext` tutorials online are stale, Brief 1 §11.1.)
- `Render::render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement` runs every frame the window redraws — keep it cheap, precompute in the entity.
- Focus: create `FocusHandle` via `cx.focus_handle()`, attach `.track_focus(&handle)` + `.key_context("Terminal")` on the root div, focus with `window.focus(&handle)`.
- Actions/keymap for chords: `actions!(terminal, [Copy, Paste, Clear])`; `cx.bind_keys([KeyBinding::new("cmd-c", Copy, Some("Terminal"))])`; handle via `.on_action(cx.listener(...))`. Unhandled keys fall through to `on_key_down` — that's the PTY write path.
- Menus via `cx.set_menus(Vec<Menu>)`; blurred background pairs with an alpha `bg` on the root div; render your own drag region (`window.start_window_move()`, `window.titlebar_double_click()`).
- Observe `window.appearance()` for light/dark switching.

---

## 3) Terminal integration recipe

Architecture (Zed-proven, Brief 3): **alacritty's own I/O thread + `Arc<FairMutex<Term>>` + snapshot; gpui frames are event-driven, never polled.**

### 3a. Threading + wiring

Wiring order (Brief 2, signatures verified against vendored 0.26.0 source at `~/.cargo/registry/src/.../alacritty_terminal-0.26.0/src/`):

```rust
tty::setup_env();  // process-global: TERM=alacritty|xterm-256color, COLORTERM=truecolor — BEFORE spawn

let size = term::test::TermSize::new(cols, lines);          // public, not test-gated
let term = Arc::new(FairMutex::new(Term::new(term::Config {
    scrolling_history: 10_000,           // default
    kitty_keyboard: false,               // we implement classic xterm encoding only (see 3c)
    ..Default::default()
}, &size, listener.clone())));

let pty = tty::new(&tty::Options {
    shell: None,                          // macOS default: /usr/bin/login -flp <user> ... (login shell, utmp)
    working_directory,
    drain_on_exit: true,
    env,                                  // ALACRITTY_WINDOW_ID/WINDOWID injected from window_id arg
}, win_size, window_id)?;

let ev = EventLoop::new(term.clone(), listener, pty, /*drain_on_exit*/ true, /*ref_test*/ false)?;
let sender = ev.channel();                // get BEFORE spawn; keep ≥1 alive or the loop PANICS
let notifier = Notifier(sender.clone());
let _io_thread = ev.spawn();              // OS thread named "PTY reader"
```

- **Listener → gpui bridge:** implement `EventListener` as a wrapper over `futures::channel::mpsc::UnboundedSender<Event>` (Zed's `ZedListener`). Events fire **on the PTY thread while the term lock is held** — the impl must be a cheap non-blocking send, `Send + 'static`, `&self` (Brief 2).
- **Must-service events** (or query responses silently break): `PtyWrite(s)` / `ColorRequest` / `TextAreaSizeRequest` → write the produced string back via `notifier.notify(...)`. Also handle `Title`, `Bell`, `ClipboardStore/Load` (OSC 52), `CursorBlinkingChange`, `ChildExit`/`Exit`.
- **gpui pump** (Brief 1 §8 + Brief 3 §1): drain the channel in an entity-scoped task —

```rust
cx.spawn(async move |this: WeakEntity<Terminal>, cx: &mut AsyncApp| {
    while let Some(event) = rx.next().await {
        // Zed batching (crates/terminal/src/terminal.rs ~1352): handle first event immediately,
        // then coalesce up to 4ms (select_biased! vs timer) or 100 events; fold Wakeups into one bool.
        this.update(cx, |term, cx| { term.process_events(batch); cx.notify(); })?;
    }
    anyhow::Ok(())
}).detach();   // dropped Task = cancelled task — forgetting .detach() silently kills the pump
```

  The channel send from the PTY thread wakes the waker → platform run loop; `cx.notify()` triggers re-render. This is the sanctioned cross-thread wake — `AsyncApp`/`ForegroundExecutor` are `!Send`, never hand a context to the reader thread.
- **Signal-mask gotcha** (Brief 3, worth copying): if PTY construction happens on `background_spawn`, capture the *foreground thread's* signal mask first and restore it for the child, or Ctrl-C breaks in the shell.
- **Shutdown:** app-initiated `sender.send(Msg::Shutdown)` → join → drop `Pty` (its `Drop` sends SIGHUP + `wait()`). Child-initiated: `ChildExit` → drain → `Event::Exit`.

### 3b. Grid render (custom `Element`)

Implement `gpui::Element` by hand (Zed's `TerminalElement`, `crates/terminal_view/src/terminal_element.rs`, ~3000 lines, is the canonical reference; the `canvas()` helper is fine for a first spike).

- **Snapshot, not live grid:** a `Content` struct (Brief 3 §2) cached on the entity: `Vec<IndexedCell>` (cloned visible cells — `Cell.extra` is `Arc`, cheap), mirrored `TermMode` bitflags, cursor (shape+point), `display_offset`, selection + `selection_text`, dims. Built by locking the term (fair `lock()` on the render side so the PTY thread can't starve it — Brief 2 `sync::FairMutex`), calling `Term::renderable_content()`, copying out, unlocking fast.
- **`prepaint` does the work each frame:**
  1. Metrics: cell width = `text_system.em_advance(font_id, font_size)` ('m' advance, Brief 1 §5); cell height = `font_size × line_height multiplier`; snap origin/height to device pixels (`× scale_factor`, floor, `÷`) to kill resize flicker; enforce **min 2 columns** (`MIN_COLUMNS = 2`; 1-col + wide emoji crashes alacritty — issue #2750, both Briefs 2 & 3).
  2. `terminal.set_size(dims)` — early-out unless rows/cols/cell metrics changed; coalesce by overwriting a pending resize at the back of the deferred-event queue.
  3. `terminal.sync()`: take the lock **once per frame**, drain a `VecDeque<InternalEvent>` (resize/scroll/selection/clear applied under the one lock), rebuild `Content`. Resize is two-legged and order matters (Brief 2): `term.resize(TermSize::new(c, l))` **and** `sender.send(Msg::Resize(WindowSize{..}))` (→ `TIOCSWINSZ` → SIGWINCH).
  4. `layout_grid` over the snapshot → three vectors: **(a)** background `LayoutRect`s, run-length-extended per line then greedily merged (`merge_background_regions`); **(b)** `BatchedTextRun`s — batch while font/color/underline/strikethrough match and cells are contiguous, shaped via `window.text_system().shape_line(text, size, &[TextRun], Some(cell_width))` — the **`force_width` per-cell advance override locks shaping to the grid** (note: `shape_line` panics on `\n`; one line per row); **(c)** box-drawing/block/sextant/quadrant chars rendered as exact sub-cell **quads** (8×24 subcell grid), not glyphs, so blocks tile across fonts.
  5. `window.insert_hitbox(bounds, ...)` for mouse; cursor rect doubles as the IME rect.
- **`paint` order:** bg quad → merged background rects → selection highlight → shaped text runs (`ShapedLine::paint(origin, line_height, ...)`, origin via `baseline_offset`) → block quads → IME marked text (underlined, bg quad over cell text) → cursor (block cursor repaints covered char in bg color; unfocused → hollow) → register mouse listeners + `window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)`.
- **Cells/colors** (Brief 2): skip `WIDE_CHAR_SPACER`/`LEADING_WIDE_CHAR_SPACER`; `Color::Named/Indexed/Spec` resolve through `term.colors()` (`Some` = escape-sequence override, `None` = your palette); apply `INVERSE`/`DIM` (alpha × 0.7)/`HIDDEN` yourself; grid `Line(i32)` is grid-relative (negative = scrollback) — convert with `term::point_to_viewport`.
- **Damage tracking — resolved contradiction:** Brief 2 documents `Term::damage()/reset_damage()`; Brief 3 shows Zed doesn't use it (full-viewport snapshot per event-driven frame). Decision: **skip damage in v1**, adopt Zed's model; damage is a later optimization (and it doesn't cover vi-cursor/selection anyway).
- Cursor blink: loop `cx.background_executor().timer(Duration::from_millis(500)).await` then update entity; honor `CursorBlinkingChange`.

### 3c. Input

- **Keys:** `on_key_down` → port Zed's `to_esc_str(keystroke, mode, option_as_meta)` table (`crates/terminal/src/mappings/keys.rs`): C0 controls, `\x1b[A`/`\x1bOA` arrows switching on `APP_CURSOR`, `\x1b[1;{mod}X` xterm modifier encoding (`1 + shift|alt<<1|ctrl<<2`), F-keys, alt-as-meta ESC prefix (gated on `option_as_meta`). Encoding is your job — alacritty_terminal only transports bytes (Brief 2). `Some` → write bytes + `cx.stop_propagation()`; `None` → fall through to gpui bindings/IME. **Kitty keyboard protocol: not implemented** (Zed doesn't; `Config::kitty_keyboard = false`). Mode-dependent encoding reads the *snapshot's* mirrored `Modes` — no lock.
- Every input also queues `Scroll::Bottom` + clear-selection. Cmd-chords (copy/paste/clear) are gpui actions, not PTY bytes.
- **IME** (dead keys, CJK, press-and-hold — mandatory): implement `EntityInputHandler` (Brief 1 §6; working reference `crates/gpui/examples/input.rs`): `replace_text_in_range` = commit → PTY; `replace_and_mark_text_in_range` = composition → held in view state, painted at cursor, **never sent to PTY**; `bounds_for_range` returns cursor rect for the candidate popup. **All ranges are UTF-16** (vs UTF-8 in `TextRun::len` — the classic gpui off-by-encoding bug).
- **Paste:** bracketed (`\x1b[200~…\x1b[201~`, strip ESC from payload) when `BRACKETED_PASTE`, else LF→CR. Focus in/out sends `\x1b[I`/`\x1b[O` when `FOCUS_IN_OUT`.
- **Mouse:** port `mappings/mouse.rs` — SGR/UTF8/normal encodings by mode flags; press/release/drag/motion gated on `MOUSE_REPORT_CLICK`/`MOUSE_DRAG`/`MOUSE_MOTION`; shift always bypasses mouse mode. Cell hit = `((pos - bounds.origin) / cell_size)`, refined via `LineLayout::closest_index_for_x`. Event `position` is window-relative.
- **Scroll:** accumulate pixel delta → whole lines; mouse mode → PTY reports; alt-screen + `ALTERNATE_SCROLL` → arrow keys; else deferred `Scroll::Delta(n)` (positive = into history) → `term.scroll_display()` at next sync. `display_offset == 0` auto-follows output.
- **Selection/copy:** lives in `term.selection` (public field): click-count → `Simple/Semantic/Lines`, drag → `selection.update(point, side)`; 2px drag threshold; snapshot carries `selection_to_string()` result; copy action writes it to the clipboard; OSC 52 via `ClipboardStore` events.

---

## 4) Core-crate extraction task list (`core/` = `superterminal-core`)

From Brief 4's classification. All source paths under `/Users/tomas/Documents/projects/SuperTerminal/src-tauri/src/`.

**Move verbatim (pure, zero changes):**
1. `session.rs` — validators, `sanitize_name`, `now_rfc3339`, `SessionManager` + 10 tests → `core/src/session.rs`. Tauri commands stay (already thin).
2. `shell_env.rs` — 100% pure, moves whole + 3 tests → `core/src/shell_env.rs`.
3. `buddy.rs` — types, consts, `strip_ansi`, `clean_output`, `run` + 6 tests → `core/src/buddy.rs`; `buddy_react` command stays.
4. `git/process.rs` (engine + 3 tests), `git/status.rs` (parser + 11 tests) — 100% pure, move verbatim.
5. `git/mod.rs` pure parts: `RepoInfo`, `RepoEntry`, `GitState`, `run_status`, `test_repo` fixture (`#[cfg(test)] pub(crate)`), 3 integration tests.
6. `git/graph.rs` pure parts (`parse_log`, `layout`, types, 6 tests); `git/actions.rs` pure parts (`run_action` machinery, 9 tests).
7. `pty.rs::proc_cwd` → `core/src/proc_cwd.rs` as `#[cfg(target_os = "macos")] pub mod` (visibility change only).

**Required signature splits (the 6 refactors):**
1. **`pty.rs` (the only real one):** `PtyManager::create/create_with_shell` drop `Channel<InvokeResponseBody>` for a sink: `pub enum PtyEvent { Data(Vec<u8>), Exit(i32) }`; `sink: impl Fn(PtyEvent) -> bool + Send + 'static` (false = receiver gone). Tauri wire framing (`InvokeResponseBody::Raw`/`Json("{\"exit\":N}")`) moves into the `pty_create` wrapper. `PtyRecord`/`PtySlot` unchanged. Rewrite `test_channel()` over `PtyEvent` + mpsc (7 tests otherwise unchanged).
2. **`bg.rs`:** core `store_background_image_in(src: &str, app_data_dir: &Path)`; wrapper resolves `app.path().app_data_dir()`.
3. **`git/mod.rs`:** extract `status_guarded(state, repo_id)` (busy-guard: `action_lock.try_lock` + `status_inflight` swap) from `git_status`.
4. **`git/graph.rs`:** extract `run_graph(state, repo_id, limit)` (clamp to `MAX_LIMIT`, `git log --all --topo-order -z` invocation, `layout(parse_log(...))`).
5. **`git/actions.rs`:** extract `run_commit(state, repo_id, message)` (lock, empty-message check, commit, fresh status).
6. **`git/network.rs`:** make `network_action` pub; extract `run_push` (set-upstream branch resolution, no-upstream/unborn detection), `run_pull` (`--ff-only` + diverged rewording), `run_fetch`. **Delete the `git_push_inner` test mirror** — tests call core `run_push`.

**Post-split invariants:** all test modules live in core (src-tauri retains zero tests); cross-module deps stay internal (`pty → shell_env + proc_cwd`, `buddy → shell_env`, `git/* → git/process`); serde camelCase derives move as-is and serve both frontends; src-tauri `lib.rs::run()` keeps managing core types as tauri `State<T>` (derefs to `&T`, no changes).

**Native's consumption:** `native` uses core for `session`, `git/*`, `shell_env`, `buddy` — **not** `core::pty` (native's PTY is `alacritty_terminal::tty` + `EventLoop`, §3a; alacritty's `EventLoop` requires its own `EventedPty` type, so `portable-pty` can't back it). See §6 for the unresolved unification question.

---

## 5) Workspace/CI change list

Decision (Brief 5): **root cargo workspace with `default-members`** — eliminates the two-lockfile version-skew hazard that would make "shared core" fiction; all breakages are shallow path edits. Exact edits:

1. New `/Cargo.toml` (virtual manifest — `resolver = "2"` is **mandatory**; virtual roots default to resolver 1):
   ```toml
   [workspace]
   resolver = "2"
   members = ["src-tauri", "core", "native"]
   default-members = ["src-tauri", "core"]   # bare `cargo test`/`build` skip gpui
   ```
2. New crates: `core/` (`name = "superterminal-core"`, lib), `native/` (`name = "superterminal-native"`, bin; `gpui = "=0.2.2"`, `alacritty_terminal = "=0.26.0"`, `superterminal-core = { path = "../core" }`).
3. `src-tauri/Cargo.toml`: add `superterminal-core = { path = "../core" }`. Nothing else; `[lib]`/`crate-type` stay.
4. **`git mv src-tauri/Cargo.lock Cargo.lock`**, then `cargo check` to fold in new members. Never let cargo generate a fresh root lockfile (silent full re-resolution).
5. Root `.gitignore`: add `/target/` (`src-tauri/.gitignore`'s `/target` becomes harmless; optionally delete).
6. `.github/workflows/ci.yml`: rust-cache `workspaces: src-tauri` → `workspaces: .`; line 35 `cargo test --manifest-path src-tauri/Cargo.toml` → `cargo test` at root (covers default members); add `cargo check -p superterminal-native` as a separate step (native compiles in CI without gpui test cost).
7. `.github/workflows/release.yml`: rust-cache → `workspaces: .`; line 36 `files: src-tauri/target/release/bundle/dmg/*.dmg` → `target/release/bundle/dmg/*.dmg`.
8. `README.md:33`: `src-tauri/target/release/bundle/` → `target/release/bundle/`.
9. Hygiene: delete stale `src-tauri/target/` after first workspace build.

Non-breakages to record in the plan: Tauri v2 CLI reads `target_directory` via `cargo metadata` → `tauri dev`/`build` need **zero** `tauri.conf.json` changes; `beforeDevCommand`/`frontendDist` unaffected; `--manifest-path` still works but stops covering new crates.

Verification: `npm run dev` (binary under root `target/debug/`), `cargo test` at root, `npx tauri build` → DMG in `target/release/bundle/dmg/`.

---

## 6) Open risks, ranked

1. **gpui pre-1.0 churn / crates.io↔git divergence.** 0.2.x already renamed the entire API surface; git main's `gpui_platform` split is the next breakage wave, and crates.io cadence is slow (nothing since Oct 2025). Mitigation: exact-pin 0.2.2, write bootstrap for 0.2.2 only, treat any move to git as a deliberate migration with a pinned `rev`. *(Partially unresolved: if the plan discovers a needed git-only API — e.g. newer window APIs — the bootstrap must be rewritten; decide before coding, per Brief 1 §11.10.)*
2. **Silent terminal death via dropped `Task`.** A gpui `Task` dropped without `.detach()` cancels the PTY event pump with no error. Mitigation: `.detach()` or store every task; smoke-test that output still flows after window interactions.
3. **Unanswered PTY protocol events.** `PtyWrite`/`ColorRequest`/`TextAreaSizeRequest` must be written back to the PTY or DSR/OSC-color/CSI 14 t queries silently break (vim, fzf misbehave). Listener also runs on the PTY thread under the term lock — it must never block or lock the term.
4. **UTF-16 vs UTF-8 range bugs in IME.** `EntityInputHandler` ranges are UTF-16; `TextRun::len`/`LineLayout` are UTF-8 bytes. The classic gpui text bug; needs targeted tests (option-e dead key, CJK composition, press-and-hold).
5. **Lock discipline / frame stalls.** Render side must use fair `lock()` once per frame, copy the snapshot, release fast; user ops must go through the deferred `InternalEvent` queue, or PTY-thread starvation and UI jank follow.
6. **CI/build cost of gpui.** Huge dep tree in the shared lockfile + feature unification between gpui's and tauri's shared deps (usually benign, occasionally forces recompiles/conflicts). Mitigated by `default-members` + `cargo check -p superterminal-native` + root rust-cache; first cold CI run will still be long.
7. **Signal-mask inheritance.** Spawning the PTY from a gpui background thread without restoring the foreground signal mask breaks Ctrl-C in the child shell (Brief 3). Copy Zed's capture-and-pass into `tty::Options` behavior.
8. **EventLoop channel panic.** If every `EventLoopSender` drops before `Msg::Shutdown`, the "PTY reader" thread panics (`"event loop channel closed"`). Keep the `Notifier`/sender alive for the terminal's lifetime; shutdown = `Msg::Shutdown` → join → drop `Pty` (SIGHUP + reap).
9. **Dual PTY stacks — UNRESOLVED.** After the split, the repo has two PTY implementations: core's `portable-pty` `PtyManager` (tauri path) and alacritty_terminal's `rustix_openpty` (native path). They differ in spawn semantics (alacritty's macOS default is `/usr/bin/login -flp` login shell + utmp; `PtyManager` uses `shell_env`). No consolidation is possible while both frontends live (alacritty's `EventLoop` requires its own `Pty`); decide at tauri-retirement time whether `core::pty`/`portable-pty` gets deleted. Also unresolved: whether native reuses core's `shell_env` for child env vars or trusts alacritty's login-shell env entirely.
10. **Min-size / degenerate grid crashes.** `MIN_COLUMNS = 2`, `MIN_SCREEN_LINES = 1` must be enforced in prepaint (1-column + wide emoji crash, alacritty issue #2750); `window_min_size` alone is insufficient (embedded/tiny layouts).
11. **Grid quality regressions vs references.** Wide-char spacers, block-char quad rendering, background-region merging, device-pixel snapping, and `force_width` shaping are each easy to get subtly wrong; the acceptance bar is side-by-side parity with Zed's terminal on `vttest`, `htop`, and a nerd-font prompt. Canonical references: Zed `crates/terminal/src/{terminal.rs,alacritty.rs,mappings/*}`, `crates/terminal_view/src/terminal_element.rs`; gpui `examples/{hello_world,input,painting,uniform_list}.rs`; vendored `alacritty_terminal-0.26.0` source.

---

## Completeness critique

Spot-check note: I verified the brief's load-bearing API claims against the vendored sources (`~/.cargo/registry/.../alacritty_terminal-0.26.0`, `gpui-0.2.2`) — `EventLoop::new`, `tty::Options`/`tty::new(_, _, window_id: u64)`, `term::test::TermSize` (public, not cfg-gated), `Config.kitty_keyboard`, `shape_line(.., force_width)`, `em_advance`, `EntityInputHandler`, `cx.spawn(AsyncFnOnce(WeakEntity<T>, &mut AsyncApp))` — all hold. The gaps below are omissions, not errors.

1. **Zed reference revision unpinned.** Every porting task cites Zed files and line numbers ("terminal_element.rs ~3000 lines", "terminal.rs ~1352", `mappings/keys.rs`, `mappings/mouse.rs`) with no commit/tag, while the brief itself establishes that zed `main`'s gpui has diverged from 0.2.2 (`gpui_platform` split). The plan needs a pinned zed rev whose vendored gpui matches 0.2.2 (e.g. the commit that published 0.2.2), or the "port Zed's X" tasks reference code that won't compile against the pinned API and the line citations are meaningless.

2. **Native v1 feature scope beyond the grid is unstated.** §4 says native consumes core `session`, `git/*`, `buddy`, `shell_env`, but there are zero requirements for any of them: single terminal vs multiple sessions/tabs, whether SessionManager persistence, git UI, or buddy appear in native v1 at all, scrollbar/search/URL-click presence. Milestones and the entity architecture (one PTY or N) cannot be planned without this decision.

3. **Font pipeline unspecified.** The metrics recipe assumes a `font_id` but the brief never states how it's obtained (`text_system().resolve_font(&Font)` exists in 0.2.2 but is never mentioned), which font family ships (bundled vs system default vs configurable), the source of the line-height multiplier, per-cell bold/italic → `Font` variant mapping for `TextRun`s, or gpui 0.2.2's fallback behavior for nerd-font glyphs/emoji — despite "nerd-font prompt parity" being the stated acceptance bar (§6.11).

4. **Entity/ownership architecture is unstated and internally inconsistent.** §2 bootstraps `TerminalView`; §3 describes a `Terminal` entity owning `process_events`/`sync()`/`set_size` and the deferred `InternalEvent` queue. One entity or two (Zed splits them)? Who owns `Arc<FairMutex<Term>>`, the `Notifier`, and the pump task; how does the hand-written Element reach the entity during prepaint (handle captured at render? `update` inside layout)? This is the central ownership/threading contract and the plan can't lay out structs without it.

5. **PTY spawn bootstrap ordering.** `Term::new` and `tty::new` need cols/lines plus a pixel `WindowSize` before the first prepaint has computed cell metrics. Unstated: initial dims (default 80×24 then resize, or defer spawn to first layout), where the `u64 window_id` argument comes from in gpui, and — explicitly deferred in §6.9 but still required for v1 — what `env` and `working_directory` native passes (trust `/usr/bin/login` env vs reuse core `shell_env`).

6. **Palette and settings provenance.** "`None` = your palette" with no palette defined: the 16 ANSI + bright/dim variants, 256-color handling, default fg/bg/cursor/selection colors, and whether native ports the existing app's themes (e.g. Monokai Pro Spectrum) or hardcodes one. Similarly `option_as_meta`, font size, and blink defaults are referenced as free variables with no stated configuration source (hardcoded consts? shared settings file in core?).

7. **gpui 0.2.2 `Element` trait shape never stated.** The brief mandates a hand-written `Element` and details prepaint/paint responsibilities, but gives no API facts for the trait itself (associated `RequestLayoutState`/`PrepaintState` types, `request_layout`'s interaction with the parent flex layout, how the element receives its `Bounds`) — the one place it defers entirely to references, which gap 1 makes unreliable.

8. **Window/app lifecycle ↔ PTY shutdown wiring.** §3a defines the PTY-side shutdown protocol but not the gpui side: which 0.2.2 hook observes window close/app quit to send `Msg::Shutdown` and join before drop, and the required behavior on `ChildExit`/`Event::Exit` (close the window, show "process exited", respawn?) is undefined.
