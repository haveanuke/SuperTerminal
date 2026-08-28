# Remote hosts for SuperTerminal — design

Date: 2026-08-28
Status: APPROVED for implementation. Five rounds of adversarial design review;
the final round found no blocking defect.

Scope note: this document specifies **slice 1** in full and fixes the
architecture the later slices depend on. Slices 2-6 are scoped here but each
gets its own spec before it is built.

## Goal

One MacBook does all the work. A Windows PC with an NVIDIA GPU is joining the
tailnet (possibly a Mac Mini after). Heavy jobs — Hunyuan3D, Blender renders —
should run on a machine that is free and capable rather than queueing on the
laptop.

Settled: SSH transport, not a companion mesh, not a resident agent daemon.
A job is a command plus declared requirements; least-loaded satisfying host wins.
Not a bidding protocol.

## Design decisions, and why they are not the obvious thing

Each of these overturned an earlier, more natural-looking choice. The rationale
is recorded so the reasoning is not lost and the cheaper-looking option is not
reintroduced later.

### D1. SSH options precede the destination

`ssh(1)` synopsis is `... destination [command [argument ...]]`, and arguments
are "appended to the command, separated by spaces, before it is sent to the
server". So `ssh <dest> -- <cmd>` sends the remote shell `-- <cmd>`, which
bash/zsh/sh reject. Verified against the local OpenSSH_10.2p1 man page.

Correct form — every option, including `-l`/`-p`, precedes the destination:

    ssh -tt <options> -- <destination> <command-template>

The `--` before the destination is belt-and-braces against a leading-dash
destination; the validator already rejects those. A unit test asserts the exact
argv vector, and slice 2's E2E asserts the client accepts `--` in that position.

### D2. The nonce crosses in a single-slot, closed-alphabet template

v2 contradicted itself: "remote command is a constant with nothing interpolated"
versus "a per-session nonce is handed to the remote at launch". SSH joins remote
arguments into one shell command, so a structured argv does not survive.

Resolution: the remote command is a per-`ShellKind` template with **exactly one
substitution slot**, filled from a **closed alphabet** — 32 lowercase hex
characters, length-checked at the substitution site. This is not general shell
quoting and cannot express a metacharacter. A unit test per shell asserts the
rendered command equals an exact expected string, and a property test asserts
no generated nonce can alter the command's token count.

### D3. Foreground and companion activity stay distinct

`foreground_busy()` is process ownership; `companion_busy()` (`pane.rs:441`)
additionally folds in agent state, `foreground_pgid()`, and output age. v2 would
have collapsed them and silently changed the phone dot for LOCAL tabs.

Two APIs, both returning `Activity`:

- `foreground_activity()` — cues, `finished`, caffeinate, folder-write
  authorization, sidebar dots.
- `companion_activity()` — the phone's existing agent/output heuristic,
  preserved exactly for local panes.

Remote title telemetry may feed both; local mappings are unchanged.

### D4. Unknown timing and aggregation

`Activity::{Unknown, Idle, Busy}`. `Unknown` is not `Idle`.

- `CueGate`: `Unknown` **resets** `busy_since` to `None`, so an Unknown interval
  never counts toward `MIN_BUSY`. `Busy -> Unknown -> Busy -> Idle` therefore
  requires a fresh `MIN_BUSY` in the second Busy run. Only `Busy -> Idle`
  finishes.
- `finished` counter increments only on `Busy -> Idle`.
- Caffeinate aggregate over panes: any `Busy` -> `Busy`; else any `Unknown` ->
  `Unknown`; else `Idle`. On `Unknown` the `AwakeHold` neither acquires nor
  releases, **and the idle grace does not age** — otherwise a long Unknown
  interval would release the instant one Idle sample arrived.
- Buddy reactions fire only on `Busy -> Idle`.
- `agent_state()` deletes the local state file only on `Idle`, never `Unknown`.

### D5. The phone wire changes additively

`page.html:271` is `s.busy ? " busy" : s.alive ? " on" : ""`. Any nonempty
string is truthy, so replacing the boolean with `"idle"` would paint every
session busy on a phone page that outlived the binary that served it.

