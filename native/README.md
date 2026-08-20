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
bundler dependency. Sessions are shared with the Tauri app; settings live in
`~/Library/Application Support/com.tomaspinal.superterminal.native/`.

## Keys

Cmd+T new tab, Cmd+W close pane, Cmd+1..9 switch tab, Cmd+D / Cmd+Shift+D
split right/down, Cmd+O sessions, Cmd+S save session, Cmd+, theme picker.
Terminal line editing: the same Cmd/Alt chords as the Tauri app; plain click
or Option+Click moves the cursor on the prompt row.
