#!/usr/bin/env bash
# Builds a universal (Apple Silicon + Intel) scr8.app and packs it into a .dmg.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
export MACOSX_DEPLOYMENT_TARGET=11.0

cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

APP=target/scr8.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/scr8" \
    target/aarch64-apple-darwin/release/scr8 \
    target/x86_64-apple-darwin/release/scr8
sed "s/VERSION/$VERSION/g" macos/Info.plist > "$APP/Contents/Info.plist"

# App icon, drawn by the app itself.
ICONSET=target/scr8.iconset
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    "$APP/Contents/MacOS/scr8" --write-icon "$s" "$ICONSET/icon_${s}x${s}.png"
    "$APP/Contents/MacOS/scr8" --write-icon "$((s * 2))" "$ICONSET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/scr8.icns"

# Ad-hoc signature: required for Apple Silicon to launch the binary at all.
codesign --force --deep --sign - "$APP"

# Disk image with the app next to an Applications shortcut to drag it onto.
STAGE=target/dmg
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f target/scr8-macos.dmg
hdiutil create -volname scr8 -srcfolder "$STAGE" -ov -format UDZO target/scr8-macos.dmg
echo "Built target/scr8-macos.dmg"