Additive only: keep `busy: bool` (= `activity == Busy`) and **add**
`activity: "idle" | "busy" | "unknown"`, lowercase, spelled out in the wire
contract. The new page prefers `activity` and falls back to `busy`. The boolean
is removed only after the page carries a version negotiation, which is not this
work.

### D6. Panels carry operation tokens, not just a target

A target switch cannot cancel an already-running git mutation; it can only
disown the result. `git_panel.rs:354` sets `busy = false` **before** checking
generation, so a stale operation can clear the new target's busy state.

- A `(target, generation)` token is checked before **every** state mutation,
  including the busy flag.
- Cancelling an *armed* destructive action (not yet started) is distinct from
  disowning a *running* mutation.
- The workspace validates the target before opening a `FileViewer`; an open
  viewer is closed on target change.
- Every click/refresh/submit handler re-checks `PanelTarget::Local` at
  invocation time, not only at render time.

### D7. Target persistence and profile loss

`Target::{Local, Remote(ProfileId)}` is serialized in `PaneNode`.

- Absent in a saved layout -> `Local`. Existing layouts restore unchanged.
- A saved `ProfileId` that no longer exists restores as a **dead remote pane**
  labelled with the missing profile, reconnect disabled. It must never silently
  become a local shell — that is how a remote pane would inherit local
  authority.
- Restore currently respawns every leaf locally (`workspace/mod.rs:2153`); this
  path must branch on target.
- A live pane **snapshots** its launch metadata at spawn. Editing or deleting a
  profile never mutates a running pane; the new profile is resolved only on an
  explicit reconnect.

### D8. An invalid profile must not erase settings

`settings.rs:201` is `serde_json::from_str(&text).unwrap_or_default()` — any
serde error resets **all** settings to default. Adding a validated profile list
makes that far likelier to fire.

Profiles deserialize permissively into a raw form; validation happens per
profile afterward. An invalid profile is skipped and surfaced in the UI. It can
never invalidate the rest of `Settings`. The permissive boundary covers a
malformed **container** too — `"remoteProfiles": 5` must degrade to "no
profiles", not reset every unrelated setting. (Hardening the whole-settings fallback
is a real pre-existing issue but is out of scope here and noted, not fixed.)

### D9. Destination modelling

`RemoteProfile` carries separately validated `user: Option<String>` and
`port: Option<u16>`, rendered as `-l` and `-p` **before** the destination —
not packed into a `user@host:port` string. This matters immediately: the Windows
account name will differ from the Mac one.

Destination accepts a dot-separated FQDN (per-label
`^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$`, 253 bytes total), a literal
IPv4, or a **bare** IPv6 literal. Validated at deserialization **and**
immediately before spawn.

IPv6 must NOT be bracketed in this argv form: with the local OpenSSH 10.2p1,
`ssh -- '[::1]'` tries to resolve the literal hostname `[::1]` and fails, while
bare `::1` is accepted. Brackets are accepted in settings for user convenience
and stripped when building argv.

### D10. Remote folder-picker behaviour

Refusing the write is correct but v2 would then fall through and silently open a
**new local window** at the picked path. Instead, on a remote pane the directory
control is disabled and relabelled to say the pane is remote. A remote path
picker is a later feature on top of the remote filesystem abstraction.

### D11. The full cwd-consumer list

`cwd()`-derived consumers must handle a remote pane, whose cwd is `None`:

- buddy repository probing (`workspace/mod.rs:797`)
- focused-bar cwd display and directory control (`workspace/mod.rs:2551`)
- split cwd inheritance (`workspace/mod.rs:1161`)
- git/files panel targeting (`workspace/mod.rs:2087`)
- sidebar cwd and status dot (`workspace/mod.rs:748`)

### D12. Windows launch is wholly deferred to slice 5

Slice 2 is **POSIX only** (zsh, bash, fish). A profile with
`os: Windows` cannot be created until slice 5 — discovery may surface a Windows
candidate, but promoting it is refused with an explicit "not yet supported"
message. `HostOs`/`ShellKind` exist from slice 1 so the dispatch point is
present rather than retrofitted, but no half-working Windows launch ships.

