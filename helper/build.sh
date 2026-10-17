#!/bin/zsh
# Builds "DSH Computer Use.app" and installs it to ~/Applications so the
# macOS privacy grants (Accessibility, Screen Recording) stay attached to one
# stable path and signing identity.
set -euo pipefail
cd "${0:A:h}"

APP="build/DSH Computer Use.app"
DEST="$HOME/Applications/DSH Computer Use.app"
IDENTITY="${DSH_CUA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $2; exit}')}"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
xcrun swiftc -O -swift-version 5 -target arm64-apple-macos14.0 \
  -framework AppKit -framework ScreenCaptureKit -framework Carbon \
  Sources/*.swift -o "$APP/Contents/MacOS/dsh-cua"

if [[ -n "$IDENTITY" ]]; then
  codesign --force --options runtime --sign "$IDENTITY" "$APP"
else
  echo "warning: no signing identity, using ad-hoc signature (grants reset on every rebuild)" >&2
  codesign --force --sign - "$APP"
fi

if [[ "${1:-}" == "--install" ]]; then
  pkill -x dsh-cua 2>/dev/null || true
  mkdir -p "$HOME/Applications"
  rm -rf "$DEST"
  cp -R "$APP" "$DEST"
  echo "installed: $DEST"
fi
