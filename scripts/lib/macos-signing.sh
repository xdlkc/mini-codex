#!/usr/bin/env bash
# Distribution credentials stay in Keychain. Never pass passwords in build arguments.
macos_distribution_error() { printf 'macOS distribution: %s\n' "$*" >&2; return 1; }
macos_distribution_preflight() {
  local app_only="${1:-0}" identities
  case "${MACOS_SIGNING_MODE:-developer-id}" in
    ad-hoc) return 0 ;;
    developer-id) ;;
    *) macos_distribution_error 'Signing mode must be developer-id or ad-hoc.'; return 1 ;;
  esac
  [[ "${MACOS_APP_SIGN_IDENTITY:-}" == 'Developer ID Application:'* ]] || {
    macos_distribution_error 'Set MACOS_APP_SIGN_IDENTITY to a Developer ID Application identity from Keychain.'; return 1;
  }
  [[ -n "${MACOS_NOTARY_PROFILE:-}" ]] || {
    macos_distribution_error 'Set MACOS_NOTARY_PROFILE to a notarytool Keychain profile.'; return 1;
  }
  identities="$(security find-identity -v -p codesigning)" || return 1
  [[ "$identities" == *"\"$MACOS_APP_SIGN_IDENTITY\""* ]] || {
    macos_distribution_error 'The Developer ID Application certificate and private key are not available.'; return 1;
  }
  if [[ "$app_only" != 1 ]]; then
    [[ "${MACOS_INSTALLER_SIGN_IDENTITY:-}" == 'Developer ID Installer:'* ]] || {
      macos_distribution_error 'Set MACOS_INSTALLER_SIGN_IDENTITY to a Developer ID Installer identity; Application certificates cannot sign a distributable PKG.'; return 1;
    }
    identities="$(security find-identity -v -p basic)" || return 1
    [[ "$identities" == *"\"$MACOS_INSTALLER_SIGN_IDENTITY\""* ]] || {
      macos_distribution_error 'The Developer ID Installer certificate and private key are not available.'; return 1;
    }
  fi
  # Validate the stored credentials before spending time on a build.
  xcrun notarytool history --keychain-profile "$MACOS_NOTARY_PROFILE" --output-format json >/dev/null
}
macos_sign_app() {
  local app="$1" details
  if [[ "${MACOS_SIGNING_MODE:-developer-id}" == ad-hoc ]]; then
    codesign --force --sign - "$app" || return 1
  else
    codesign --force --sign "$MACOS_APP_SIGN_IDENTITY" --options runtime --timestamp "$app" || return 1
    details="$(codesign --display --verbose=4 "$app" 2>&1)" || return 1
    [[ "$details" == *'Authority=Developer ID Application:'* && "$details" == *'runtime'* ]] || {
      macos_distribution_error 'App does not have Developer ID signing with hardened runtime.'; return 1;
    }
  fi
  codesign --verify --deep --strict --all-architectures "$app"
}
macos_notarize() {
  local artifact="$1" reports="$2" label upload report submission status
  [[ "${MACOS_SIGNING_MODE:-developer-id}" == developer-id ]] || {
    macos_distribution_error 'Ad-hoc builds cannot be notarized or published as verified releases.'; return 1;
  }
  mkdir -p "$reports" || return 1
  label="$(basename "$artifact")"; upload="$artifact"
  if [[ "$artifact" == *.app ]]; then
    upload="$reports/$label.zip"
    ditto -c -k --keepParent "$artifact" "$upload" || return 1
  fi
  report="$reports/$label.submission.json"
  xcrun notarytool submit "$upload" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait --timeout "${MACOS_NOTARY_TIMEOUT:-30m}" --output-format json >"$report" || {
    macos_distribution_error "Notarization did not complete. Keep $report and inspect its submission ID before retrying."; return 1;
  }
  status="$(python3 - "$report" <<'PY'
import json,sys
with open(sys.argv[1]) as stream: value=json.load(stream)
print(value.get('status',''))
PY
)" || return 1
  if [[ "$status" != Accepted ]]; then
    submission="$(python3 - "$report" <<'PY'
import json,sys
with open(sys.argv[1]) as stream: print(json.load(stream).get('id',''))
PY
)" || return 1
    if [[ -n "$submission" ]]; then
      xcrun notarytool log "$submission" --keychain-profile "$MACOS_NOTARY_PROFILE" "$reports/$label.log.json" || true
    fi
    macos_distribution_error "Apple did not accept $label (status: $status)."; return 1
  fi
  xcrun stapler staple "$artifact" || return 1
  xcrun stapler validate "$artifact" || return 1
  [[ "$upload" == "$artifact" ]] || rm -f "$upload"
}
macos_assess_app() { spctl --assess --verbose=2 --type execute "$1"; }
macos_assess_installers() {
  pkgutil --check-signature "$2" || return 1
  codesign --verify --strict "$1" || return 1
  xcrun stapler validate "$1" || return 1
  xcrun stapler validate "$2" || return 1
  spctl --assess --verbose=2 --type open --context context:primary-signature "$1" || return 1
  spctl --assess --verbose=2 --type install "$2"
}
