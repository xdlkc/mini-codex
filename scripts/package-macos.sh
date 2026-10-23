#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

APP_NAME="Mini ChatGPT"
BUNDLE_ID="com.minicodex.desktop"
BUILD_PROFILE="release"
BUILD_ARCH=""
SIGNING_MODE="${MACOS_SIGNING_MODE:-}"
APP_ONLY=0
REGEN_ICON=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) BUILD_PROFILE="debug"; shift ;;
    --app-only) APP_ONLY=1; shift ;;
    --regen-icon) REGEN_ICON=1; shift ;;
    --signing)
      [[ $# -ge 2 ]] || { echo "--signing requires developer-id or ad-hoc" >&2; exit 2; }
      SIGNING_MODE="$2"; shift 2 ;;
    --arch)
      [[ $# -ge 2 ]] || { echo "--arch requires universal, arm64, x86_64 or native" >&2; exit 2; }
      BUILD_ARCH="$2"; shift 2 ;;
    --help)
      echo "Usage: scripts/package-macos.sh [--arch universal|arm64|x86_64|native] [--debug] [--app-only] [--regen-icon] [--signing developer-id|ad-hoc]"
      echo "Release builds default to Universal 2 (Apple Silicon and Intel); debug builds default to native."
      echo "Release builds require Developer ID identities and a notarytool Keychain profile. Ad-hoc builds are local tests only."
      exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done
if [[ -z "$SIGNING_MODE" ]]; then
  if [[ "$BUILD_PROFILE" == release ]]; then SIGNING_MODE=developer-id; else SIGNING_MODE=ad-hoc; fi
fi
export MACOS_SIGNING_MODE="$SIGNING_MODE"
source "$ROOT_DIR/scripts/lib/macos-signing.sh"
macos_distribution_preflight "$APP_ONLY"
if [[ -z "$BUILD_ARCH" ]]; then
  if [[ "$BUILD_PROFILE" == "release" ]]; then BUILD_ARCH="universal"; else BUILD_ARCH="native"; fi
fi
HOST_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
case "$BUILD_ARCH" in
  universal) BUILD_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin); EXPECTED_ARCHS=(arm64 x86_64) ;;
  arm64) BUILD_TARGETS=(aarch64-apple-darwin); EXPECTED_ARCHS=(arm64) ;;
  x86_64) BUILD_TARGETS=(x86_64-apple-darwin); EXPECTED_ARCHS=(x86_64) ;;
  native)
    BUILD_TARGETS=("$HOST_TARGET")
    case "$HOST_TARGET" in
      aarch64-apple-darwin) EXPECTED_ARCHS=(arm64) ;;
      x86_64-apple-darwin) EXPECTED_ARCHS=(x86_64) ;;
      *) echo "A macOS build host is required" >&2; exit 2 ;;
    esac ;;
  *) echo "Unsupported macOS architecture: $BUILD_ARCH" >&2; exit 2 ;;
esac
for target in "${BUILD_TARGETS[@]}"; do
  target_lib="$(rustc --print target-libdir --target "$target")"
  if [[ ! -d "$target_lib" ]]; then
    echo "Missing Rust target. Run: rustup target add $target" >&2
    exit 1
  fi
