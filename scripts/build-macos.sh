#!/bin/bash
# Builds the macOS customer app: regenerates ui/dist, builds the .app and .dmg,
# then optionally signs and notarizes. Run from the project root (macos-build/).
# Mirror image of scripts/build_app.ps1 for the Windows build.
#
# Prerequisites: Xcode Command Line Tools, Rust (rustup), Node/npm (for terser),
# python3 with Pillow + numpy (for the icon).
#
# Optional signing/notarization (needed to distribute outside Gatekeeper):
#   export APPLE_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
#   export APPLE_ID="you@example.com"
#   export APPLE_TEAM_ID="TEAMID"
#   export APPLE_APP_SPECIFIC_PASSWORD="xxxx-xxxx-xxxx-xxxx"
#   ./scripts/build-macos.sh
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# 1. Frontend, minified exactly like the Windows build (scripts/build_ui.ps1).
if [ ! -d node_modules ]; then
  echo "== npm install (terser)"
  npm install --no-audit --no-fund
fi
terser="node_modules/.bin/terser"
if [ ! -x "$terser" ]; then
  echo "terser belum terpasang. Jalankan: npm install" >&2
  exit 1
fi
echo "== ui/dist"
rm -rf ui/dist
mkdir -p ui/dist
for js in art.js main.js; do
  "$terser" "ui/$js" --compress --mangle --comments false -o "ui/dist/$js"
  echo "minified $js"
done
cp ui/index.html ui/style.css ui/brand.png ui/dist/
echo "ui/dist ready"

# 2. Icon: builds src-tauri/icons/icon.icns from the Wedugames logo.
echo "== icon.icns"
python3 scripts/make_icns.py

# 3. The .app and .dmg (bundle config lives in src-tauri/tauri.conf.json).
echo "== tauri build"
npx @tauri-apps/cli@2 build
mkdir -p releases
cp target/release/bundle/dmg/*.dmg releases/ 2>/dev/null || true

# 4. Optional signing + notarization.
if [ -n "${APPLE_SIGN_IDENTITY:-}" ]; then
  app="target/release/bundle/macos/Wedugames FM Packs.app"
  echo "== codesign $app"
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" "$app"
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]; then
    profile="fm-packs-notary"
    dmg="target/release/bundle/dmg/Wedugames FM Packs.dmg"
    echo "== notarize"
    xcrun notarytool store-credentials "$profile" \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD"
    xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait
    xcrun stapler staple "$dmg"
    xcrun stapler staple "$app"
  else
    echo "== APPLE_ID / APPLE_TEAM_ID / APPLE_APP_SPECIFIC_PASSWORD belum di-set: lewati notarization"
  fi
fi

echo "== selesai:"
ls -lh target/release/bundle/macos/*.app 2>/dev/null || true
ls -lh target/release/bundle/dmg/*.dmg releases/*.dmg 2>/dev/null || true