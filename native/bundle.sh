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

codesign --force --deep --sign - "$APP"

SIZE=$(du -sh "$APP" | cut -f1)
echo "Bundled: $APP ($SIZE)"
