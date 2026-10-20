#!/usr/bin/env bash
# Assemble vWordCraft.app (WordCraft with Vim keys) from a built `wordcraft` binary, following
# packaging/macos/package.sh: ad-hoc signed, not notarized.
#
# Usage: packaging/vwordcraft/bundle.sh <wordcraft binary> <output dir> [build id]
# The build id (default: the git commit) is written to Contents/Resources/build.txt, which
# update.sh compares with the published one to decide whether to install.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$1"
OUT="$2"
VERSION="$(awk '/^\[/ { p = ($0 == "[workspace.package]"); next } p && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }' "$ROOT/Cargo.toml")"
SHA="$(git -C "$ROOT" rev-parse --short HEAD)-vim"
BUILD_ID="${3:-$SHA}"
APP="$OUT/vWordCraft.app"
ENT="$ROOT/packaging/macos/entitlements.plist"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/vWordCraft"
cp "$ROOT/assets/app-icon/wordcraft.icns" "$APP/Contents/Resources/vWordCraft.icns"
printf '%s\n' "$BUILD_ID" >"$APP/Contents/Resources/build.txt"
P="$APP/Contents/Info.plist"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHORT_VERSION@/${VERSION%%-*}/g" -e "s/@BUILD_SHA@/$SHA/g" \
  "$ROOT/packaging/macos/Info.plist.in" >"$P"
plutil -replace CFBundleDisplayName -string vWordCraft "$P"
plutil -replace CFBundleName -string vWordCraft "$P"
plutil -replace CFBundleExecutable -string vWordCraft "$P"
plutil -replace CFBundleIconFile -string vWordCraft "$P"
plutil -replace CFBundleIdentifier -string ai.storyteller.wordcraft.vim "$P"
plutil -lint "$P"
printf 'APPL????' >"$APP/Contents/PkgInfo"

xattr -cr "$APP"
codesign --force --sign - --timestamp=none --options runtime --entitlements "$ENT" "$APP/Contents/MacOS/vWordCraft"
codesign --force --sign - --timestamp=none --options runtime --entitlements "$ENT" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"
