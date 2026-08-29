# Remote terminals (slice 2) — design

Date: 2026-08-29
Status: PROPOSAL, revised once after design review.
Parent: `docs/superpowers/specs/2026-08-28-remote-hosts-design.md` (slice 1 shipped as `a9f654e`)

## Goal

Open a real terminal on another machine, from SuperTerminal, over ssh. POSIX
hosts only. No telemetry — the busy dot stays `Unknown` for remote panes, which
slice 1 already made safe. Telemetry is slice 3.

Concretely: Tomas adds his Windows PC and (maybe) a Mac Mini to the tailnet, and
wants a tab that is a shell on that box, so heavy work can run there instead of
on the laptop.

## What slice 1 already built (do not rebuild)

- `hosts::RemoteProfile { id, label, destination, user, port, os, shell }`,
  `ProfileId`, `HostOs`, `ShellKind`, `validate_destination`, `load_profiles`
  (permissive, quarantines duplicate ids), `Settings::profiles()`.
- `hosts::Target { Local, Remote(ProfileId) }` on the pane AND persisted in
  `PaneNode::Terminal` (omitted when Local, so old layouts are byte-identical).
- `resolve_target` -> `ResolvedTarget::{Local, Remote, MissingProfile}`, and a
  dead-pane construction path (`TerminalPane::dead`, `Workspace::spawn_dead_pane`)
  that never calls `TermSession::spawn`.
- `Activity::{Unknown, Idle, Busy}` end to end; `hosts::pane_activity` already
  maps a remote pane with no session to `Unknown`.
- Panels detach on a remote target; the folder picker refuses a non-local pane;
  buddy probing and the focused-bar directory control skip remote panes.

## D0. A live remote pane is still untrusted (blocking finding, verified)

Slice 1's `hosts::pane_activity` is `session_activity.unwrap_or(...)`: it forces
`Unknown` only when there is NO session. The moment this slice spawns `ssh` as a
real `TermSession`, a LIVE remote pane reports the local ssh process's activity
and cwd — `tcgetpgrp` on the local PTY, and `pid_cwd` of the local ssh process.
Both are meaningless, and both would be consumed as if they meant something.

This is the same defect that was fixed for dead panes during slice 1's push gate,
one level up; it simply could not manifest until something spawned a remote pane.

Required, and it is a class fix, not a patch of the one accessor:
- `Target::Remote` reports `Activity::Unknown` from `foreground_activity`,
  `status_activity`, and `companion_activity` REGARDLESS of whether a session
  exists, until slice 3 supplies real telemetry.
- `cwd()` returns `None` for a remote pane. The local ssh process's cwd is the
  Mac's, and letting it through re-arms every consumer slice 1 disarmed.
- Every accessor on `TerminalPane` and `TermSession` reachable from a remote pane
  gets audited, not only the ones panels happen to call today. The review
  enumerated these as carrying local meaning; each must be made target-aware or
  explicitly classified local-only (and made private, or renamed to say so):

  `TerminalPane`: `cwd` (must be `None` for remote), `foreground_busy`
  (local `tcgetpgrp` truth only), `companion_busy` (combines local pgid, local
  adapter state, and local output timing — must not back remote
  `companion_activity`), plus `foreground_activity`, `companion_activity`, and
  `status_activity` forcing `Unknown` for `Target::Remote` even with a live
  session.

  `TermSession`: `foreground_busy`, `foreground_activity`, `agent_state`,
  `foreground_pgid`, `status`, `status_activity`, `cwd` — all local process/PTY
  introspection, all local-only unless wrapped by a target-aware pane API.

  Reviewed as target-neutral (they operate on transport or screen state, not on
  interpreting a local process): `title`, `target`, `send_text`/`write`,
  `input_sender`, `take_bell`, `visible_text`, search, resize, snapshot, dirty,
  exit, shutdown.

  One naming caveat worth fixing while in here: `has_live_shell` actually means
  "the local PTY child has not exited". For a remote pane that child is `ssh`,
  which is not evidence that a remote shell is alive.

## D1. The launch suppresses inherited ssh authority (blocking finding)

The argv shape below parses correctly against the local OpenSSH 10.2p1, but it
inherits the user's ssh config. A `Host` entry can enable agent forwarding, X11
forwarding, port forwards, a `RemoteCommand`, or `PermitLocalCommand` — and
**agent forwarding hands the remote machine the use of local SSH keys**. That is
a local-authority leak of exactly the class this work exists to prevent,
arriving through a channel the parent design never considered.

The default remote terminal therefore suppresses them explicitly:

