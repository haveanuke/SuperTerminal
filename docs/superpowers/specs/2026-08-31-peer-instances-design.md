# Peer instances: SuperTerminal to SuperTerminal — design

Date: 2026-08-31
Status: PROPOSAL, revised once after design review.
Supersedes: `2026-08-29-remote-terminals-slice-2-design.md` (ssh remote terminals)
Builds on: `2026-08-28-remote-hosts-design.md` slice 1, shipped as `a9f654e`

## Why this replaces the ssh design

Tomas proposed it and he is right. Open SuperTerminal on each machine, mark
individual terminals as broadcast, and other paired instances can see and use
them.

Three reasons it beats an ssh transport for the actual use case (a personal
MacBook and a work MacBook, switching between them at will):

1. **A working transport already exists to build on** — though NOT, as an
   earlier draft of this document claimed, one that can be reused verbatim.
   `companion/server.rs` serves `/sessions`, `/stream` (SSE grid snapshots),
   `/input`, `/spawn`, `/close`, `/rename`, `/version`, `/previews`, and the
   phone is genuinely a remote terminal client over the tailnet. But it is a
   PHONE-VIEW protocol: deliberately lossy, phone-grade input, no geometry
   ownership model. What carries over is the hub, the serializer, the SSE
   plumbing, and the tailnet-only + constant-time-token posture. What must be
   added is set out in D1-D4 below.
2. **It deletes two slices.** Over ssh a remote terminal is a dumb pipe: busy
   state needs a shell hook shipped to every host (old slice 3) and the panels
   need a remote filesystem abstraction (old slice 4). If the terminal lives on
   its own machine, both are already native there — `SessionInfo` ALREADY
   carries `alive`, `busy`, `activity`, `finished`.
3. **Sessions outlive the viewer.** Close the lid on the personal Mac and the
   work Mac's terminal keeps running, because it never left that machine.

ssh terminals still earn a place later, for reaching a machine that is NOT
running SuperTerminal — a server, a VPS, the Windows PC. Complementary, not
competing, and no longer first.

## What slice 1 already gives us, unchanged

Slice 1's safety work is transport-agnostic and none of it is wasted:

- `Activity::{Unknown, Idle, Busy}` — and `Unknown` matters MORE here, because a
  peer's report can go stale when the network hiccups. That is precisely the
  state it exists for.
- `Target::{Local, Remote(..)}` on the pane and persisted in the layout, with a
  missing target restoring as a dead pane rather than silently becoming local.
- Panels detach on a remote target; the folder picker refuses a non-local pane;
  buddy probing and the focused-bar directory control skip remote panes.
- Atomic focus/panel retargeting across all 14 sites, guarded by a test.
- Permissive profile loading that quarantines duplicate identities.

What changes is what `Remote(..)` POINTS AT: a paired peer instance rather than
an ssh destination.

## D1. Input is raw bytes, not the symbolic key grammar

`/input` today accepts `{text}` or a small symbolic set — enter, ctrl-c, ctrl-u,
tab, esc, arrows, y/n (`companion/input.rs`). That is right for a phone keypad
and useless for a desktop pane: no modified arrows, function keys, home/end/page,
alt/meta, bracketed paste, or mouse tracking. A TUI would feel broken.

The fix is not to grow the grammar. `keys.rs:36` already has

    pub fn key_to_bytes(input: &KeyInput, app_cursor: bool, option_as_meta: bool) -> Option<Vec<u8>>

which is exactly what a LOCAL pane writes to its PTY. An attached pane runs the
same encoder against its own key events and ships the resulting bytes. The
encoding stays in one place, and the peer endpoint becomes a byte sink rather
than a vocabulary that must be kept in sync.

`app_cursor` already crosses the wire in the snapshot, so the attached pane can
encode correctly. `option_as_meta` is the VIEWER's preference and stays local.

The phone's symbolic endpoint is untouched.

## D2. The broadcaster owns the geometry

Two desktop clients of different sizes cannot both resize one PTY without reflow
fights. For this cut: **the broadcasting instance owns the grid**. Attached panes
fit and scroll to what they are given; they do not resize the remote PTY.

