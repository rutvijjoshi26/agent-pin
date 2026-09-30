#!/bin/sh
set -eu

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
configuration="${1:-release}"
app_dir="$repo_dir/dist/AgentPin.app"
contents_dir="$app_dir/Contents"
macos_dir="$contents_dir/MacOS"

cd "$repo_dir"
swift build -c "$configuration"

rm -rf "$app_dir"
mkdir -p "$macos_dir"
cp "$repo_dir/.build/$configuration/AgentPin" "$macos_dir/AgentPin"

cat > "$contents_dir/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>AgentPin</string>
  <key>CFBundleIdentifier</key>
  <string>local.agentpin.overlay</string>
  <key>CFBundleName</key>
  <string>Agent Pin</string>
  <key>CFBundleDisplayName</key>
  <string>Agent Pin</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo "$app_dir"
