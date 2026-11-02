#!/usr/bin/env bash
# Builds the menu bar app, links the CLI, and registers a LaunchAgent so the app starts at login.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
APP="$HOME/Applications/Agent Clip.app"
BIN_DIR="$HOME/.local/bin"
LABEL="com.agent-clip"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

# Migrate an install from when this was called agent-clipboard: stop and remove the old app, keep the clip history.
OLD_LABEL="com.agent-clipboard"
launchctl bootout "gui/$(id -u)/$OLD_LABEL" 2>/dev/null || true
rm -rf "$HOME/Library/LaunchAgents/$OLD_LABEL.plist" "$HOME/Applications/Agent Clipboard.app"
if [ -d "$HOME/.agent-clipboard" ] && [ ! -e "$HOME/.agent-clip" ]; then
  mv "$HOME/.agent-clipboard" "$HOME/.agent-clip"
fi

mkdir -p "$APP/Contents/MacOS" "$BIN_DIR" "$HOME/Library/LaunchAgents" "$HOME/.agent-clip"

command -v cargo >/dev/null || { echo "install.sh: Rust is required (brew install rust)" >&2; exit 1; }
cargo build --release --quiet --manifest-path "$ROOT/cli/Cargo.toml"

swiftc -O -o "$APP/Contents/MacOS/AgentClip" "$ROOT/app/AgentClip.swift"
strip -x "$APP/Contents/MacOS/AgentClip"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$LABEL</string>
  <key>CFBundleName</key><string>Agent Clip</string>
  <key>CFBundleExecutable</key><string>AgentClip</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>LSUIElement</key><true/>
  <key>NSAppleEventsUsageDescription</key>
  <string>Reads the front Terminal tab's directory to filter commands by project.</string>
</dict>
</plist>
EOF
codesign --force --sign - "$APP" >/dev/null 2>&1

# Copied, not linked, so `cargo clean` or deleting the checkout doesn't break the command.
rm -f "$BIN_DIR/agent-clip"
install -m 755 "$ROOT/cli/target/release/agent-clip" "$BIN_DIR/agent-clip"

cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$APP/Contents/MacOS/AgentClip</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
</dict>
</plist>
EOF

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
# bootout returns before the old instance is gone; retry until bootstrap is accepted.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null && break
  sleep 0.5
done
launchctl print "gui/$(id -u)/$LABEL" >/dev/null
echo "installed: $APP, $BIN_DIR/agent-clip"

# Herdr users get the in-terminal picker and panel as a plugin (linked to this checkout).
if command -v herdr >/dev/null; then
  herdr plugin link "$ROOT/herdr" >/dev/null && echo "linked herdr plugin: agent-clip"
fi
