# Projects that survive a quit — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** reopen a project, with all of its folders, after closing SuperTerminal.

**Spec:** `docs/superpowers/specs/2026-09-02-projects-design.md`

**Architecture:** a `projects.json` store beside `settings.json`, written when
projects change and read at launch. A project is a label plus a LIST of
directories (the user's own work spans four repos in one project). Closing the
last terminal in a tab stops deleting it and instead records it. The sidebar
grows a pinned section and a recent section; clicking either reopens a terminal
per remembered directory.

## Global Constraints

- `gpui` and `alacritty_terminal` pinned with `=`, never forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate. No new crates.
- **No emoji in rendered UI** — `native/src/icons.rs` holds the icon set.
- `cargo fmt` before every commit. `cargo test` from the REPO ROOT.
  **Baseline: 761 passing, 0 failing.** Verify by grepping `test result: FAILED`
  explicitly — summing "N passed" hides failures and has already misled once.
- **No gpui test harness exists and none may be introduced.** Extract every
  decision into a pure function and test that. Say plainly which paths are
  verified by reading; never write a test that pretends to cover wiring.
- Sabotage every new test: break the behaviour it names, confirm it fails,
  restore. A test that survives sabotage of the code it names must be deleted,
  not kept.
- **Losing the projects file must never be worse than an inconvenience** — a
  missing or corrupt file loads defaults, exactly as `settings.rs` does.

---

### Task 1: The store

**Files:**
- Create: `native/src/projects.rs`
- Modify: `native/src/main.rs` (module decl)

**Produces:** `Project { id, label, dirs: Vec<PathBuf>, pinned: bool,
last_opened: u64, icon: ProjectIcon }`, `ProjectStore` with `load`/`save`,
`record(project)`, `pinned()`, `recent()`, and `projects_path()`.

Pure and file-backed only — no gpui, no workspace. Follow `settings.rs`
exactly: atomic write (tmp + rename), missing or corrupt loads defaults.

`last_opened` is a unix timestamp (`u64`), not `SystemTime` — it is serialized,
compared and sorted, and a plain integer keeps the JSON readable and the
ordering total.

**The rules that need encoding, each with a test:**
- `recent()` returns unpinned projects newest first, capped at 10.
- `pinned()` returns pinned projects and is NOT subject to the cap.
- Recording evicts only the oldest UNPINNED project past the cap.
- **Identity is the directory SET for an auto-captured project**: recording a
  project whose `dirs` match an existing unpinned, unrenamed record UPDATES
  that record's `last_opened` rather than adding a duplicate. Order within
  `dirs` must not defeat the match.
- A record the user has PINNED or RENAMED keeps its `id` and is never merged
  into by the rule above — once it is theirs, its identity is the id.
- A project with an empty `dirs` is not recorded at all: there is nothing to
  reopen.

- [ ] **1: failing tests** — [ ] **2: observe** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(projects): a store that outlives the app`

---

### Task 2: Capture, and stop deleting

**Files:**
- Modify: `native/src/workspace/mod.rs`

`close_terminal` currently calls `self.tabs.remove(tab_index)` when a tab's last
terminal goes (`mod.rs:1870`, and the sibling at `:1923`). Both must record the
project before the tab disappears.

**Directories come from LOCAL panes only.** `TerminalPane::cwd()` is `None` for
a remote pane by construction, and that is correct rather than a gap: a peer
pane's directory exists on ANOTHER machine, so reopening it here would spawn a
local shell in a path that may not exist — or worse, may exist and be something
else. Dedupe, preserve first-seen order.

Also record on app quit, so a project open at the time is not lost.

Extract the decision — "given these panes' targets and cwds, what dirs does this
project have?" — as a pure function and test it, including: a tab of only
remote panes yields nothing; duplicates collapse; order is stable.

- [ ] **1: failing tests** — [ ] **2: observe** — [ ] **3: implement** — [ ] **4: `cargo test`; a tab with live terminals behaves exactly as before** — [ ] **5: commit** `feat(projects): remember a project when its last terminal closes`

---

### Task 3: The list, reopening, and what a project shows about itself

**Files:**
- Modify: `native/src/workspace/mod.rs`

A pinned section, then a recent section, in the existing projects sidebar.
Clicking either spawns one terminal per remembered directory in a new tab
labelled with the project's label.

**Each row also carries what the project is**, not just its name: its folder
count, its terminal count, and how long it has been worked in. `Project` gains
`terminals: usize` and `active_secs: u64` (see the spec's "Stats a project
carries"), both captured rather than entered. `active_secs` accrues from tab
creation or reopen until capture; a session that never closes cleanly loses
that increment rather than inventing one.

Format the numbers in a pure function and test it: zero, one and many for each;
sub-minute durations reading as such rather than "0h"; and a project captured
twice accumulating rather than overwriting its time.

**A directory that no longer exists** spawns in `$HOME` with a visible note
rather than failing silently or refusing the whole project — the other three
folders of a four-folder project must still open.

Test the pure parts: which section a project belongs in, the order within each,
and the fallback decision for a missing directory. Say plainly that the click
wiring itself is verified by reading.

- [ ] **1: failing tests** — [ ] **2: observe** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(projects): reopen a project and its folders`

---

### Task 4: Pinning

**Files:**
- Modify: `native/src/workspace/mod.rs`, `native/src/projects.rs`

A pin control on each project row. Pinning applies whether the project is open
or sitting in recents. Pinning a recent project moves it to the pinned section
and exempts it from the cap; unpinning returns it to recents with its
`last_opened` intact rather than resetting it.

- [ ] **1: failing tests** — [ ] **2: observe** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(projects): pin a project to keep it`

---

### Task 5: Icons

**Files:**
- Modify: `native/src/projects.rs`, `native/src/workspace/mod.rs`

A generated mark: the label's first character over a colour derived from a hash
of the label, resolved through the ACTIVE THEME's palette so it cannot clash
with a custom theme. No emoji, no new SVG artwork.

`ProjectIcon` stays a field so auto-detection (`Cargo.toml` → Rust) and a
hand-picked set can later write to it without changing anything built here.

Test: the same label always yields the same slot; different labels spread
across the palette rather than collapsing onto one; an empty or
whitespace-only label still yields a mark rather than a blank.

- [ ] **1: failing tests** — [ ] **2: observe** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(projects): a generated mark per project`

---

## Seams to check at the whole-branch stage

Every serious defect in the last phase lived BETWEEN two individually-correct
tasks, so these get checked deliberately rather than assumed:

- **1 to 2** — the store's identity rule versus what capture actually passes it:
  a project recorded on close and again on quit must not duplicate.
- **2 to 3** — dirs captured from live panes versus dirs read back at launch;
  a path with a space, a symlink, or a trailing slash must round-trip.
- **3 to 4** — reopening a PINNED project must not re-record it as a new recent.
- **2 to quit** — the quit path and the close path both record; whichever runs
  second must not undo the first.
- **Cross-cutting** — `tabs.remove` has two call sites (`:1870`, `:1923`) and
  this phase's recurring failure was fixing one instance and missing its
  sibling. Both must record.
