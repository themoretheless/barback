#!/bin/sh
# Remove the temporary signing keychain. Runs with if: always() so a failed build
# never leaves a private key sitting on a runner, and exits 0 either way: this is
# cleanup, and failing it would mask the error that actually broke the job.

set -u

KEYCHAIN="${1:-}"
if [ -z "$KEYCHAIN" ]; then
  exit 0
fi

security list-keychains -d user -s login.keychain-db 2>/dev/null || true
security delete-keychain "$KEYCHAIN" 2>/dev/null || true
rm -f "${RUNNER_TEMP:-/tmp}/certificate.p12" "${RUNNER_TEMP:-/tmp}/AuthKey.p8"
exit 0
