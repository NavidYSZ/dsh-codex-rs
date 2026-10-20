#!/bin/zsh
# Builds "DSH Computer Use.app" and installs it to ~/Applications so the
# macOS privacy grants (Accessibility, Screen Recording) stay attached to one
# stable path and signing identity.
#
#   helper/build.sh              build and sign into helper/build/
#   helper/build.sh --install    build, sign and install to ~/Applications
#   helper/build.sh --no-sign    build only, unsigned (CI); cannot be installed
set -euo pipefail
cd "${0:A:h}"

NO_SIGN=0
for arg in "$@"; do
  if [[ "$arg" == "--no-sign" ]]; then NO_SIGN=1; fi
done
if (( NO_SIGN )) && [[ "${1:-}" == "--install" ]]; then
  echo "error: --no-sign cannot be combined with --install" >&2
  exit 2
fi

APP="build/DSH Computer Use.app"
DEST="$HOME/Applications/DSH Computer Use.app"
IDENTITY=""
if (( ! NO_SIGN )); then
  IDENTITY="${DSH_CUA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $2; exit}')}"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
xcrun swiftc -O -swift-version 5 -target arm64-apple-macos14.0 \
  -framework AppKit -framework ScreenCaptureKit -framework Carbon \
  Sources/*.swift -o "$APP/Contents/MacOS/dsh-cua"

if (( NO_SIGN )); then
  echo "built without signature: $APP"
elif [[ -n "$IDENTITY" ]]; then
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
