#!/bin/sh
# Assemble target/Barback.app around a cargo-built binary.
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
# that identity instead of on a hash. In CI the same knob takes a Developer ID.
#
# The binary also carries the plist in __TEXT,__info_plist (see build.rs), which
# is what makes a plain `cargo run` work during development.
#
# Usage: [SIGN_ID="My Dev Cert"] [BARBACK_BIN=path] tools/make-app.sh [debug|release]
#
# BARBACK_BIN skips the cargo build and bundles an existing binary instead, which
# is how the release workflow gets a universal one in here. Set BARBACK_BUILD to
# the same value that build used, or the embedded and bundle plists disagree.

set -eu

PROFILE="${1:-release}"
SIGN_ID="${SIGN_ID:--}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Barback.app"

# Single source of truth for the version. Only [package] has a line starting
# with "version =": dependency tables keep the key inside braces.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
if [ -z "$VERSION" ]; then
  echo "cannot read the package version out of Cargo.toml" >&2
  exit 1
fi
BUILD="${BARBACK_BUILD:-$VERSION}"

if [ -n "${BARBACK_BIN:-}" ]; then
  BIN="$BARBACK_BIN"
else
  # --bin barback: the crate also builds a calendar CLI, which has no business
  # in this bundle. Its dependencies still get compiled, since cargo resolves
  # those per package, but its own code and link step are skipped.
  case "$PROFILE" in
    debug)   cargo build --manifest-path "$ROOT/Cargo.toml" --bin barback ;;
    release) cargo build --manifest-path "$ROOT/Cargo.toml" --bin barback --release ;;
    *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
  esac
  BIN="$ROOT/target/$PROFILE/barback"
fi

if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN" >&2
  exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD/g" \
  "$ROOT/Info.plist.in" > "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/barback"

# The Mach-O carries its own copy of the plist, baked in when it was compiled.
# If that disagrees with the bundle copy, the version a caller sees depends on
# which of the two it happens to read, so refuse rather than ship both answers.
# Only reachable through BARBACK_BIN: a build done here shares this BUILD value.
EMBEDDED="$(otool -P "$APP/Contents/MacOS/barback" \
  | sed -n '/CFBundleVersion/{n;s/.*<string>\(.*\)<\/string>.*/\1/p;}' | head -1)"
if [ -n "$EMBEDDED" ] && [ "$EMBEDDED" != "$BUILD" ]; then
  echo "the binary was compiled with CFBundleVersion $EMBEDDED, the bundle says $BUILD" >&2
  echo "rebuild it with the same BARBACK_BUILD, or unset BARBACK_BUILD here" >&2
  exit 1
fi

if [ "$SIGN_ID" = "-" ]; then
  codesign --force --sign - --timestamp=none "$APP"
else
  # Hardened runtime and a trusted timestamp are both preconditions for
  # notarization, and neither can be bolted on afterwards.
  codesign --force --sign "$SIGN_ID" --options runtime --timestamp "$APP"
fi

codesign --verify --strict "$APP"

echo "built $APP ($VERSION, build $BUILD)"
echo
echo "run it:      open \"$APP\""
echo "reset TCC:   tccutil reset Calendar com.barback.barback"
if [ "$SIGN_ID" = "-" ]; then
  echo
  echo "note: signed ad hoc, so the calendar grant will not survive a rebuild."
  echo "      see the header of this script for the one-time fix."
fi
