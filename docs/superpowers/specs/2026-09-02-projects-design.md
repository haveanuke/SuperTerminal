# Projects that survive a quit

**Goal:** reopen a project after closing SuperTerminal.

## The problem, measured

A "project" today is a `Tab`: an id, a label, and split trees of panes. It has
no directory of its own. When its last terminal closes, `tabs.remove(index)`
deletes it outright — the project does not go quiet, it ceases to exist.

Nothing auto-saves and nothing restores at launch. `SessionManager` exists but
is a manual named-session feature; the sessions directory on this machine is
empty, so it has never been used. **Quitting loses every project and every
directory, and the next launch is one blank `terminal` tab.**

## A project is several folders

This is the shape the user's own work takes: one chat project spanning `/chat`,
`/board-kid`, `/penpot` and `/forgejo`. Modelling a project as one directory
would split that into four, which is wrong in the way that matters — it is one
thing in the user's head.

```
Project {
    id: String,
    label: String,
    dirs: Vec<PathBuf>,
    pinned: bool,
    last_opened: SystemTime,
    icon: ProjectIcon,
}
```

Recent and pinned are the same record with a flag, which is why building either
gets the other nearly free.

- **Recent** — captured automatically, unpinned, sorted by `last_opened`, capped.
- **Pinned** — kept indefinitely, never evicted by the cap.
- **Reopen** — one terminal per directory in `dirs`.

## Decisions, with their reasons

**Live cwd, not spawn cwd.** `TerminalPane::cwd()` reports where the shell
actually is. Recording the directory a terminal was SPAWNED with would keep
sending the user to the top of a repo they never work in.

**Only local panes contribute directories.** `cwd()` is `None` for a remote
pane by construction, and that is correct rather than a gap: a peer pane's
directory exists on ANOTHER machine, so reopening it here would silently spawn
a local shell in a path that may not exist, or worse, may exist and be
something else entirely.

**Identity is the primary directory** — the first directory in `dirs` that is
not `$HOME`. Opening `~/projects/foo` today, quitting, and opening it again
tomorrow must UPDATE one entry rather than accumulate duplicates. The full set
is still stored, and reopening still spawns one terminal per directory; the set
just does not decide *which* project this is.

An earlier draft made identity the whole SET, and that forked a project on
entirely ordinary use. A tab with the repo open and a second terminal sitting
in `~` to run `brew upgrade` records `{repo, ~}`. The same tab tomorrow,
without that second terminal, records `{repo}` — a different set, so a second
record, and recents fill with near-duplicates that differ only by which
incidental terminal happened to be open at close time. `cd`-ing that second
terminal from `~` to `/tmp` forks it a third time.

`$HOME` is skipped rather than allowed to be a primary because every launch
opens a starter tab there: a `$HOME` primary would make every one of them the
same project. A capture with no non-`$HOME` directory has no primary and
matches nothing — which changes nothing in practice, since the rule below
already refuses to record it.

The cost, and it is a real one: **two genuinely separate projects rooted in the
same folder now merge into one record.** That is a deliberate call — a project
is the folder you work in, and the terminals beside it come and go — not an
oversight. A consequence of the same rule: the folders AFTER the primary may be
reordered, added or dropped freely, but a capture that puts a *different*
folder first is a different project.

A pinned or renamed project keeps what the user made theirs — its `id`, its
`label`, its `dirs` — and an auto-capture never overwrites those. It does move
its `last_opened`, because that is what "you just used this" means, and an
earlier draft that froze it was wrong twice over: `pinned()` sorts on
`last_opened`, so the pinned list's order would be stuck at pin time forever,
and every reopen would leave a second unpinned copy of the same project sitting
in recents.

**Directories match case-insensitively.** macOS volumes are case-insensitive by
default and both spellings genuinely occur, since a shell's `cd` preserves
whatever was typed — this session's own environment carries
`/Users/…/Documents/…` and `/Users/…/documents/…` for one directory. Matching
case-sensitively would record one project twice. Not `canonicalize`: it touches
the filesystem, fails for a directory that has since been deleted (which a
remembered project must outlive), and resolves symlinks, silently merging
projects a user deliberately keeps apart.

**A project's directories are deduped on capture.** Two panes in one folder are
one folder; storing it twice would make reopening spawn two shells in the same
place.

**Reopening spawns the shells.** Four directories means four terminals,
immediately. Listing them and spawning on demand is lighter but answers the
wrong request: "reopen my project" should return the project, not a menu.

**The label is the user's.** A folder basename is a fine default for one
directory and useless for four — "chat" is not derivable from those four paths.
Default to the first directory's basename; keep whatever the user renames it to.

## Stats a project carries

Orca's "Welcome back" dashboard shows agent count and runtime hours per
project, and that metadata is most of what makes a resume screen feel like it
knows you. Two fields, added while the record's shape is still soft rather than
retrofitted:

- `terminals: usize` — how many terminals the project had when it was last
  captured. Answers "how big is this thing" before you open it. Every pane the
  tab ever had, not the ones still alive when it died — see "A project is the
  union of its panes" below, which is the same population its `dirs` come from,
  so the two halves of a row cannot disagree.
- `active_secs: u64` — accumulated wall-clock time the project has been open,
  summed across sessions. Answers "how much have I actually worked here",
  which is what separates a real project from a folder visited once.

Both are captured, never entered. `active_secs` accrues from when a tab is
created (or reopened) to when it is captured; a session that never closes
cleanly loses that increment rather than inventing one.

A note on what NOT to copy: Orca counts agents because a task there IS an
agent in a worktree. Here a terminal is the unit, so counting terminals is the
honest analogue and counting "agents" would mean inferring which panes happen
to be running one.

## Icons

No emoji in this UI. `icons.rs` holds six hand-drawn SVGs and none of them
means "project".

Start with a **generated mark**: the label's first character over a colour
derived from a hash of the label, resolved through the active theme's palette
so it cannot clash with a custom theme. Zero new artwork, every project
distinct immediately, and it cannot fail to render for a directory whose type
nothing recognises.

`icon: ProjectIcon` is a field, so auto-detection (`Cargo.toml` → Rust) and a
hand-picked set both become values written to it later. Starting with
per-language SVGs instead would mean drawing artwork before anything ships and
a blank for every project that is not a recognised language.

## Storage

`~/Library/Application Support/com.tomaspinal.superterminal/projects.json`,
following `settings.rs` exactly: atomic write (tmp + rename), and a missing or
corrupt file loads defaults rather than failing. Losing the recents list must
never be worse than an inconvenience.

## Explicitly deferred

**Restoring each project's split layout.** That touches session save/load and
can mangle a live workspace if it is wrong. One terminal per directory is most
of the value at a fraction of the risk, and `Project` can grow a `layout` field
later without disturbing anything built here.

## Edge cases that need an answer, not a guess

- A directory that no longer exists at reopen time: spawn in `$HOME` and say
  so, rather than failing silently or refusing to open the project. That pane
  still contributes the folder it was ASKED for, not the `$HOME` it landed in,
  for as long as its shell stays in that fallback — otherwise a folder that is
  temporarily missing would drop out of the project's `dirs` and `$HOME` would
  take its place. Once the user moves that terminal somewhere real, live cwd is
  the truth again.
- A project whose panes are all remote: `dirs` is empty. Do not persist it —
  there is nothing to reopen.
- **A tab that only ever sat in `$HOME` is not a project.** Every launch opens a
  starter tab there, so recording it would put "home" at the top of recents
  after any launch-then-quit — the list filling itself with the one entry that
  carries no intent. A project that also contains other directories is kept
  whole, `$HOME` included: the exclusion is about a tab nobody did anything
  with, not about the folder being forbidden.
- **A shell that exited still remembers where it was.** Typing `exit` is the
  commonest way to close a terminal, and it leaves no process whose directory
  can be read — so capture reads a last-known cwd the session caches on a slow
  tick, not the live one. `cwd()` keeps answering "where is it NOW", which is
  `None` for a dead shell and which panels and the folder picker rely on.
- **A project is the union of its PANES, not a snapshot of its survivors.** A
  tab dies when its LAST terminal closes, so a capture that reads whoever is
  still alive records a four-folder project closed one pane at a time as a
  one-folder project with one terminal — and reopens it as a single shell.
  Closing panes individually is completely ordinary, so the workspace keeps,
  per tab, each pane's last known directory, maintained as panes come and go,
  and capture reads that. The union is over panes and never over time: a pane's
  entry is REPLACED when its cwd changes, so a shell that `cd`s around all day
  contributes one directory rather than every directory it ever visited. The
  map dies with its tab, alongside the tab's other per-tab state.
- **A project that is open is not also listed in RECENT.** It is the live tab
  above; listing it twice invites the user to "reopen" what they are looking
  at. It IS still listed in PINNED when pinned — pinning is a promise that the
  project is always in that list, and hiding it the moment it is opened would
  look like pinning had broken. "Open" is decided by the same primary-directory
  rule identity uses, so a tab that has since picked up an extra terminal
  somewhere still counts as the project it is.
- The cap evicts the oldest UNPINNED project only, and never the capture that
  was just recorded: `last_opened` comes from the system clock, so a store
  carrying future timestamps (skew, a restored backup, a hand-edited file) would
  otherwise make every new capture the oldest and discard it permanently.
- Pinning applies to a project whether it is currently open or sitting in
  recents.

## What it grows into

Every later feature hangs off the same record: per-project auto-run
(`auto_run`), project-scoped peer sharing (`shared_with`), full layout restore
(`layout`), auto-detected icons (overwrites `icon`).