An attached pane that wants to drive geometry is a resize lease or an exclusive
attach mode — a separate design, not this one. Say so in the UI rather than
letting a user discover their resize does nothing.

## D3. Auth is route-scoped principals, not just more tokens

Today one token guards every protected route. Adding per-peer secrets naively
would let the phone token reach peer-only surfaces, or let a paired peer call
`/spawn`, `/close`, `/rename`, `/previews`.

Authentication therefore resolves to a principal:

    enum Principal { Phone, Peer(PeerId) }

and every route states which principals it admits. `Peer` gets the broadcast
session list, the peer stream, and the peer input sink — nothing else, unless
this document is amended to say otherwise. The phone's existing behaviour must be
byte-identical after the change; it is the surface Tomas uses most and it must
not regress.

## D4. Attached panes are never re-broadcast

If both instances broadcast and both attach to each other, an unguarded
implementation produces remote views of remote views, duplicated sessions, and
input routed through a chain. `workspace/companion_ui.rs:83` currently registers
every live local pane when the companion starts; the new code must NOT do that
for attached panes.

Rule: only a pane that owns a local PTY may be broadcast. An attached pane is
never publishable, and the broadcast list is filtered by origin, not merely by
what happens to be open.

## D5. The degraded contract, stated rather than discovered

An attached pane is not a native terminal and this document does not pretend
otherwise:

- Scrollback is `HISTORY_TAIL = 150` rows, not the full local buffer.
- Selection and search do not cross the wire (`wire.rs` omits them by design).
- Frames arrive on the SSE cadence with a 200ms floor and a 1MB serialized cap;
  a very large grid degrades rather than streaming perfectly.

These are acceptable for a first cut ONLY because they are named. Each is a
candidate for later work; none should surprise anyone.

## The model

- **Peer** — another SuperTerminal instance, paired once, named, individually
  revocable.
- **Broadcast** — a per-terminal, opt-in publication. A terminal is private
  until its owner marks it broadcast. Nothing is exposed by default.
- **Attach** — a local pane that is a VIEW of a peer's terminal: it renders
  snapshots from `/stream` and sends keystrokes to `/input`. It owns no PTY.

## Pairing, and why not tailnet-membership

The tailnet includes a WORK MacBook. That machine may carry MDM, corporate
monitoring, or IT admin access, so "on the tailnet" is not equivalent to "mine".
A single shared bearer token would let a machine Tomas's employer partly
controls open a live shell on his personal Mac, and revoking it would force
rotating the token his phone also uses.

Pairing is cheaper than it sounds — it needs no keypairs and no handshake
protocol:

- Each peer relationship gets its own 128-bit secret from `/dev/urandom`, the
  existing `companion/auth.rs` pattern, compared in constant time exactly as the
  phone token already is.
- **What this does not defend against, stated plainly:** a bearer secret is a
  capability. An administrator or MDM agent on the work Mac that can read its
  stored secret can replay it. Challenge-signed keypairs would prevent that;
  bearer secrets do not. Given Tomas is deliberately pairing a work machine he
  accepts some exposure to, this is judged acceptable for a first cut — but it is
  a known limit, not an oversight, and it is the thing to revisit first if the
  work machine's posture ever changes.
- Exchange reuses the QR flow already built for the phone.
- Each peer is stored with a name Tomas chose, and deleting it is revocation.
- Peer records load permissively and quarantine duplicate ids, following slice
  1's `load_profiles` pattern, so a hand-edited settings file cannot reset every
  setting (`settings.rs:201` is `unwrap_or_default()`).

A second consequence matters for Tomas's stated requirement to tell sources
apart: under a shared token a host label is self-asserted and a confused or
compromised peer can claim to be the other machine. A paired identity is one he
established.

## Discovery

Candidates only; promotion is always explicit (slice 1's rule, because the
tailnet also contains an Android phone that is not a terminal host).

