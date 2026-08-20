# SuperTerminal Native

Fully native macOS build of SuperTerminal: gpui (Metal) + alacritty_terminal.
No webview anywhere in the process tree.

## Pins

- `gpui = 0.2.2` (crates.io; `runtime_shaders` feature so Command Line Tools
  suffice — no full Xcode required, validated by the spike)
- `alacritty_terminal = 0.26.0`
- Reference for gpui idioms: the zed repository at the commit publishing
  gpui 0.2.2; the spike (`git log --follow native/src/main.rs`) is the
  proven in-repo example.

## Develop

```sh
cargo run -p superterminal-native      # dev build, opens the window
cargo test -p superterminal-native     # unit + PTY round-trip tests
```

## Bundle

```sh
native/bundle.sh                       # -> target/release/bundle/SuperTerminal Native.app
```

The bundle is hand-rolled (Info.plist + binary + icns, ad-hoc signed): no
bundler dependency. The script resolves its own location, so it runs from any
working directory. Sessions are shared with the Tauri app; settings live in
`~/Library/Application Support/com.tomaspinal.superterminal.native/`.

## Install

```sh
native/bundle.sh
pkill -x superterminal-native          # the PROCESS name, not "SuperTerminal Native"
rm -rf "/Applications/SuperTerminal Native.app"
ditto "target/release/bundle/SuperTerminal Native.app" "/Applications/SuperTerminal Native.app"
open "/Applications/SuperTerminal Native.app"
```

Two pitfalls this sequence avoids:

- `pkill -x "SuperTerminal Native"` matches nothing (the binary is
  `superterminal-native`), which leaves a stale build running for `open`
  to refocus.
- If you run `open` from inside an agent CLI session (Claude Code etc.),
  prefix it with `env -u CLAUDECODE -u CLAUDE_CODE_CHILD_SESSION
  -u CLAUDE_CODE_ENTRYPOINT -u CLAUDE_CODE_SSE_PORT` so session markers
  don't leak into the app. The app also scrubs these at startup.

## Layout

The left activity rail tabs between sidebar views: **projects** (tabs live here —
new/close/rename, plus per-terminal quick status; there is no bottom tab
strip. A project can hold multiple WINDOWS — full-pane terminals without
splitting, one visible at a time: '+' on the project row or Cmd+N adds one,
and the sidebar's window rows switch between them), **git** (VS Code-style source control: stage/unstage/discard
with confirm, commit, sync, inline file diffs, commit history with painted
branch lines and per-commit file expansion), and **files** (lazy tree over
the focused terminal's directory with inline previews). The bottom bar keeps
the global controls: directory picker, buddy note, broadcast, git/search/
sessions/theme, and the focused terminal's title.

## Keys

Cmd+T new project, Cmd+N new window in the current project, Cmd+W close
pane, Cmd+Shift+W close project, Cmd+1..9 switch
tab, Cmd+D / Cmd+Shift+D split right/down, Cmd+O sessions, Cmd+S save
session, Cmd+, theme picker, Cmd+F search scrollback, Cmd+Shift+G git
sidebar. Terminal line editing: the same Cmd/Alt chords as the Tauri app;
plain click or Option+Click moves the cursor on the prompt row; Cmd+Click
opens URLs.
