#!/usr/bin/env bash
# Ad-hoc sign an Apple Silicon OpenLess.app that contains mlx.metallib, then
# rebuild the DMG from the signed app.
#
# Tauri copies bundle.macOS.files into Contents/MacOS/ but does not add them to
# its sign list. codesign then rejects the unsigned metallib as nested code.
# This script is the post-bundle counterpart: sign inside-out, then pack a new
# DMG so the installer matches the signed .app.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <OpenLess.app> <OpenLess_version_aarch64.dmg>" >&2
  exit 1
fi

APP="$1"
DMG_PATH="$2"
ENTITLEMENTS="${ENTITLEMENTS:-$(cd "$(dirname "$0")/.." && pwd)/src-tauri/Entitlements.plist}"
IDENTITY="${APPLE_SIGNING_IDENTITY:--}"

if [ ! -d "$APP" ]; then
  echo "✗ missing app bundle: $APP" >&2
  exit 1
fi
if [ ! -f "$ENTITLEMENTS" ]; then
  echo "✗ missing entitlements: $ENTITLEMENTS" >&2
  exit 1
fi

METALLIB="$APP/Contents/MacOS/mlx.metallib"
if [ ! -s "$METALLIB" ]; then
  echo "✗ Apple Silicon app is missing Contents/MacOS/mlx.metallib" >&2
  exit 1
fi

echo "▶ ad-hoc signing nested MLX metallib"
codesign --force --sign "$IDENTITY" --timestamp=none "$METALLIB"

echo "▶ ad-hoc signing $APP"
codesign --force --sign "$IDENTITY" --timestamp=none --entitlements "$ENTITLEMENTS" "$APP"

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/openless-adhoc-dmg.XXXXXX")"
cleanup_stage() {
  rm -rf "$STAGE"
}
trap cleanup_stage EXIT

APP_NAME="$(basename "$APP")"
ditto "$APP" "$STAGE/$APP_NAME"
ln -s /Applications "$STAGE/Applications"

echo "▶ rebuilding DMG from signed app: $DMG_PATH"
mkdir -p "$(dirname "$DMG_PATH")"
rm -f "$DMG_PATH"
hdiutil create -volname OpenLess -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH" >/dev/null
echo "✓ ad-hoc signed $APP_NAME and rebuilt $(basename "$DMG_PATH")"
