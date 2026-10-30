#!/bin/zsh
# Builds the native macOS app "Aether.app" (AppKit + WKWebView) with the
# release dsh-codex binary and "DSH Computer Use.app" inside, signs it and
# packs a disk image.
#
#   desktop/build.sh               build, sign, desktop/build/Aether.app + Aether.dmg
#   desktop/build.sh --no-sign     unsigned (CI)
#   desktop/build.sh --ad-hoc      portable local signature (no signing identity)
#   desktop/build.sh --skip-cargo  reuse target/release/dsh-codex
#   desktop/build.sh --no-dmg      app bundle only
#
# Signing uses DSH_DESKTOP_SIGN_IDENTITY, else the first "Apple Development"
# identity, else an ad-hoc signature. No notarisation.
set -euo pipefail
HERE="${0:A:h}"
ROOT="${HERE:h}"
cd "$HERE"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo "error: the desktop package currently requires an Apple Silicon Mac" >&2
  exit 1
fi

NO_SIGN=0 SKIP_CARGO=0 NO_DMG=0 AD_HOC=0
for arg in "$@"; do
  case "$arg" in
    --no-sign) NO_SIGN=1 ;;
    --ad-hoc) AD_HOC=1 ;;
    --skip-cargo) SKIP_CARGO=1 ;;
    --no-dmg) NO_DMG=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

VERSION="$(awk -F'"' '/^version *=/{print $2; exit}' "$ROOT/Cargo.toml")"
BUILD="$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)"
APP="build/Aether.app"
DMG="build/Aether.dmg"
IDENTITY=""
if (( ! NO_SIGN && ! AD_HOC )); then
  IDENTITY="${DSH_DESKTOP_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $2; exit}')}"
fi

if (( ! SKIP_CARGO )); then
  echo "==> cargo build --release -p dsh-codex"
  (cd "$ROOT" && cargo build --locked --release -p dsh-codex)
fi
BIN="$ROOT/target/release/dsh-codex"
[[ -x "$BIN" ]] || { echo "error: $BIN missing (drop --skip-cargo)" >&2; exit 1; }

echo "==> DSH Computer Use.app"
if (( NO_SIGN )); then
  DSH_CUA_SIGN_IDENTITY="" "$ROOT/helper/build.sh" --no-sign
elif [[ -n "$IDENTITY" ]]; then
  DSH_CUA_SIGN_IDENTITY="$IDENTITY" "$ROOT/helper/build.sh"
elif (( AD_HOC )); then
  DSH_CUA_SIGN_IDENTITY="-" "$ROOT/helper/build.sh"
else
  "$ROOT/helper/build.sh"
fi

echo "==> pinned Node.js runtime for PTC"
python3 fetch-node.py

echo "==> Aether.app $VERSION ($BUILD)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Helpers" build/tmp
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" Info.plist > "$APP/Contents/Info.plist"
xcrun swiftc -O -swift-version 5 -target arm64-apple-macos14.0 \
  -framework AppKit -framework WebKit -framework UserNotifications -framework Security \
  Sources/*.swift -o "$APP/Contents/MacOS/dsh-codex-desktop"
cp "$BIN" "$APP/Contents/MacOS/dsh-codex"
cp build/runtime/node "$APP/Contents/MacOS/node"
cp build/runtime/NODE-LICENSE.txt "$APP/Contents/Resources/NODE-LICENSE.txt"
cp -R "$ROOT/helper/build/DSH Computer Use.app" "$APP/Contents/Helpers/"

xcrun swiftc -O -swift-version 5 -target arm64-apple-macos14.0 -framework AppKit \
  Tools/make-icon.swift -o build/tmp/make-icon
rm -rf build/tmp/AppIcon.iconset
build/tmp/make-icon build/tmp/AppIcon.iconset
iconutil -c icns build/tmp/AppIcon.iconset -o "$APP/Contents/Resources/AppIcon.icns"

if (( NO_SIGN )); then
  echo "built without signature: $APP"
else
  SIGN=(--force --sign -)
  if [[ -n "$IDENTITY" ]]; then
    SIGN=(--force --options runtime --timestamp=none --sign "$IDENTITY")
    echo "==> signing with $IDENTITY"
  else
    echo "warning: no signing identity, using ad-hoc signature" >&2
    codesign --force --sign - "$APP/Contents/Helpers/DSH Computer Use.app"
  fi
  # Inside out: nested code first, then the bundle.
  codesign "${SIGN[@]}" --entitlements node-entitlements.plist "$APP/Contents/MacOS/node"
  codesign "${SIGN[@]}" "$APP/Contents/MacOS/dsh-codex"
  codesign "${SIGN[@]}" "$APP"
  codesign --verify --deep --strict "$APP"
fi

if (( ! NO_DMG )); then
  echo "==> $DMG"
  STAGE="build/tmp/dmg"
  rm -rf "$STAGE" "$DMG"
  mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Programme"
  hdiutil create -quiet -volname "Aether $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
  [[ -n "$IDENTITY" ]] && codesign --force --timestamp=none --sign "$IDENTITY" "$DMG"
  rm -rf "$STAGE"
fi
rm -rf build/tmp
echo "done: desktop/$APP"
(( NO_DMG )) || echo "      desktop/$DMG"