### D13. Remote panes carry no local agent instrumentation

A remote session must not create a local `ST_PANE_STATE` slot or put the local
adapter shims on `PATH`. That instrumentation would be attached to the local
`ssh` process and can only be meaningless. `Target::Remote` skips the whole
adapter block in `TermSession::spawn` (`term_session.rs:521`).

### D14. Focus and panel target change atomically

`(target, generation)` tokens fix stale *async completions* but not a stale
*current panel target*. There are **fourteen** separate
`self.focused_terminal = ...` assignment sites in `workspace/mod.rs` — 1097,
1131, 1147, 1172, 1221, **1236**, **1259**, **1304**, 1361, 1946, 2178, 2270,
2278, 2301 — while panel retargeting happens on a periodic
tick gated on `self.sidebar_open && self.pet_tick_count.is_multiple_of(3)`
(`workspace/mod.rs:741`) — every third pet tick, and only while the sidebar is
open.

So focus can sit on a remote pane while the git panel still holds a live local
repository target and remains fully actionable, including its destructive
actions. Handler-side `PanelTarget::Local` checks cannot save this, because the
panel has not yet been told focus moved.

Every logical focus mutation routes through ONE helper that, in a single update
and before any other UI action can dispatch:

- assigns focus;
- sets both panel targets and invalidates their generations;
- closes or disowns an open `FileViewer`;
- updates the focused-bar directory control's enabled state.

All fourteen sites are converted; the periodic tick keeps refreshing *content*
but is no longer responsible for *identity*. A test asserts that immediately
after each of the fourteen focus paths, both panels report the new pane's target
with no intervening tick — in particular that focusing a remote pane leaves no
actionable local repo.

The three close paths (1236 full-pane window close, 1259 final window/tab close
via `close_terminal`, 1304 active tab close via `close_tab`) are the most
important of the set and are easy to miss: closing a focused LOCAL
pane can land focus on a remote or dead-remote pane, which is exactly the
transition where the panels would otherwise retain local authority. They assign
focus across a line break, which is why a naive grep undercounts them. The async folder picker re-checks the target when it returns, since focus
can move while the native dialog is open.

`remap_ids` (`workspace/mod.rs:3751`) reconstructs `PaneNode` manually and must
carry `target` through; the compiler will surface most such sites.

### D15. Profile identity is globally unambiguous

Per-profile validation is not enough: two valid profiles sharing a `ProfileId`
make restore and reconnect ambiguous, and could resolve to the wrong host.

- `ProfileId` is opaque, app-generated, immutable, and nonempty. Never derived
  from the label or destination.
- Duplicate IDs quarantine **every** colliding profile. Never first-wins or
  last-wins — a silent wrong-host selection is the failure being prevented.
- The collision is surfaced in settings.
- While identity is ambiguous, reconnect stays disabled and affected panes
  restore as dead remote panes.
- Renaming a profile keeps its ID. Reusing a *label* generates a new ID.
  Deliberately recreating a deleted profile may reuse the old ID, which is what
  makes a dead pane recoverable (see Q2).

A dead remote pane uses a distinct construction path and must never call
`TerminalPane::new`, which unconditionally attempts a local shell.


## Slices

1. **Activity + target foundations.** Tri-state with distinct foreground and
   companion APIs, target ownership and persistence, panel targets with
   operation tokens, hard local-only folder-write guard, additive phone field.
   No remote pane can be created. Ships alone.
2. **Plain remote terminals, POSIX only.** Safe argv, auth/error surfacing,
   exited-pane retention, resize/signal/exit E2E. No telemetry.
3. **POSIX shell integration.** Nonce title protocol, versioned atomic
   installer, zsh/bash emitters, tmux and overwrite degradation.
4. **Remote operation abstraction.** Structured remote command + filesystem
   reads, ControlMaster, remote lifetime policy, then git/files/viewer.
5. **Windows integration.** PowerShell launcher and emitter.
6. **Scheduler.** Capability/load probes (`nvidia-smi`, load average, free
   memory) — explicitly NOT terminal prompt activity, which sees only one
   foreground command in one shell and is blind to detached jobs, other tabs,
   other users, and GPU state.

## Testing

