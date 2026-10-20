#!/usr/bin/env bash
# Build a real headless binary for the requested target, plus its portable source archive.
set -euo pipefail

AGENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$AGENT_ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
AGENT_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
AGENT_PROFILE="release"
AGENT_OUTPUT="$AGENT_ROOT/dist/agent"
AGENT_SOURCE_ONLY=0
AGENT_TARGET_EXPLICIT=0
AGENT_OFFLINE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) [[ $# -ge 2 ]] || { echo "--target needs a Rust target triple" >&2; exit 2; }; AGENT_TARGET="$2"; AGENT_TARGET_EXPLICIT=1; shift 2 ;;
    --output) [[ $# -ge 2 ]] || { echo "--output needs a directory" >&2; exit 2; }; AGENT_OUTPUT="$2"; shift 2 ;;
    --debug) AGENT_PROFILE="debug"; shift ;;
    --source-only) AGENT_SOURCE_ONLY=1; shift ;;
    --offline) AGENT_OFFLINE=1; shift ;;
    --help) echo "Usage: scripts/package-agent.sh [--target TRIPLE] [--debug] [--offline] [--output DIR] [--source-only]"; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done
if [[ "$AGENT_SOURCE_ONLY" == 1 && "$AGENT_TARGET_EXPLICIT" == 1 ]]; then
  echo "--target requires a binary build. Omit --target when using --source-only." >&2
  exit 2
fi
if [[ ! "$AGENT_TARGET" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]]; then
  echo "Expected a Rust target triple, not a path: $AGENT_TARGET" >&2
  exit 2
fi
AGENT_VERSION="$(python3 - <<'PY'
import re
from pathlib import Path
print(re.search(r'^version\s*=\s*"([^"]+)"', Path('Cargo.toml').read_text(), re.M)[1])
PY
)"

