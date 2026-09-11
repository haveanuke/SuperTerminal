# Project rows that read like Orca's

**Goal:** the projects sidebar should say what a project IS at a glance — its
state, its branch, what is happening in it — the way Orca's does.

**Direction:** Orca is the functionality target for projects, git and folders;
the phone companion and the buddy are the parts that stay ours.

## What Orca actually draws

From its own screenshots, one sidebar entry is:

```
● codex-pane                              status dot + name, bright
  brennanb2025/codex-pane                 branch, muted, second line
┌──────────────────────────────────────┐
│ c Agent Statuses                     │  selected = rounded card, lighter bg
│   brennanb2025/agent-status-demo     │
│   ⌄ AGENTS (2)                       │  collapsible, tiny caps, muted
│     ✳ What does orca do?   just now  │  icon + message + right-aligned time
└──────────────────────────────────────┘
● codex-hook-test                    ⊘   status dot, name, right-side state
  brennanb2025/codex-hook-test
  ⑂ PR #1547 fix(codex): register hook…  muted detail line
```

The pattern, which is what to copy rather than the pixels:

- **Two lines per entry.** Identity on top, bright. Context beneath, muted and
  smaller. Ours currently puts everything on one line and truncates the name.
- **A status dot leads the row**, so state is scannable without reading.
- **The selected entry is a CARD**, not a highlighted line.
- **Children nest under a tiny uppercase header** with their own icons and
  right-aligned relative timestamps.

## This slice

Rows 1 and 2 of that anatomy. Nested children and the buddy line come after,
and the layout must leave room for them rather than having to be redrawn.

### The second line is the git line

`superterminal_core::git::status::StatusReport` ALREADY parses everything
needed — `branch`, `detached`, `upstream`, `ahead`, `behind`, and `entries`
(the dirty files). Nothing reads it per project yet; that is the whole gap.

What the line says, in priority order, dropping what does not apply:

- the branch, or the short oid when HEAD is detached
- ahead/behind counts against upstream when either is non-zero
- a dirty count when the working tree is not clean
- nothing at all for a folder that is not a repo — the line is simply absent,
  not an empty row or a "not a repo" label

### Cost, stated rather than discovered

**Every git call shells out and can block** — `git_panel`'s own module doc says
so, and it runs all engine work on the background executor. A project row must
never make the UI wait on git. So: a cache keyed by project anchor, refreshed
on a slow background tick, read synchronously by render. Same shape as the
sidebar status cache and the peer session poller that already exist.

A project with no cache entry yet draws its first line and no git line, then
gains one. It must never draw a stale branch for a folder that has since
changed — an entry is dropped when its project leaves the list.

### What NOT to copy

Orca shows a PR number and title. That needs a GitHub integration this app does
not have, and inventing a placeholder row would be worse than leaving it out.
The buddy goes where Orca's AGENTS section goes, in the next slice.