**This is the one canonical argv. There is no other form in this document.**

    ssh -tt -a -x \
        -o ForwardAgent=no \
        -o ClearAllForwardings=yes \
        -o PermitLocalCommand=no \
        -o RemoteCommand=none \
        [-l <user>] [-p <port>] -- <destination>

`RemoteCommand=none` is decided, not deferred: without it a configured
`RemoteCommand` in the user's ssh config makes "open a shell" silently run
something else entirely.

All five options were verified accepted by the installed OpenSSH_10.2p1 via
`ssh -G` before this was written; `ssh -G` reports `forwardagent no` and
`permitlocalcommand no` as expected. An option the local ssh does not recognise
makes it exit non-zero, so the feature would fail closed rather than silently run
unsuppressed — but verify again if the toolchain moves.

These are defaults, not a policy engine. If a host genuinely needs forwarding
later, that is a per-profile opt-in with its own design, not a silently
inherited default.

## The launch, and why it is simpler than the parent spec assumed

The parent spec described a remote command carrying an `ST_ST=1` marker so a
shell hook could emit telemetry. **Slice 2 has no telemetry**, so it needs no
remote command at all:

(For the exact argv including the authority-suppression options, see D1 above —
that is the single canonical form. The points below are about the SHAPE of the
launch, not a competing definition of it.)

- No remote command of our own means no per-`ShellKind` template, no quoting question, and
  nothing to get wrong across zsh/bash/fish. `ShellKind` stays unused until
  slice 3's emitter needs it.
- Every option precedes the destination. `ssh(1)`'s synopsis is
  `... destination [command [argument ...]]` and arguments after the destination
  are joined into the remote command, so `--` must come BEFORE the destination.
- `-tt` forces remote PTY allocation; we have a local PTY and ssh must request a
  remote one.
- No `BatchMode`: interactive auth (passphrase, 2FA, host-key prompt) must still
  work against a plain sshd. Tailscale SSH needs no prompt.
- The destination is its own argv element and is re-validated immediately before
  spawn, not only at settings load.

`TermSession::spawn` gains a `Target`. For `Target::Remote`, `shell` becomes ssh
with those args. Per slice 1's D13, a remote session sets NO local
`ST_PANE_STATE`/`ST_PANE_ID` and does not put the adapter shims on `PATH` —
that instrumentation would attach to the local `ssh` process and can only lie.

## Creating a profile

Discovery produces CANDIDATES; only an explicit user action creates a profile.
Rationale from the parent spec: the tailnet contains an Android phone, which is
not a shell target.

- A "scan tailnet" action shells `tailscale status --json` and parses it with the
  serde_json already in the tree, subject to the same treatment as
  `companion/blender.rs`: a total deadline and an output cap, because the command
  can hang or return unexpectedly large output. `tailscale` absent from PATH
  means no candidates and the feature is simply absent.
- Candidates whose `os` is `windows` are shown but NOT promotable, with an
  explicit "not yet supported" message. Windows launch is slice 5. Android/iOS
  candidates are not offered at all.
- Promotion assigns a fresh `ProfileId` (`/dev/urandom` hex, the `auth.rs`
  pattern) and validates the destination.

## Opening a remote terminal

A new-terminal affordance gains a host choice: local (default, unchanged) or one
of the configured profiles. The default path must remain exactly today's
keystroke and click behaviour — a local terminal must not get slower or gain a
step.

The tab strip shows a host badge on remote panes, drawn from `icons.rs`. No
emoji.

## Failure is visible, not silent

`workspace/mod.rs` currently routes `PaneEvent::Exited` straight to
`close_terminal`. For a REMOTE pane that would make an ssh auth failure, an
unreachable host, or a dropped connection vanish before it can be read.

Remote panes therefore retain the dead pane with its final output and an explicit
reconnect action. Local panes keep today's close-on-exit behaviour unchanged.

Reconnect re-resolves the profile and never reconnects on its own.

**A real launch snapshot is required, and does not exist yet.**
`Target::Remote(ProfileId)` is persisted identity, not a snapshot — and shipped
code resolves the label from CURRENT settings in a render path
(`workspace/mod.rs:2177`). So editing a profile today would change a live pane's
displayed host. Slice 2 adds a `RemoteLaunch` snapshot held by the live pane:
resolved label, destination, user, port, os, shell, and the exact argv used.
Profile edits do not touch it; only an explicit reconnect rebuilds it.

## Signals, resize, exit