AGENT_BINARY=""
if [[ "$AGENT_SOURCE_ONLY" == 0 ]]; then
  AGENT_BUILD=(build --locked --no-default-features --bin mcodex --target "$AGENT_TARGET")
  [[ "$AGENT_PROFILE" == release ]] && AGENT_BUILD+=(--release)
  [[ "$AGENT_OFFLINE" == 1 ]] && AGENT_BUILD+=(--offline)
  echo "Building headless agent v$AGENT_VERSION for $AGENT_TARGET ($AGENT_PROFILE)"
  cargo "${AGENT_BUILD[@]}"
  AGENT_TARGET_DIR="${CARGO_TARGET_DIR:-$AGENT_ROOT/target}"
  [[ "$AGENT_TARGET_DIR" = /* ]] || AGENT_TARGET_DIR="$AGENT_ROOT/$AGENT_TARGET_DIR"
  AGENT_EXECUTABLE="mcodex"
  [[ "$AGENT_TARGET" == *windows* ]] && AGENT_EXECUTABLE="mcodex.exe"
  AGENT_BINARY="$AGENT_TARGET_DIR/$AGENT_TARGET/$AGENT_PROFILE/$AGENT_EXECUTABLE"
  [[ -f "$AGENT_BINARY" ]] || { echo "Cargo did not produce the requested target binary: $AGENT_BINARY" >&2; exit 1; }
fi

python3 - "$AGENT_ROOT" "$AGENT_OUTPUT" "$AGENT_VERSION" "$AGENT_TARGET" "$AGENT_PROFILE" "$AGENT_BINARY" <<'PY'
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

root, output = Path(sys.argv[1]), Path(sys.argv[2]).resolve()
version, target, profile, binary = sys.argv[3:]
output.mkdir(parents=True, exist_ok=True)

def digest(path):
    hasher = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(chunk)
    return hasher.hexdigest()

def command(*args):
    result = subprocess.run(args, cwd=root, capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None

metadata = {
    'schemaVersion': 1,
    'name': 'mini-codex-agent',
    'version': version,
    'features': [],
    'createdAt': datetime.now(timezone.utc).isoformat(),
    'gitRevision': command('git', 'rev-parse', 'HEAD'),
    'gitDirty': bool(command('git', 'status', '--porcelain')),
    'rustc': command('rustc', '--version'),
    'transport': 'stdio-jsonl',
    'entrypoint': ['app-server'],
    'stateDirectoryEnvironment': 'MCODEX_HOME',
}
with tempfile.TemporaryDirectory(prefix='.agent-package-', dir=output) as temporary:
    staging = Path(temporary)
    source_name = f'mini-codex-agent-source-{version}'
    source = staging / source_name
    source.mkdir()
    for name in ['Cargo.toml', 'Cargo.lock', 'README.md']:
        shutil.copy2(root / name, source / name)
    # Explicit paths exclude .git, target, dist, local settings, sessions and credentials.
    for name in ['src', 'assets']:
        shutil.copytree(root / name, source / name, symlinks=True)
    (source / 'scripts').mkdir()
    for script in (root / 'scripts').iterdir():
        if script.is_file() and script.suffix in {'.sh', '.py', '.mjs'}:
            shutil.copy2(script, source / 'scripts' / script.name)
    (source / 'docs').mkdir()
    for document in (root / 'docs').glob('*.md'):
        shutil.copy2(document, source / 'docs' / document.name)
    if (root / 'docs/screenshots').is_dir():
        shutil.copytree(root / 'docs/screenshots', source / 'docs/screenshots',
                        ignore=shutil.ignore_patterns('.DS_Store', '__pycache__'))
    source_metadata = {**metadata, 'kind': 'source', 'target': None, 'build': ['cargo', 'build', '--release', '--locked', '--no-default-features', '--bin', 'mcodex']}
    (source / 'agent-source.json').write_text(json.dumps(source_metadata, indent=2) + '\n')
    source_archive = staging / (source_name + '.tar.gz')
    with tarfile.open(source_archive, 'w:gz') as archive:
        archive.add(source, arcname=source_name)
    manifest = {**metadata, 'kind': 'source' if not binary else 'binary', 'target': target if binary else None,
                'sourceArchive': {'file': source_archive.name, 'sha256': digest(source_archive)}}
    artifacts = [source_archive]
    if binary:
        binary = Path(binary)
        bundle_name = f'mini-codex-agent-{version}-{target}'
        bundle = staging / bundle_name
        (bundle / 'bin').mkdir(parents=True)
        executable = bundle / 'bin' / binary.name
        shutil.copy2(binary, executable)
        if not target.endswith('windows-msvc') and not target.endswith('windows-gnu'):
            executable.chmod(0o755)
        license_dir = bundle / 'licenses'
        license_dir.mkdir()
        for license_file in (root / 'src/vendor').glob('LICENSE-*.txt'):
            shutil.copy2(license_file, license_dir / license_file.name)
        if (root / 'src/vendor/editor/licenses').is_dir():
            shutil.copytree(root / 'src/vendor/editor/licenses', license_dir / 'editor')
        shutil.copy2(root / 'docs/HEADLESS_AGENT.md', bundle / 'README.md')
        build_metadata = {**metadata, 'kind': 'binary', 'target': target, 'profile': profile,
                          'binary': {'file': 'bin/' + binary.name, 'sha256': digest(executable)}}
        (bundle / 'manifest.json').write_text(json.dumps(build_metadata, indent=2) + '\n')
        binary_archive = staging / (bundle_name + '.tar.gz')
        with tarfile.open(binary_archive, 'w:gz') as archive:
            archive.add(bundle, arcname=bundle_name)
        manifest.update({'profile': profile, 'binary': build_metadata['binary'],
                         'binaryArchive': {'file': binary_archive.name, 'sha256': digest(binary_archive)}})
        artifacts.append(binary_archive)
        manifest_name = bundle_name + '.json'
    else:
        manifest_name = source_name + '.json'
    manifest_file = staging / manifest_name
    manifest_file.write_text(json.dumps(manifest, indent=2) + '\n')
    artifacts.append(manifest_file)
    for artifact in artifacts:
        destination = output / artifact.name
        os.replace(artifact, destination)
        print(destination)
PY
