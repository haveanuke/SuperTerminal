# Accepting a pairing

**Goal:** two Macs can actually authenticate to each other.

## The gap

`pair_peer(host)` mints a FRESH random secret and stores it locally. There is
no import path anywhere — nothing creates a peer record carrying a secret that
came from somewhere else.

So pairing on both machines gives each its own secret, and neither recognises
the other's. Verified against the user's two Macs: this Mac's stored secret for
MAC-138 is answered with 404 by MAC-138, while the tailnet path itself is
healthy (`tailscale ping` pongs, port 43110 accepts connections).

The peers feature therefore cannot work between two Macs as built. The existing
panel even says "Paired {label} — scan on that Mac to finish", promising a step
that was never implemented.

## What makes this small

`companion::auth::principal_for` matches on the SECRET ONLY —
`peer_secret_matches(presented, peers)`. Ids, labels and grants are local and
need not agree between machines.

**So one shared secret authenticates BOTH directions.** Mac A holds
`{host: B, secret: S}`; Mac B holds `{host: A, secret: S}`. A→B presents S and
B matches it; B→A presents S and A matches it. A single paste on the second
machine completes the pair.

## The flow

1. On Mac A, pair as today. It mints S and shows it — the panel already
   renders a QR of `{companion_url}#S`.
2. On Mac B, **paste that pairing link (or the bare code)**. Mac B creates a
   record for Mac A carrying S rather than minting one.

Both forms are accepted because both occur in practice:

- **A full pairing URL** — carries the host AND the secret, so nothing else
  needs choosing.
- **A bare 32-hex code** — for when it arrived over a message rather than a
  scan; the user picks which discovered host it belongs to.

The QR keeps its existing job for the phone. This is the Mac-to-Mac path, and a
Mac has no natural in-app camera.

## Rules

- **A pasted secret is validated before it is stored** with the same
  `secret_ok` the loader uses: 32 lowercase hex characters. A malformed paste
  is refused with a reason, never stored to fail mysteriously later.
- **Refuse a secret already held by another peer record.** `load_peers`
  already quarantines duplicate secrets, and creating one by hand would mean
  two peers authenticating as each other.
- **Pairing by acceptance uses the same mutation path as pairing by minting**
  (`apply_peer_mutation`), so a live companion restarts and honours the new
  peer immediately rather than at the next manual toggle.
- **Grants follow the existing default**, `Grants::on_pair` — view and type on,
  spawn off. An accepted pairing is no more or less trusted than a minted one.

## What this does not change

The phone flow, the QR, `Grants::default` staying deny-all for malformed
records on disk, and the fact that broadcasting stays opt-in per terminal. A
pairing lets two machines talk; it does not share anything by itself.
