#!/usr/bin/env bash
# Stamp the package version from a release tag.
#
#   scripts/ci/stamp_version.sh <version>       # e.g. 0.1.5, 0.1.5-beta.2
#
# Rewrites `[package] version` in Cargo.toml. regexr is a single leaf crate with
# no internal path deps, so there is nothing else to pin.
#
# Only a prerelease tag ever changes anything: release-validate.yml has already
# proved the tag's base version matches Cargo.toml, so for `v0.1.5` this is a
# no-op and for `v0.1.5-beta.2` it appends the suffix. That makes re-running a
# release stage idempotent.

set -euo pipefail

VERSION="${1:?usage: stamp_version.sh <version>}"

CURRENT=$(cargo metadata --no-deps --format-version=1 \
    | jq -r '.packages[] | select(.name == "regexr") | .version')

if [[ "$VERSION" == "$CURRENT" ]]; then
    echo "Version already $VERSION — nothing to stamp."
    exit 0
fi

# First `version = "..."` at the start of a line is [package]; dependency
# versions in this manifest are all inline-table or same-line forms.
perl -i -pe 'if (!$done && /^version = "/) { s/^version = ".*"/version = "'"$VERSION"'"/; $done=1 }' Cargo.toml

echo "Stamped package version: $CURRENT -> $VERSION"
