#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICON_DIR="$ROOT_DIR/assets/macos"
SVG_PATH="$ICON_DIR/AppIcon.svg"
PNG_PATH="$ICON_DIR/AppIcon.svg.png"
ICONSET_DIR="$ICON_DIR/AppIcon.iconset"
ICNS_PATH="$ICON_DIR/AppIcon.icns"

command -v qlmanage >/dev/null
command -v sips >/dev/null
command -v iconutil >/dev/null

rm -rf "$ICONSET_DIR" "$PNG_PATH" "$ICNS_PATH"

qlmanage -t -s 1024 -o "$ICON_DIR" "$SVG_PATH" >/dev/null 2>&1
mkdir -p "$ICONSET_DIR"

sips -z 16 16 "$PNG_PATH" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null
sips -z 32 32 "$PNG_PATH" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$PNG_PATH" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null
sips -z 64 64 "$PNG_PATH" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$PNG_PATH" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null
sips -z 256 256 "$PNG_PATH" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$PNG_PATH" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null
sips -z 512 512 "$PNG_PATH" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$PNG_PATH" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null
sips -z 1024 1024 "$PNG_PATH" --out "$ICONSET_DIR/icon_512x512@2x.png" >/dev/null

iconutil -c icns "$ICONSET_DIR" -o "$ICNS_PATH"
rm -rf "$ICONSET_DIR" "$PNG_PATH"

printf '%s\n' "$ICNS_PATH"
