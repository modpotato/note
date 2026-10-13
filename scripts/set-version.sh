#!/usr/bin/env bash
# Sets the package version in Cargo.toml and Cargo.lock: scripts/set-version.sh X.Y.Z
# Used by the release workflow, so the built exe and the release commit always agree.
set -euo pipefail
version="${1:?usage: set-version.sh X.Y.Z}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "not X.Y.Z: $version" >&2; exit 1; }
cd "$(dirname "$0")/.."

# Only the first "version = " line, which is the one in [package].
sed -i "0,/^version = /{s/^version = \"[^\"]*\"/version = \"${version}\"/}" Cargo.toml
# tr: Windows checkouts may use CRLF.
actual="$(grep -m1 '^version = ' Cargo.toml | tr -d '\r')"
[[ "$actual" == "version = \"${version}\"" ]] || { echo "Cargo.toml has '$actual'" >&2; exit 1; }

if [[ -f Cargo.lock ]]; then
  # The line after `name = "note"` in the lock file.
  sed -i "/^name = \"note\"\r\?$/{n;s/^version = \"[^\"]*\"/version = \"${version}\"/}" Cargo.lock
  locked="$(sed -n '/^name = "note"\r\?$/{n;p}' Cargo.lock | tr -d '\r')"
  [[ "$locked" == "version = \"${version}\"" ]] || { echo "Cargo.lock has '$locked'" >&2; exit 1; }
fi
echo "version set to ${version}"
