# Scrollback that survives a quit

**Goal:** reopening a project shows what its terminals said, not empty shells.

Orca's own wording is "scrollback restored on restart", and it is the most
visceral difference between reopening a project there and here: theirs comes
back looking like you left it, ours spawns fresh prompts. Reopening a project
that has forgotten every command you ran is a weaker promise than it sounds.

## What is saved

The text of each terminal, and only the text.

`RenderableSnapshot` already carries the shape this needs — a grid of
`SnapshotCell`, plus `history_rows` for the companion, capped at
`HISTORY_TAIL = 150`. Reuse that cap: it is already the number this codebase
considers "a few screens", the companion has run on it for months, and a second
scrollback limit would be a second thing to reason about.

**Not saved, deliberately:**

- **Live process state.** A restored pane is DEAD — it has no shell, and this
  work does not change that. It shows what was there; it does not resume it.
  Anything else is a promise the code cannot keep.
- **Selection, search, scroll offset.** Transient view state, not content.
- **Colour as theme-resolved values.** Store `CellColor` as-is. Resolving to hex
  at save time would freeze the old theme into the file, so a restored pane
  would keep painting last month's palette after a theme change.

## Where it is saved

Per terminal, under the project store's directory, one file per terminal id —
NOT inside `projects.json`.

A grid of 150 rows x 200 columns of styled cells is on the order of tens of
kilobytes per terminal. Inlining that into `projects.json` would make the file
the list is read from at every launch tens of megabytes for a busy user, so
listing projects would pay for content nobody has asked to see yet. Separate
files mean the list stays small and a terminal's text is read only when its
project is actually reopened.

## The rules that need deciding, not defaulting

**A restored pane is dead and says so.** It already has the vocabulary: a
restored ssh pane is built through `TerminalPane::dead`. The pane shows its old
text with the existing exited/dead treatment, and typing into it does nothing
until the user starts a shell. **A restored pane that looked live would be the
worst outcome here** — the user types a command, sees it echo into a grid that
is a picture, and believes it ran.

**Saving happens where capture already happens.** Task 2 established three
tab-destroying paths plus `shutdown_all` and the quit hook. Scrollback saves on
exactly those, never on a new one. A sixth path that saves text but not the
project, or the reverse, is the failure this codebase has produced six times.

**A missing, corrupt or oversized file loads nothing and the pane opens
empty.** Same discipline as `settings.rs` and `projects.json`: losing scrollback
must never be worse than an inconvenience, and must never be worse than not
having the feature.

**Files are reaped.** A terminal id whose project is no longer in the store
leaves a file nobody will ever read. Evicting a project deletes its terminals'
files, or the directory grows without bound for the lifetime of the install.

## Cost, stated rather than discovered

Saving runs on the same paths as project capture, which are user-initiated
(closing a tab, quitting) rather than periodic — so this adds no polling. The
write is bounded by 150 rows per terminal.

## Explicitly deferred

Resuming a live shell in a restored pane, and restoring the split ARRANGEMENT
rather than one terminal per directory. Both are the same deferral the projects
spec already made, for the same reason: they touch session save/load, which can
mangle a live workspace.
