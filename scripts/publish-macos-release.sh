#!/usr/bin/env bash
# Refuse to promote local/ad-hoc or incomplete notarization builds.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
DIST_DIR="${1:-$ROOT_DIR/dist}"
export MACOS_SIGNING_MODE=developer-id
source "$ROOT_DIR/scripts/lib/macos-signing.sh"
python3 - "$DIST_DIR" <<'PY'
import hashlib,json,pathlib,re,subprocess,sys
root=pathlib.Path(sys.argv[1]).resolve()
try:
    manifest=json.loads((root/'distribution-manifest.json').read_text())
except (OSError,json.JSONDecodeError):
    raise SystemExit('Refusing publication: a completed distribution-manifest.json from Developer ID packaging is required.')
if manifest.get('signingMode')!='developer-id' or manifest.get('notarized') is not True or manifest.get('gatekeeperVerified') is not True:
    raise SystemExit('Refusing publication: Developer ID signing, notarization and Gatekeeper verification must all complete.')
commit=manifest.get('sourceCommit','')
if not isinstance(commit,str) or not re.fullmatch(r'[0-9a-f]{40}',commit):
    raise SystemExit('Refusing publication: a full source commit is required.')
if subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()!=commit:
    raise SystemExit('Refusing publication: installers were built from a different commit.')
if subprocess.check_output(['git','status','--porcelain'],text=True).strip():
    raise SystemExit('Refusing publication: commit the source changes before publishing.')
version=manifest.get('version','')
if not isinstance(version,str) or not re.fullmatch(r'\d+\.\d+\.\d+',version):
    raise SystemExit('Refusing publication: invalid release version.')
expected={f'Mini ChatGPT-{version}-macos.dmg',f'Mini ChatGPT-{version}-macos.pkg'}
artifacts=manifest.get('artifacts',[])
if not isinstance(artifacts,list) or len(artifacts)!=2 or {a.get('name') for a in artifacts}!=expected:
    raise SystemExit('Refusing publication: both Universal 2 installer records are required.')
for artifact in artifacts:
    path=root/artifact['name']
    if not path.is_file() or artifact.get('sha256')!=hashlib.sha256(path.read_bytes()).hexdigest():
        raise SystemExit('Refusing publication: installer hash mismatch.')
PY
VERSION="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$DIST_DIR/distribution-manifest.json")"
APP_BUNDLE="$DIST_DIR/Mini ChatGPT.app"
DMG_PATH="$DIST_DIR/Mini ChatGPT-${VERSION}-macos.dmg"
PKG_PATH="$DIST_DIR/Mini ChatGPT-${VERSION}-macos.pkg"
codesign --verify --deep --strict --all-architectures "$APP_BUNDLE"
xcrun lipo "$APP_BUNDLE/Contents/MacOS/mcodex" -verify_arch arm64 x86_64
xcrun stapler validate "$APP_BUNDLE"
macos_assess_app "$APP_BUNDLE"
macos_assess_installers "$DMG_PATH" "$PKG_PATH"
NOTES="$DIST_DIR/release-notes.md"
[[ -f "$NOTES" ]] || { echo "Write $NOTES before publishing." >&2; exit 1; }
SOURCE_COMMIT="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["sourceCommit"])' "$DIST_DIR/distribution-manifest.json")"
[[ "$(git rev-parse HEAD)" == "$SOURCE_COMMIT" && -z "$(git status --porcelain)" ]] || {
  echo 'Refusing publication: the source checkout changed during verification.' >&2; exit 1;
}
gh release create "v$VERSION" --target "$SOURCE_COMMIT" --title "v$VERSION" --notes-file "$NOTES" --latest \
  "$DMG_PATH#DMG - notarized Universal 2" "$PKG_PATH#PKG - notarized Universal 2" \
  "$DIST_DIR/distribution-manifest.json"
