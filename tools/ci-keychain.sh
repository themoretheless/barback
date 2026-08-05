#!/bin/sh
# Load the Developer ID certificate into a throwaway keychain on a CI runner and
# report the identity to use for signing.
#
# A temporary keychain rather than the login one: the runner's login keychain is
# locked and its password is not ours to know, and anything we create here has to
# die with the job. tools/ci-keychain-clean.sh removes it.
#
# The keychain password is generated here instead of being another repository
# secret. It only has to outlive this job, and a secret that never leaves the
# runner is not a secret worth managing.
#
# Reads MACOS_CERT_P12 (base64 of the .p12) and MACOS_CERT_PASSWORD from the
# environment. Writes sign-id=<sha1> to GITHUB_OUTPUT: codesign accepts the
# fingerprint, which saves storing the human readable identity name as well.

set -eu

: "${MACOS_CERT_P12:?MACOS_CERT_P12 is required}"
: "${MACOS_CERT_PASSWORD:?MACOS_CERT_PASSWORD is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

KEYCHAIN="$RUNNER_TEMP/barback-signing.keychain-db"
CERT="$RUNNER_TEMP/certificate.p12"
PASSWORD="$(uuidgen)"

printf '%s' "$MACOS_CERT_P12" | base64 --decode > "$CERT"

security create-keychain -p "$PASSWORD" "$KEYCHAIN"
# Without this the keychain relocks on a timer mid-job and codesign starts
# failing with errSecInternalComponent.
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$PASSWORD" "$KEYCHAIN"
security import "$CERT" -k "$KEYCHAIN" -P "$MACOS_CERT_PASSWORD" \
  -T /usr/bin/codesign -T /usr/bin/security
# Otherwise the first codesign call raises a GUI keychain prompt that nobody is
# there to answer, and the job hangs until it times out.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PASSWORD" "$KEYCHAIN"
security list-keychains -d user -s "$KEYCHAIN"

rm -f "$CERT"

SIGN_ID="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk 'NR==1 {print $2}')"
if [ -z "$SIGN_ID" ]; then
  echo "no code signing identity in the imported certificate" >&2
  exit 1
fi

echo "sign-id=$SIGN_ID" >> "$GITHUB_OUTPUT"
echo "keychain=$KEYCHAIN" >> "$GITHUB_OUTPUT"
echo "imported signing identity $SIGN_ID"
