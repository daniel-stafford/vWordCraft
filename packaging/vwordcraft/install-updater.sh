#!/usr/bin/env bash
# Install update.sh and a LaunchAgent that runs it at login and every 3 hours.
# Log: ~/Library/Logs/vWordCraft-update.log. Remove with: install-updater.sh --uninstall
set -euo pipefail
LABEL="com.github.daniel-stafford.vwordcraft-update"
DIR="$HOME/Library/Application Support/vWordCraft"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$PLIST" "$DIR/update.sh"
  echo "removed $LABEL"
  exit 0
fi

mkdir -p "$DIR" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cp "$(dirname "${BASH_SOURCE[0]}")/update.sh" "$DIR/update.sh"
chmod +x "$DIR/update.sh"
cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$DIR/update.sh</string></array>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>10800</integer>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/vWordCraft-update.log</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/vWordCraft-update.log</string>
</dict>
</plist>
EOF
plutil -lint "$PLIST"
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "installed $LABEL"