`tailscale status --json`, parsed with the serde_json already in the tree, gives
peer addresses. Each candidate is probed on the companion port for `/version`.
Both the scan and the probe get the `companion/blender.rs` treatment: a total
deadline and an output cap, one attempt per interval, absent-on-failure rather
than a hot retry. `tailscale` missing from PATH means no candidates and the
feature is simply absent.

## The attached pane

`Target::Remote(PeerId)` reuses slice 1's enum. The pane:

- Renders snapshots from the peer's `/stream`, and sends input to `/input`.
  There is no local PTY, no shell process, and no adapter shims — which also
  means none of slice 1's local-authority accessors can lie, because there is no
  local process to misreport.
- Takes its activity from the peer's `SessionInfo.activity` — and reports
  `Unknown` whenever the stream is disconnected, stale beyond a threshold, or
  the peer's own report is `Unknown`. Staleness is not idleness.
- Reports `cwd() -> None` for now. The peer knows its cwd and could report it
  later, but that is panel work, not this design.
- Retains the pane when the peer goes away, showing why, rather than closing it
  out from under the user — the same rule the ssh design reached for a different
  reason.

## What happens to the other slices

- **Old slice 3 (telemetry hook)** — mostly deleted. The peer reports its own
  activity because it owns the PTY. No shell hook, no per-shell template, no
  title-channel encoding.
- **Old slice 4 (remote panels)** — DEFERRED, not deleted. An earlier draft
  overstated this. Local panels stay detached for a remote target (slice 1
  already enforces that), so nothing is broken — but if Tomas wants the git and
  files panels to follow an attached pane, the owning peer must expose them over
  the protocol. That is much smaller than arbitrary filesystem remoting, and it
  is still real panel work that has to happen somewhere.
- **Old slice 5 (Windows)** — only ever needed for ssh. Deferred indefinitely;
  revisit if the PC returns.
- **Old slice 6 (scheduler/bidding)** — parked at Tomas's direction. It existed
  to place work you did not want to think about; deliberately switching between
  two Macs is not that.

## Risks and hard parts

- **Attaching grants typing.** A broadcast terminal accepts input. That is the
  point, but it means pairing is the whole security boundary.
- **Direction matters with a work machine, and needs a control.** Broadcasting a
  work terminal to the personal Mac puts work output on a personal machine; the
  reverse puts personal output on a monitored one. Pairing answers "who may
  connect"; direction answers "what may leave THIS machine". Both are required —
  a per-peer setting for what this instance is willing to publish to that peer.
- **Version skew.** Two instances may run different builds. `/version` exists;
  the design must say what happens on mismatch rather than discovering it.
- **The server is single-token today.** `auth.rs` holds one token per instance
  and every handler checks it. Per-peer secrets change that shape, and the phone
  must keep working unchanged throughout.
- **Snapshot cost.** `MAX_SERIALIZED` is 1MB per snapshot and the hub memoizes
  per revision. A second full-fidelity client is a real load question, not an
  obviously free one.

## Questions resolved in review

- **Shared, with broadcaster-owned geometry** (D2). Both views live and typeable,
  matching the phone's existing behaviour and Tomas's "see it and start using
  it". Attached panes do not resize the remote PTY.
- **Per-direction control is required**, not optional, because of the work Mac.
- **Do not reuse `/stream` and `/input` verbatim.** Reuse the hub, the
  serializer, and the SSE plumbing; add peer endpoints with principal-scoped
  auth, raw-byte input, and broadcast filtering.
- **`/version` grows a protocol version and capability field.** Refuse on
  protocol mismatch; tolerate build differences only when the protocol version
  matches. Refusing must say why, in the pane, rather than failing silently.

## Open questions

Q1. Is broadcaster-owned geometry acceptable in practice for two MacBooks of
    different screen sizes, or does a same-size assumption make attached panes
    unpleasant enough to need the lease design sooner than "later"?
Q2. Should `Principal::Peer` be allowed `/spawn` — i.e. can Tomas open a NEW
    terminal on the work Mac from the personal one, or only attach to ones
    already broadcast there? Opening one is the more useful product and the
    larger authority grant.
Q3. Does the 150-row scrollback limit need lifting for peers specifically, given
    a desktop attached pane invites scrolling in a way a phone does not?
