#!/bin/sh
# Assemble target/Barback.app around the cargo-built binary.
#
# Why this exists: TCC decides whether to hand out calendar access by looking at
# the caller's identity. For a bare executable that identity is the path plus the
# code signature, and cargo re-signs ad hoc on every build, so the cdhash changes
# and the previous grant stops matching. Inside a bundle macOS has a stable
# bundle identifier to key the entry on, and the user gets a real app name in the
# consent dialog instead of "barback".
#
# The binary also carries the plist in __TEXT,__info_plist (see build.rs), which
# is what makes a plain `cargo run` work during development. This script is for
# when you want the grant to stick.
#
# Usage: tools/make-app.sh [debug|release]

set -eu

PROFILE="${1:-release}"
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

# Ad hoc is enough to launch. For a grant that survives rebuilds, create a
# self-signed code signing certificate in Keychain Access once and replace the
# "-" below with its name: the designated requirement then keys on that identity
# rather than on a hash that changes every build.
codesign --force --sign - --timestamp=none "$APP"

echo "built $APP"
echo
echo "run it:      open \"$APP\""
echo "reset TCC:   tccutil reset Calendar com.barback.barback"
