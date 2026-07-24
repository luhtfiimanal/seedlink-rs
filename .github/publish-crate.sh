#!/usr/bin/env bash
# Publish one workspace crate; succeed if this exact version is already on
# crates.io (idempotent re-runs), fail loudly on anything else.
set -uo pipefail
crate="$1"
out=$(cargo publish -p "$crate" 2>&1)
code=$?
echo "$out"
if [ $code -eq 0 ]; then
  exit 0
fi
if echo "$out" | grep -qE "already (exists|uploaded)|is already uploaded"; then
  echo "::notice::$crate: version already on crates.io, skipping"
  exit 0
fi
echo "::error::$crate: publish failed (see log above)"
exit $code
