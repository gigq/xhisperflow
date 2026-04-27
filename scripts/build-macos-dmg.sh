#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="xhisperflow"
VERSION="${VERSION:-$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' "$ROOT_DIR/Cargo.toml")}"
APP_DIR="${APP_DIR:-$ROOT_DIR/target/$APP_NAME.app}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/.build/dist}"
STAGING_DIR="$ROOT_DIR/.build/dmg"
DMG_NAME="${DMG_NAME:-$APP_NAME-$VERSION-universal.dmg}"
VOLUME_NAME="${VOLUME_NAME:-xhisperflow}"
DMG_PATH="$DIST_DIR/$DMG_NAME"
SIGNING_IDENTITY="${SIGNING_IDENTITY:--}"

if [[ ! -d "$APP_DIR" ]]; then
  echo "App bundle not found at:"
  echo "  $APP_DIR"
  exit 1
fi

rm -rf "$STAGING_DIR" "$DMG_PATH"
mkdir -p "$STAGING_DIR" "$DIST_DIR"

cp -R "$APP_DIR" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"

hdiutil create \
  -volname "$VOLUME_NAME" \
  -srcfolder "$STAGING_DIR" \
  -ov \
  -format UDZO \
  "$DMG_PATH" \
  >/dev/null

if [[ "$SIGNING_IDENTITY" != "-" ]]; then
  codesign --force --sign "$SIGNING_IDENTITY" --timestamp "$DMG_PATH" >/dev/null
fi

printf '%s\n' "$DMG_PATH"
