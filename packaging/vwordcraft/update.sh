#!/usr/bin/env bash
# Install the newest vWordCraft build from GitHub into /Applications, if it differs from the
# installed one. Run by a LaunchAgent every few hours (see install-updater.sh); safe to run by hand.
# Leaves the app alone while it is open and tries again next time.
set -euo pipefail
BASE="https://github.com/daniel-stafford/vWordCraft/releases/download/daily"
APP="/Applications/vWordCraft.app"

log() { echo "$(date '+%Y-%m-%d %H:%M') $*"; }

remote="$(curl -fsSL "$BASE/build.txt")" || { log "no network or no build yet"; exit 0; }
installed="$(cat "$APP/Contents/Resources/build.txt" 2>/dev/null || true)"
if [ "$remote" = "$installed" ]; then
  exit 0
fi
if pgrep -xq vWordCraft; then
  log "build $remote is ready, but vWordCraft is open; will retry"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/vWordCraft.zip" "$BASE/vWordCraft-macos-arm64.zip"
ditto -x -k "$tmp/vWordCraft.zip" "$tmp/new"
codesign --verify --strict --deep "$tmp/new/vWordCraft.app"
new="$(cat "$tmp/new/vWordCraft.app/Contents/Resources/build.txt")"

rm -rf "$APP.old"
if [ -d "$APP" ]; then mv "$APP" "$APP.old"; fi
ditto "$tmp/new/vWordCraft.app" "$APP"
rm -rf "$APP.old"
log "installed build $new"
osascript -e "display notification \"Installed build $new\" with title \"vWordCraft updated\"" || true
