#!/bin/bash
# Double-click to build PVEStats.app inside this project directory.
#
# The bundle is a thin wrapper around launch.command. It stays in the repo and
# writes nothing anywhere else on the system — drag it to the Dock from here if
# you want it there.

set -e
REPO="$(cd "$(dirname "$0")" && pwd)"
APP="$REPO/PVEStats.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>PVEStats</string>
  <key>CFBundleDisplayName</key><string>PVEStats</string>
  <key>CFBundleIdentifier</key><string>net.local.pvestats</string>
  <key>CFBundleVersion</key><string>1.0</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>PVEStats</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <!-- The dashboard window belongs to this process, so the app needs a normal
       Dock icon and menu bar. Do not set LSUIElement here — without a Dock
       presence there is no way to ⌘Q it. -->
</dict>
</plist>
PLIST

cat > "$APP/Contents/MacOS/PVEStats" <<LAUNCHER
#!/bin/bash
exec "$REPO/launch.command"
LAUNCHER

chmod +x "$APP/Contents/MacOS/PVEStats" "$REPO/launch.command"

echo "Built $APP"
echo
echo "Double-click it here in the project folder."
echo "Re-run this script if you move the repo."
read -r -p "Press return to close."