ssh puts its local tty in raw mode and owns the remote tty, so Ctrl-C and
SIGWINCH should propagate without special handling — but "should" is not
evidence, and these get real end-to-end tests.

`TERM=xterm-256color` is already set at spawn and is forwarded by ssh.

ssh's exit status covers both the remote shell's status AND transport or
authentication failure — and it cannot separate them. 255 usually means
transport or auth, but a remote shell can exit 255 too, and a nonzero exit can be
perfectly intentional. Without a remote-side marker (slice 3) there is no honest
classification available.

So the design does NOT promise one. Every remote exit retains the pane and shows
a generic exited/disconnected state carrying the numeric status and the final
output, plus a reconnect action. That is truthful and useful; a confident
"could not connect" that is sometimes wrong is worse than a status code the user
can read.

This needs plumbing: the exit status is currently discarded between
`term_session.rs:382` and `workspace/mod.rs:1156`, so it must be carried through
to be displayed.

## Parked items from slice 1 that land here

- **Uppercase destinations are normalised** at validation — by lowercasing.
  Hostnames are case-insensitive; without this, two differently-cased entries for
  one host coexist and defeat duplicate detection. Note `validate_destination`'s
  doc comment at `hosts.rs:233` ALREADY says "validate and normalise" while doing
  no normalisation. That overclaiming doc comment is the second instance of the
  same pattern in this codebase (the first, on `AwakeHold::tick`, hid a real bug
  for a full review round) — when fixing this, make the comment describe what the
  code does.
- **`cwd.expect(...)` at `workspace/mod.rs:841` and `:2726`** become `if let`
  bindings. They are correct today but are panics awaiting a reorder, and a panic
  there takes the whole app down rather than one feature.
- **`profile_label` caches its resolved profile list.** It calls
  `settings.profiles()` from a render path — a full clone per profile plus an
  O(n^2) duplicate scan, per frame. Free today only because the list is empty.
- **The remote empty-state copy** loses "in this release" (this app ships
  direct-to-`/Applications`, it has no release cadence) in favour of
  "not supported over remote hosts".

## Testing

E2E is possible today with no second machine: `ssh localhost` with macOS Remote
Login enabled is a real ssh path, a real remote shell, a real PTY, and a real
exit status. Remote Login is currently OFF on this Mac and must be enabled
before these tests can run.

Unit (pure, in `hosts.rs` — this codebase has no gpui test harness and must not
grow one): exact argv construction including `--` placement and `-l`/`-p`
ordering; destination re-validation at spawn; uppercase normalisation; the
Windows-not-promotable rule; `tailscale status --json` parsing including a
truncated and an oversized document.

E2E against `ssh localhost`: a remote pane opens and echoes; Ctrl-C interrupts a
remote `sleep`; a window resize changes the remote `tput cols`; a remote `exit`
retains the pane with its output rather than closing it; a bad destination
surfaces an auth/transport failure that stays readable.

## Explicitly NOT in this slice

Telemetry and the busy dot for remote panes (slice 3). Remote filesystem, git
panel, or file viewer (slice 4) — panels stay detached. Windows (slice 5). The
scheduler (slice 6). ControlMaster/ControlPersist multiplexing, which slice 4
needs for panel polling and should own.

## Questions resolved in review

- **A bare login shell is the right slice-2 shape** — it avoids premature shell
  quoting. But build the argv as a small dedicated builder now, so slice 3 can
  replace "no remote command" with a command template cleanly rather than
  retrofitting one into an inline vector.
- **Clean remote exits are retained too**, not just failures. It avoids hiding
  output, avoids the misclassification above, and gives one consistent affordance.
  The user closes the pane themselves.
- **The default new-terminal path stays local-only** — same click, same
  keystroke, same cost. Remote creation lives in an adjacent host menu or on the
  hosts surface with an explicit "open" action. No chooser in the default path.

## Open questions for the reviewer

Q1. Is the D0 class fix complete as scoped — are there accessors on
    `TerminalPane` or `TermSession` beyond activity and cwd that leak local
    meaning for a live remote pane?
Q2 (resolved): suppressing forwarding by default is correct. It does break real
    workflows — remote git using local agent keys, X11 apps, port forwards,
    config-driven `RemoteCommand` — but every one of those is authority-expanding
    and belongs in an explicit per-profile opt-in later, never inherited silently.
Q3 (resolved): `RemoteLaunch` does NOT need to persist across an app restart. A
    restored pane is dead until explicit reconnect, so rebuilding it from the
    current profile at reconnect is correct. `Target::Remote(ProfileId)` stays the
    durable identity; `RemoteLaunch` describes a live process.
