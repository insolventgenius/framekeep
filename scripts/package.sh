#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

export MACOSX_DEPLOYMENT_TARGET=13.0
cargo build --locked --release "$@"

icon_work=$(mktemp -d "${TMPDIR:-/tmp}/framekeep-icon.XXXXXX")
trap 'rm -rf "$icon_work"' EXIT
xcrun clang -fobjc-arc -DFRAMEKEEP_ICON_TOOL native/icon.m \
    -framework Foundation -framework CoreGraphics -framework ImageIO -o "$icon_work/render-icon"
"$icon_work/render-icon" "Small Robot.png" "$icon_work/Framekeep.icns"

app="$PWD/dist/Framekeep.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/ui"
install -m 755 "${CARGO_TARGET_DIR:-target}/release/framekeep" "$app/Contents/MacOS/Framekeep"
install -m 644 native/Info.plist "$app/Contents/Info.plist"
install -m 644 "$icon_work/Framekeep.icns" "$app/Contents/Resources/Framekeep.icns"
install -m 644 ui/index.html ui/app.js ui/style.css ui/welcome.jpg "$app/Contents/Resources/ui"
codesign --force --sign - "$app"
printf '%s\n' "$app"
