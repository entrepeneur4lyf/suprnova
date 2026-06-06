#!/usr/bin/env bash
# Release-tagging script for Suprnova.
#
# Usage:
#   scripts/release.sh <new-version>
#
# Example:
#   scripts/release.sh 0.1.0
#
# What it does (in order):
#   1. Refuses to run with a dirty working tree.
#   2. Runs the full local gate — fmt --check, clippy -D warnings on the
#      workspace and both opt-in feature builds, the workspace test
#      suite, and rustdoc (verifying the warning count hasn't regressed
#      past the ci.yml RUSTDOC_BASELINE).
#   3. Bumps `workspace.package.version` in the root Cargo.toml.
#   4. Commits the bump.
#   5. Tags `v<new-version>`.
#   6. Pushes the commit and tag to `origin`.
#
# Under the current git-distribution model nothing is published to
# crates.io — the tag IS the release. See release-prep.md "Distribution
# model (corrected 2026-05-30)" and project_distribution_model.md.

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <new-version>" >&2
  echo "example: $0 0.1.0" >&2
  exit 64
fi

NEW_VERSION="$1"

# Loose semver shape — full validation is in cargo, this just blocks
# obvious typos before we spend gate time.
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?(\+[A-Za-z0-9.-]+)?$ ]]; then
  echo "error: '$NEW_VERSION' does not look like a semver version" >&2
  exit 64
fi

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# ---------- 1. Clean tree --------------------------------------------------

if ! git diff-index --quiet HEAD --; then
  echo "error: working tree is dirty — commit or stash first" >&2
  git status --short >&2
  exit 1
fi

# Verify we're on `main` so the tag points where it should.
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "$CURRENT_BRANCH" != "main" ]; then
  echo "error: release must be cut from main (currently on '$CURRENT_BRANCH')" >&2
  exit 1
fi

# Make sure the tag doesn't already exist.
if git rev-parse "v$NEW_VERSION" >/dev/null 2>&1; then
  echo "error: tag v$NEW_VERSION already exists" >&2
  exit 1
fi

# ---------- 2. Local gate --------------------------------------------------

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo clippy -p suprnova --features vector-pinecone -- -D warnings"
cargo clippy -p suprnova --features vector-pinecone -- -D warnings

echo "==> cargo clippy -p suprnova --features broadcasting-fanout -- -D warnings"
cargo clippy -p suprnova --features broadcasting-fanout -- -D warnings

echo "==> cargo test --workspace --no-fail-fast"
cargo test --workspace --no-fail-fast

echo "==> cargo doc -p suprnova --no-deps (baseline check)"
cargo doc -p suprnova --no-deps 2> /tmp/release_rustdoc.log
DOC_WARNINGS=$(grep -c "^warning:" /tmp/release_rustdoc.log || true)
DOC_WARNINGS=${DOC_WARNINGS:-0}
BASELINE=$(grep "RUSTDOC_BASELINE:" .github/workflows/ci.yml | head -n1 | sed -E 's/.*: *([0-9]+).*/\1/')
if [ -z "${BASELINE:-}" ]; then BASELINE=0; fi
echo "    rustdoc warnings: $DOC_WARNINGS (baseline: $BASELINE)"
if [ "$DOC_WARNINGS" -gt "$BASELINE" ]; then
  echo "error: rustdoc warnings regressed: $DOC_WARNINGS > $BASELINE" >&2
  echo "       ratchet RUSTDOC_BASELINE in ci.yml or fix the new warnings" >&2
  exit 1
fi

# ---------- 3. Bump workspace.package.version ------------------------------

CURRENT_VERSION=$(awk '
  /^\[workspace\.package\]/ { in_pkg = 1; next }
  /^\[/                     { in_pkg = 0 }
  in_pkg && /^version *=/   { gsub(/"/, "", $3); print $3; exit }
' Cargo.toml)

if [ -z "$CURRENT_VERSION" ]; then
  echo "error: could not find workspace.package.version in Cargo.toml" >&2
  exit 1
fi

if [ "$CURRENT_VERSION" = "$NEW_VERSION" ]; then
  echo "error: workspace version is already $NEW_VERSION" >&2
  exit 1
fi

echo "==> bumping workspace.package.version $CURRENT_VERSION -> $NEW_VERSION"

# Replace only the version line inside [workspace.package].
# Portable sed (BSD + GNU) requires the in-place flag form below.
python3 - "$NEW_VERSION" << 'PY'
import re
import sys
from pathlib import Path

new_version = sys.argv[1]
path = Path("Cargo.toml")
src = path.read_text()

pattern = re.compile(
    r"(\[workspace\.package\][^\[]*?\nversion\s*=\s*\")[^\"]+(\")",
    re.DOTALL,
)
new_src, n = pattern.subn(rf"\g<1>{new_version}\g<2>", src, count=1)
if n != 1:
    sys.exit("could not rewrite workspace.package.version")

path.write_text(new_src)
PY

# Sanity-check the new version landed.
BUMPED_VERSION=$(awk '
  /^\[workspace\.package\]/ { in_pkg = 1; next }
  /^\[/                     { in_pkg = 0 }
  in_pkg && /^version *=/   { gsub(/"/, "", $3); print $3; exit }
' Cargo.toml)

if [ "$BUMPED_VERSION" != "$NEW_VERSION" ]; then
  echo "error: version-bump verification failed (expected $NEW_VERSION, got $BUMPED_VERSION)" >&2
  exit 1
fi

# Refresh Cargo.lock so the commit is self-contained.
echo "==> cargo check --workspace (refresh Cargo.lock for the bumped version)"
cargo check --workspace

# ---------- 4 + 5 + 6. Commit, tag, push -----------------------------------

echo "==> committing release: v$NEW_VERSION"
git add Cargo.toml Cargo.lock
git commit -m "release: v$NEW_VERSION"

echo "==> tagging v$NEW_VERSION"
git tag -a "v$NEW_VERSION" -m "Suprnova v$NEW_VERSION"

echo "==> pushing main + tag"
git push origin main
git push origin "v$NEW_VERSION"

echo
echo "released v$NEW_VERSION"
echo "  commit: $(git rev-parse HEAD)"
echo "  tag:    v$NEW_VERSION"
echo
echo "next steps:"
echo "  - draft GitHub release notes from CHANGELOG.md section [$NEW_VERSION]"
echo "  - update manual/releases.md per its 'When v0.1.0 ships' plan"
