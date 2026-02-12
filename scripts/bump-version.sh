#!/usr/bin/env bash
set -euo pipefail

# Usage: ./scripts/bump-version.sh <new-version>
# Example: ./scripts/bump-version.sh 0.2.0
#
# Updates version in all workspace crates and the workspace dependency,
# runs cargo check, then commits and tags.

if [ $# -ne 1 ]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.2.0"
    exit 1
fi

NEW_VERSION="$1"

# Validate semver format (basic)
if ! echo "$NEW_VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$'; then
    echo "Error: '$NEW_VERSION' is not a valid semver version"
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "Bumping all crates to v${NEW_VERSION}..."

# 1. Update each crate's Cargo.toml version
for crate_dir in seedlink-protocol seedlink-client seedlink-server; do
    toml="$ROOT/$crate_dir/Cargo.toml"
    if [ -f "$toml" ]; then
        sed -i "s/^version = \".*\"/version = \"${NEW_VERSION}\"/" "$toml"
        echo "  Updated $crate_dir/Cargo.toml"
    fi
done

# 2. Update workspace dependency version
sed -i "s/seedlink-rs-protocol = { version = \"[^\"]*\"/seedlink-rs-protocol = { version = \"${NEW_VERSION}\"/" "$ROOT/Cargo.toml"
echo "  Updated workspace dependency in Cargo.toml"

# 3. Verify
echo ""
echo "Verifying..."
cargo check --workspace 2>&1
echo ""

# 4. Show diff
echo "Changes:"
git diff --stat
echo ""
git diff Cargo.toml */Cargo.toml

echo ""
echo "Done! Version bumped to ${NEW_VERSION}."
echo ""
echo "Next steps:"
echo "  git add -A && git commit -m 'chore: bump version to v${NEW_VERSION}'"
echo "  git tag v${NEW_VERSION}"
echo "  git push origin main v${NEW_VERSION}"
