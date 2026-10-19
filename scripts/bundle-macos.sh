#!/usr/bin/env bash
# Wrap a release binary in a minimal macOS .app bundle (retina-capable, dark-mode aware), written
# next to the binary: target/release/terminal-rs unless another path is given.
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bin=${1:-target/release/terminal-rs}
app=$(dirname "$bin")/terminal-rs.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/terminal-rs"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>terminal-rs</string>
  <key>CFBundleDisplayName</key><string>terminal-rs</string>
  <key>CFBundleIdentifier</key><string>dev.terminal-rs.app</string>
  <key>CFBundleExecutable</key><string>terminal-rs</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.finance</string>
</dict>
</plist>
PLIST
echo "bundled $app"
