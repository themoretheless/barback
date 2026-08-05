#!/bin/sh
# Notarize an already signed .app and staple the ticket into it.
#
# Order matters and is not obvious: notarization takes an archive, but the ticket
# is stapled to the .app, and only then is the app zipped for distribution. Zip
# the app for shipping first and you ship an unstapled copy, which makes the
# first launch require a working network connection to Apple.
#
# ditto rather than `zip`: zip flattens the symlinks and drops the extended
# attributes that carry the signature, and notarization rejects the result.
#
# Authenticates with an App Store Connect API key rather than an Apple ID and an
# app-specific password: it is not tied to one person's account and it does not
# break when that person's 2FA changes.
#
# Usage: tools/ci-notarize.sh path/to/Barback.app

set -eu

APP="${1:?usage: $0 path/to/App.app}"
: "${APPLE_API_KEY_P8:?APPLE_API_KEY_P8 is required}"
: "${APPLE_API_KEY_ID:?APPLE_API_KEY_ID is required}"
: "${APPLE_API_ISSUER_ID:?APPLE_API_ISSUER_ID is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

KEY="$RUNNER_TEMP/AuthKey.p8"
UPLOAD="$RUNNER_TEMP/notarize.zip"

printf '%s' "$APPLE_API_KEY_P8" | base64 --decode > "$KEY"
trap 'rm -f "$KEY"' EXIT

ditto -c -k --keepParent "$APP" "$UPLOAD"

# --wait blocks until Apple answers, which is normally a couple of minutes but
# has no contractual upper bound. A non-zero exit here means rejected, and the
# log is worth printing rather than guessing at.
if ! xcrun notarytool submit "$UPLOAD" \
  --key "$KEY" \
  --key-id "$APPLE_API_KEY_ID" \
  --issuer "$APPLE_API_ISSUER_ID" \
  --wait; then
  echo "notarization failed; fetching the log for the last submission" >&2
  xcrun notarytool history --key "$KEY" --key-id "$APPLE_API_KEY_ID" \
    --issuer "$APPLE_API_ISSUER_ID" >&2 || true
  exit 1
fi

xcrun stapler staple "$APP"
# Proves the ticket is actually in the bundle. Gatekeeper's own verdict, not
# codesign's, so it catches a valid signature that Apple still refuses.
spctl --assess --type execute --verbose=2 "$APP"

echo "notarized and stapled $APP"