done
# Both slices must support the same minimum macOS version as Info.plist.
export MACOSX_DEPLOYMENT_TARGET="12.0"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
[[ "$TARGET_DIR" = /* ]] || TARGET_DIR="$ROOT_DIR/$TARGET_DIR"
VERSION="$(grep -E '^version\s*=' Cargo.toml | head -n1 | sed -E 's/.*"([^"]+)".*/\1/')"
DIST_DIR="$ROOT_DIR/dist"
APP_BUNDLE="$DIST_DIR/${APP_NAME}.app"
ARTIFACT_SUFFIX=""
[[ "$BUILD_ARCH" == "universal" ]] || ARTIFACT_SUFFIX="-${EXPECTED_ARCHS[0]}"
[[ "$BUILD_PROFILE" != "debug" ]] || ARTIFACT_SUFFIX="${ARTIFACT_SUFFIX}-debug"
[[ "$SIGNING_MODE" != ad-hoc ]] || ARTIFACT_SUFFIX="${ARTIFACT_SUFFIX}-unnotarized"
DMG_PATH="$DIST_DIR/${APP_NAME}-${VERSION}-macos${ARTIFACT_SUFFIX}.dmg"
PKG_PATH="$DIST_DIR/${APP_NAME}-${VERSION}-macos${ARTIFACT_SUFFIX}.pkg"

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

echo "==> [2/5] Building ${BUILD_PROFILE} ${BUILD_ARCH} binary (mcodex v${VERSION})..."
BUILD_INPUTS=()
for target in "${BUILD_TARGETS[@]}"; do
  build_args=(build --locked --bin mcodex)
  [[ "$BUILD_PROFILE" != "release" ]] || build_args+=(--release)
  if [[ "$BUILD_ARCH" == "native" ]]; then
    cargo "${build_args[@]}"
    BUILD_INPUTS+=("$TARGET_DIR/$BUILD_PROFILE/mcodex")
  else
    cargo "${build_args[@]}" --target "$target"
    BUILD_INPUTS+=("$TARGET_DIR/$target/$BUILD_PROFILE/mcodex")
  fi
done
if [[ "$BUILD_ARCH" == "universal" ]]; then
  UNIVERSAL_DIR="$TARGET_DIR/universal-apple-darwin/$BUILD_PROFILE"
  mkdir -p "$UNIVERSAL_DIR"
  BUILT_BINARY="$UNIVERSAL_DIR/mcodex"
  xcrun lipo -create "${BUILD_INPUTS[@]}" -output "$BUILT_BINARY"
else
  BUILT_BINARY="${BUILD_INPUTS[0]}"
fi
# Refuse to emit an installer if a requested architecture is missing.
xcrun lipo "$BUILT_BINARY" -verify_arch "${EXPECTED_ARCHS[@]}"

echo "==> [3/5] Assembling ${APP_NAME}.app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "$BUILT_BINARY" "$APP_BUNDLE/Contents/MacOS/mcodex"
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

macos_sign_app "$APP_BUNDLE"
xcrun lipo "$APP_BUNDLE/Contents/MacOS/mcodex" -verify_arch "${EXPECTED_ARCHS[@]}"
codesign --verify --deep --strict --all-architectures "$APP_BUNDLE"
if [[ "$SIGNING_MODE" == developer-id ]]; then
  macos_notarize "$APP_BUNDLE" "$DIST_DIR/notarization"
  macos_assess_app "$APP_BUNDLE"
else
  echo "LOCAL TEST BUILD: ad-hoc signed, not notarized. Other Macs may reject this app." >&2
fi

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
if [[ "$SIGNING_MODE" == developer-id ]]; then
  codesign --force --sign "$MACOS_APP_SIGN_IDENTITY" --timestamp "$DMG_PATH"
fi

echo "==> [5/5] Building macOS Installer PKG (${PKG_PATH})..."
rm -f "$PKG_PATH"
PKG_STAGE="$(mktemp -d)"
mkdir -p "$PKG_STAGE/root/Applications"
mkdir -p "$PKG_STAGE/root/usr/local/bin"
cp -R "$APP_BUNDLE" "$PKG_STAGE/root/Applications/${APP_NAME}.app"
ln -s "/Applications/${APP_NAME}.app/Contents/MacOS/mcodex" "$PKG_STAGE/root/usr/local/bin/mcodex"

UNSIGNED_PKG="$PKG_STAGE/unsigned.pkg"
pkgbuild \
  --root "$PKG_STAGE/root" \
  --identifier "${BUNDLE_ID}.pkg" \
  --version "${VERSION}" \
  --install-location "/" \
  "$UNSIGNED_PKG" >/dev/null
if [[ "$SIGNING_MODE" == developer-id ]]; then
  productsign --sign "$MACOS_INSTALLER_SIGN_IDENTITY" --timestamp "$UNSIGNED_PKG" "$PKG_PATH"
else
  mv "$UNSIGNED_PKG" "$PKG_PATH"
fi
rm -rf "$PKG_STAGE"
if [[ "$SIGNING_MODE" == developer-id ]]; then
  macos_notarize "$DMG_PATH" "$DIST_DIR/notarization"
  macos_notarize "$PKG_PATH" "$DIST_DIR/notarization"
  macos_assess_installers "$DMG_PATH" "$PKG_PATH"
fi
python3 - "$DIST_DIR" "$APP_BUNDLE" "$DMG_PATH" "$PKG_PATH" "$SIGNING_MODE" "$VERSION" <<'PY'
import hashlib,json,pathlib,sys,os,subprocess
directory,app,dmg,pkg,mode,version=sys.argv[1:]
artifacts=[{'name':pathlib.Path(p).name,'sha256':hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()} for p in (dmg,pkg)]
commit=os.environ.get('MACOS_SOURCE_COMMIT')
if not commit:
    result=subprocess.run(['git','rev-parse','HEAD'],text=True,capture_output=True)
    commit=result.stdout.strip() if result.returncode==0 else None
pathlib.Path(directory,'distribution-manifest.json').write_text(json.dumps({'version':version,'sourceCommit':commit,'signingMode':mode,'notarized':mode=='developer-id','gatekeeperVerified':mode=='developer-id','appName':pathlib.Path(app).name,'artifacts':artifacts},indent=2)+'\n')
PY

echo ""
echo "Packaging complete:"
echo "  App Bundle : $APP_BUNDLE"
echo "  DMG Image  : $DMG_PATH"
echo "  PKG Install: $PKG_PATH"
