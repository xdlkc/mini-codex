#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

APP_NAME="Mini ChatGPT"
BUNDLE_ID="com.minicodex.desktop"
BUILD_PROFILE="release"
APP_ONLY=0
REGEN_ICON=0
for argument in "$@"; do
  case "$argument" in
    --debug) BUILD_PROFILE="debug" ;;
    --app-only) APP_ONLY=1 ;;
    --regen-icon) REGEN_ICON=1 ;;
    *) echo "Unknown option: $argument" >&2; exit 2 ;;
  esac
done
VERSION="$(grep -E '^version\s*=' Cargo.toml | head -n1 | sed -E 's/.*"([^"]+)".*/\1/')"
DIST_DIR="$ROOT_DIR/dist"
APP_BUNDLE="$DIST_DIR/${APP_NAME}.app"
DMG_PATH="$DIST_DIR/${APP_NAME}-${VERSION}-macos.dmg"
PKG_PATH="$DIST_DIR/${APP_NAME}-${VERSION}-macos.pkg"

echo "==> [1/5] Preparing App Icon (assets/AppIcon.icns)..."
if [[ ! -f "assets/AppIcon.icns" || ! -f "assets/icon_256.png" || "$REGEN_ICON" == "1" ]]; then
  swift -e '
import AppKit
let svgURL = URL(fileURLWithPath: "assets/icon.svg")
guard let img = NSImage(contentsOf: svgURL) else { fatalError("Failed to load assets/icon.svg") }
let size = NSSize(width: 1024, height: 1024)
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
)!
rep.size = size
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high
NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowOffset = NSSize(width: 0, height: -14)
shadow.shadowBlurRadius = 28
shadow.shadowColor = NSColor(calibratedWhite: 0.0, alpha: 0.50)
shadow.set()
let squircle = NSBezierPath(roundedRect: NSRect(x: 100, y: 100, width: 824, height: 824), xRadius: 185, yRadius: 185)
NSColor.black.setFill()
squircle.fill()
NSGraphicsContext.restoreGraphicsState()
img.draw(in: NSRect(origin: .zero, size: size), from: .zero, operation: .sourceOver, fraction: 1.0)
NSGraphicsContext.restoreGraphicsState()
let pngData = rep.representation(using: .png, properties: [:])!
try! pngData.write(to: URL(fileURLWithPath: "assets/icon.png"))
'
  sips -z 256 256 assets/icon.png --out assets/icon_256.png >/dev/null
  ICONSET_DIR="$(mktemp -d)/MiniChatGPT.iconset"
  mkdir -p "$ICONSET_DIR"
  for sz in 16 32 128 256 512; do
    sips -z "$sz" "$sz" assets/icon.png --out "$ICONSET_DIR/icon_${sz}x${sz}.png" >/dev/null
    sz2=$((sz * 2))
    sips -z "$sz2" "$sz2" assets/icon.png --out "$ICONSET_DIR/icon_${sz}x${sz}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET_DIR" -o assets/AppIcon.icns
  rm -rf "$(dirname "$ICONSET_DIR")"
fi

echo "==> [2/5] Building ${BUILD_PROFILE} binary (mcodex v${VERSION})..."
if [[ "$BUILD_PROFILE" == "release" ]]; then cargo build --release; else cargo build; fi

echo "==> [3/5] Assembling ${APP_NAME}.app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "target/$BUILD_PROFILE/mcodex" "$APP_BUNDLE/Contents/MacOS/mcodex"
chmod +x "$APP_BUNDLE/Contents/MacOS/mcodex"
cp "assets/AppIcon.icns" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
mkdir -p "$APP_BUNDLE/Contents/Resources/ThirdPartyLicenses"
cp src/vendor/LICENSE-*.txt "$APP_BUNDLE/Contents/Resources/ThirdPartyLicenses/"
if [[ -d src/vendor/editor/licenses ]]; then
  cp -R src/vendor/editor/licenses "$APP_BUNDLE/Contents/Resources/ThirdPartyLicenses/Editor"
fi
printf "APPL????" > "$APP_BUNDLE/Contents/PkgInfo"

cat > "$APP_BUNDLE/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>${APP_NAME}</string>
  <key>CFBundleDisplayName</key>
  <string>${APP_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleExecutable</key>
  <string>mcodex</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIconName</key>
  <string>AppIcon</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.productivity</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Mini ChatGPT uses the microphone only when you start a voice conversation.</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP_BUNDLE"

if [[ "$APP_ONLY" == "1" ]]; then
  echo "App bundle ready: $APP_BUNDLE"
  exit 0
fi

echo "==> [4/5] Building DMG disk image (${DMG_PATH})..."
rm -f "$DMG_PATH"
DMG_STAGE="$(mktemp -d)"
cp -R "$APP_BUNDLE" "$DMG_STAGE/${APP_NAME}.app"
ln -s /Applications "$DMG_STAGE/Applications"
cp "assets/AppIcon.icns" "$DMG_STAGE/.VolumeIcon.icns"
SetFile -a C "$DMG_STAGE" 2>/dev/null || true

hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "$DMG_STAGE" \
  -ov \
  -format UDZO \
  "$DMG_PATH" >/dev/null
rm -rf "$DMG_STAGE"

echo "==> [5/5] Building macOS Installer PKG (${PKG_PATH})..."
rm -f "$PKG_PATH"
PKG_STAGE="$(mktemp -d)"
mkdir -p "$PKG_STAGE/root/Applications"
mkdir -p "$PKG_STAGE/root/usr/local/bin"
cp -R "$APP_BUNDLE" "$PKG_STAGE/root/Applications/${APP_NAME}.app"
ln -s "/Applications/${APP_NAME}.app/Contents/MacOS/mcodex" "$PKG_STAGE/root/usr/local/bin/mcodex"

pkgbuild \
  --root "$PKG_STAGE/root" \
  --identifier "${BUNDLE_ID}.pkg" \
  --version "${VERSION}" \
  --install-location "/" \
  "$PKG_PATH" >/dev/null
rm -rf "$PKG_STAGE"

echo ""
echo "Packaging complete:"
echo "  App Bundle : $APP_BUNDLE"
echo "  DMG Image  : $DMG_PATH"
echo "  PKG Install: $PKG_PATH"
