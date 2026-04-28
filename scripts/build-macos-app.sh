#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="xhisperflow"
BUNDLE_ID="com.gigq.xhisperflow"
BUILD_DIR="${BUILD_DIR:-$ROOT_DIR/target}"
APP_DIR="${APP_DIR:-$BUILD_DIR/$APP_NAME.app}"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ICON_FILE="AppIcon.icns"
ICON_SOURCE="$ROOT_DIR/assets/macos/$ICON_FILE"
ARCHS="${ARCHS:-native}"
VERSION="${VERSION:-$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' "$ROOT_DIR/Cargo.toml")}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
MINIMUM_SYSTEM_VERSION="${MINIMUM_SYSTEM_VERSION:-12.0}"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-}"
ENABLE_HARDENED_RUNTIME="${ENABLE_HARDENED_RUNTIME:-0}"
ENTITLEMENTS="$ROOT_DIR/scripts/macos-entitlements.plist"

cd "$ROOT_DIR"

if [[ -z "$SIGNING_IDENTITY" ]]; then
  SIGNING_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '/Developer ID Application:/ { print $2; exit }')"
fi

if [[ -z "$SIGNING_IDENTITY" ]]; then
  SIGNING_IDENTITY="-"
fi

target_for_arch() {
  case "$1" in
    native)
      printf '%s\n' ""
      ;;
    arm64 | aarch64)
      printf '%s\n' "aarch64-apple-darwin"
      ;;
    x86_64)
      printf '%s\n' "x86_64-apple-darwin"
      ;;
    *)
      printf 'unknown macOS arch: %s\n' "$1" >&2
      return 1
      ;;
  esac
}

build_slice() {
  local arch="$1"
  local target
  target="$(target_for_arch "$arch")"

  if [[ -n "$target" ]]; then
    cargo build --release --bin xhisperflow-mac --target "$target"
    printf '%s\n' "$ROOT_DIR/target/$target/release/xhisperflow-mac"
  else
    cargo build --release --bin xhisperflow-mac
    printf '%s\n' "$ROOT_DIR/target/release/xhisperflow-mac"
  fi
}

declare -a executable_slices=()
for arch in $ARCHS; do
  executable_slices+=("$(build_slice "$arch")")
done

for executable_path in "${executable_slices[@]}"; do
  if [[ ! -x "$executable_path" ]]; then
    echo "Built executable not found at:"
    echo "  $executable_path"
    exit 1
  fi
done

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

if [[ "${#executable_slices[@]}" -eq 1 ]]; then
  cp "${executable_slices[0]}" "$MACOS_DIR/$APP_NAME"
else
  lipo -create "${executable_slices[@]}" -output "$MACOS_DIR/$APP_NAME"
fi

chmod +x "$MACOS_DIR/$APP_NAME"
cp "$ICON_SOURCE" "$RESOURCES_DIR/$ICON_FILE"

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$BUILD_NUMBER</string>
  <key>LSMinimumSystemVersion</key>
  <string>$MINIMUM_SYSTEM_VERSION</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSMicrophoneUsageDescription</key>
  <string>xhisperflow records microphone audio for dictation transcription.</string>
</dict>
</plist>
PLIST

printf 'APPL????' > "$CONTENTS_DIR/PkgInfo"

codesign_args=(--force --deep --sign "$SIGNING_IDENTITY")
if [[ "$SIGNING_IDENTITY" != "-" ]]; then
  codesign_args+=(--timestamp)
  if [[ "$ENABLE_HARDENED_RUNTIME" == "1" ]]; then
    codesign_args+=(--options runtime --entitlements "$ENTITLEMENTS")
  fi
fi

codesign "${codesign_args[@]}" "$APP_DIR" >/dev/null

printf '%s\n' "$APP_DIR"