**Acceptance criterion, stated narrowly.** "Slice 1 is a behavioural no-op for
local panes" is too broad and is retired. Atomic panel/viewer retargeting is an
INTENTIONAL behaviour change for local panes too: it changes the timing of
local focus transitions, and closing or disowning a viewer on target change can
alter local-to-local focus behaviour. That change is the point of D14, not a
regression.

What must remain equivalent is narrower and testable:

1. local **activity semantics** — cue, `finished`, caffeinate, buddy, sidebar;
2. **legacy wire fields** — byte-identical, identical meaning, plus exactly one
   additive field;
3. **serialized local layouts** — unchanged, via omitting `target: Local`.



`CueGate::tick` migrates to `Activity` in production and tests together; no bool
wrapper is retained purely to claim untouched test files (Rust cannot overload
`tick`, and the real signature also takes time and bell). Equivalence is proven
instead by a small legacy reference model: for any sequence of local-only
Idle/Busy samples, the new implementation must produce identical cue,
`finished`, caffeinate, buddy, and sidebar results to the reference.

"Byte-identical wire" is also wrong, since adding `activity` necessarily changes
the bytes. The criterion is: every legacy field is byte-identical and carries
identical semantics, plus exactly one additive field. Similarly, `target: Local`
is omitted from serialized layouts so existing saved JSON is unchanged.

`finished` remains `MIN_BUSY`-qualified — it follows `long_job_finished`
(`workspace/mod.rs:475`), so a quick `Busy -> Idle` run must still not count.

The sidebar today already distinguishes Idle, Busy-with-output, and Busy-but-
quiet. Adding `Unknown` makes a fourth state; the three existing local states
must not be collapsed, and the new state is drawn with the `icons.rs` set, never
an emoji.

Unit: title grammar (well-formed, wrong nonce, missing sentinel, oversized,
control bytes, sentinel inside the display half, absent/garbage state);
destination validation (spaces, `;`, `$(`, backticks, newlines, leading `-`,
over-length label, over-length FQDN, empty, valid MagicDNS FQDN, IPv6, user,
port); exact-argv assertions including `--` placement and `-l`/`-p` ordering;
nonce template rendering per shell plus a token-count property test; `CueGate`
tri-state including `Busy -> Unknown -> Busy -> Idle`; caffeinate aggregation
including the non-ageing idle grace; panel operation tokens rejecting a stale
completion's busy write; folder picker refusing a remote pane while Idle;
restore with an absent target defaulting to Local; restore with a missing
profile producing a dead remote pane; an invalid profile leaving other settings
intact.

E2E: no second machine exists yet. `ssh localhost` with macOS Remote Login is a
real ssh path with a real remote shell and a real title round-trip; slices 2 and
3 are testable against it today. Remote Login is currently OFF on this Mac, so
slice 2 begins by enabling it.

## Questions resolved during review

- **The 14-site focus inventory is complete.** No indirect mutation via `take`,
  `replace`, or `mem::replace` exists, and the constructor's
  `focused_terminal: None` is initialisation, not a transition. The phone's
  spawn path reaches 1147 then 1946 via `focus_terminal_by_id`, and its close
  path reaches 1221/1236/1259 via `close_terminal` — there is no companion
  focus endpoint that bypasses the helper.
- **Quarantining every colliding profile is the right failure mode.** It keeps
  unrelated valid profiles usable while guaranteeing an ambiguous ID can never
  select a host. Refusing the whole list is safe but needlessly destructive.
- **A deleted profile may be deliberately recreated with its old ID** to revive
  a dead pane. Safe because IDs are opaque and generated, duplicates are
  quarantined, live panes hold launch snapshots, and reappearance only *enables*
  an explicit reconnect — it never reconnects on its own.
- **`ProfileId` generation reuses the existing dependency-free pattern** —
  128 bits from `/dev/urandom`, as in `companion/auth.rs:6`.
- **No untested local-regression class remains** beyond ordinary implementation
  detail: the reference-model activity tests, legacy-field assertions,
  unchanged-local-layout assertion, fourteen focus-path tests, operation-token
  tests, and the folder-picker recheck cover the boundaries.
