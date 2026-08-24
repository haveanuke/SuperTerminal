#!/bin/sh
# Build SuperTerminal Native.app from the release binary (contract rev 2 §12:
# hand-rolled bundle, no bundler dependency; ad-hoc codesign).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="SuperTerminal Native"
BUNDLE_DIR="$ROOT/target/release/bundle"
APP="$BUNDLE_DIR/$APP_NAME.app"

cargo build --release -p superterminal-native --manifest-path "$ROOT/Cargo.toml"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$ROOT/target/release/superterminal-native" "$APP/Contents/MacOS/superterminal-native"
cp "$ROOT/src-tauri/icons/icon.icns" "$APP/Contents/Resources/icon.icns"
# Tool-adapter shims (claude/codex bell integration) — must stay executable.
mkdir -p "$APP/Contents/Resources/adapters"
cp "$ROOT/native/adapters/"* "$APP/Contents/Resources/adapters/"
chmod +x "$APP/Contents/Resources/adapters/"*

cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>SuperTerminal Native</string>
    <key>CFBundleDisplayName</key>
    <string>SuperTerminal Native</string>
    <key>CFBundleIdentifier</key>
    <string>com.tomaspinal.superterminal.native</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleExecutable</key>
    <string>superterminal-native</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

# A stable identity keeps macOS permission grants (TCC) across rebuilds —
# ad-hoc signatures make every build look like a different app, so the
# system re-prompts for folder access after each install.
if security find-identity -v -p codesigning 2>/dev/null | grep -q "SuperTerminal Dev"; then
    SIGN_ID="SuperTerminal Dev"
else
    SIGN_ID="-"
    echo "WARNING: no 'SuperTerminal Dev' identity — signing AD-HOC." >&2
    echo "         macOS will reset this app's permission grants on every" >&2
    echo "         rebuild. Run native/dev-cert.sh once to fix (one-time," >&2
    echo "         per machine)." >&2
fi
codesign --force --deep --sign "$SIGN_ID" "$APP"
echo "Signed with: $SIGN_ID"

SIZE=$(du -sh "$APP" | cut -f1)
echo "Bundled: $APP ($SIZE)"
