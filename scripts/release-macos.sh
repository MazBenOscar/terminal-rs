#!/usr/bin/env bash
# Build a universal (Apple Silicon + Intel) Terminal.app, sign it with a Developer ID
# certificate and the hardened runtime, notarize and staple it, then package it as
# target/universal-apple-darwin/release/terminal-rs-universal.dmg, itself signed, notarized and
# stapled. That file name is the release asset the download page links to.
#
# Notarization credentials come from a notarytool keychain profile. Create it once with an
# app-specific password from account.apple.com and the team ID shown in the certificate name:
#
#   xcrun notarytool store-credentials terminal-rs-notary --apple-id you@example.com --team-id TEAMID
#
#   SIGN_IDENTITY    signing identity (default: the first "Developer ID Application" in the keychain)
#   NOTARY_PROFILE   notarytool keychain profile (default: terminal-rs-notary)
#   NOTARY_KEYCHAIN  keychain holding that profile (default: the login keychain)
#   NOTARIZE=0       sign and package without notarizing
set -euo pipefail
cd "$(dirname "$0")/.."

identity=${SIGN_IDENTITY:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1)}
[ -n "$identity" ] || { echo "no Developer ID Application certificate in the keychain; set SIGN_IDENTITY" >&2; exit 1; }
notarize=${NOTARIZE:-1}
notary=(--keychain-profile "${NOTARY_PROFILE:-terminal-rs-notary}")
[ -z "${NOTARY_KEYCHAIN:-}" ] || notary+=(--keychain "$NOTARY_KEYCHAIN")
if [ "$notarize" = 1 ] && ! xcrun notarytool history "${notary[@]}" >/dev/null; then
  echo "no notarytool credentials; create the profile described at the top of $0, or set NOTARIZE=0" >&2
  exit 1
fi

# Submits $1, waits for Apple's verdict, then staples the ticket to $2.
notarize() {
  local out id
  out=$(xcrun notarytool submit "$1" "${notary[@]}" --wait --output-format json) || true
  if [ "$(plutil -extract status raw -o - - <<<"$out" 2>/dev/null)" != Accepted ]; then
    echo "notarizing $1 failed: $out" >&2
    if id=$(plutil -extract id raw -o - - <<<"$out" 2>/dev/null); then
      xcrun notarytool log "$id" "${notary[@]}" >&2
    fi
    exit 1
  fi
  xcrun stapler staple "$2"
}

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked --target "$target" -p terminal-rs
done
out=target/universal-apple-darwin/release
mkdir -p "$out"
lipo -create -output "$out/terminal-rs" \
  target/aarch64-apple-darwin/release/terminal-rs target/x86_64-apple-darwin/release/terminal-rs
scripts/bundle-macos.sh "$out/terminal-rs"
app=$out/Terminal.app

echo "signing as $identity"
codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"
if [ "$notarize" = 1 ]; then
  ditto -c -k --keepParent "$app" "$out/Terminal.zip"
  notarize "$out/Terminal.zip" "$app"
  rm "$out/Terminal.zip"
fi

dmg=$out/terminal-rs-universal.dmg
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/Terminal.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Terminal -srcfolder "$stage" -format UDZO -ov "$dmg"
codesign --force --timestamp --sign "$identity" "$dmg"
if [ "$notarize" = 1 ]; then
  notarize "$dmg" "$dmg"
  spctl --assess --type execute --verbose=2 "$app"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi
echo "built $dmg"
