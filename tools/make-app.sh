#!/bin/sh
# Assemble target/Barback.app around the cargo-built binary.
#
# Why this exists: TCC decides whether to hand out calendar access by looking at
# the caller's identity, and it wants a real app to name in the consent dialog.
# A bundle gives it one: the entry is keyed on com.barback.barback and the prompt
# says "Barback" rather than "barback".
#
# What this does NOT do by itself: make the grant survive a rebuild. The TCC row
# also stores a code signing requirement, and for an ad-hoc signature that
# requirement degrades to a plain cdhash, which changes on every build. To make
# the grant stick, create a self-signed code signing certificate once in Keychain
# Access (Certificate Assistant > Create a Certificate, type "Code Signing") and
# run this script with SIGN_ID set to its name. Then the requirement anchors on
# that identity instead of on a hash.
#
# The binary also carries the plist in __TEXT,__info_plist (see build.rs), which
# is what makes a plain `cargo run` work during development.
#
# Usage: [SIGN_ID="My Dev Cert"] tools/make-app.sh [debug|release]

set -eu

PROFILE="${1:-release}"
SIGN_ID="${SIGN_ID:--}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Barback.app"

case "$PROFILE" in
  debug)   cargo build --manifest-path "$ROOT/Cargo.toml" ;;
  release) cargo build --manifest-path "$ROOT/Cargo.toml" --release ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

BIN="$ROOT/target/$PROFILE/barback"
if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN" >&2
  exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$ROOT/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/barback"

codesign --force --sign "$SIGN_ID" --timestamp=none "$APP"

echo "built $APP"
echo
echo "run it:      open \"$APP\""
echo "reset TCC:   tccutil reset Calendar com.barback.barback"
if [ "$SIGN_ID" = "-" ]; then
  echo
  echo "note: signed ad hoc, so the calendar grant will not survive a rebuild."
  echo "      see the header of this script for the one-time fix."
fi
