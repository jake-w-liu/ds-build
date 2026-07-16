#!/usr/bin/env bash
set -euo pipefail

# ── bump-and-install.sh ──────────────────────────────────────────────────────
# Bump the patch version of ds-pager-bin and ds-version, commit + push to
# origin/main, then build and install the binary to ~/.local/bin/ds.
#
# Usage:  ./bump-and-install.sh [--help]
#
# Guards:
#   - Must be on branch 'main'
#   - Working tree must be clean (no modified tracked files)
#   - origin/main must exist as a remote-tracking branch
#
# Idempotency:
#   If HEAD is already a version-bump commit (message starts with
#   "chore: bump to v") and the tree is clean, the script exits early
#   with a diagnostic.
#
# Flow: bump toml files → build (lockfile updates) → commit all → push → install
# ──────────────────────────────────────────────────────────────────────────────

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
VERSION_FILE1="crates/codegen/ds-version/Cargo.toml"
VERSION_FILE2="crates/codegen/ds-pager-bin/Cargo.toml"
INSTALL_PATH="$HOME/.local/bin/ds"

# ── helpers ──────────────────────────────────────────────────────────────────

die() {
    echo "ERROR: $*" >&2
    exit 1
}

info() {
    echo "→ $*"
}

# ── --help ───────────────────────────────────────────────────────────────────

if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
    cat <<'EOF'
bump-and-install.sh — bump patch version, push to main, build & install ds

Usage:
  ./bump-and-install.sh [--help]

What it does:
  1. Guards: verifies branch=main, clean tree, origin/main exists.
  2. Reads the current semver from ds-version/Cargo.toml.
  3. If HEAD is already a version-bump commit, exits early (idempotent).
  4. Increments the patch component, writes the new version to both
     ds-version/Cargo.toml and ds-pager-bin/Cargo.toml.
  5. Builds ds-pager-bin in release mode with DS_VERSION set (this also
     updates Cargo.lock with the new version metadata).
  6. Commits the version bump (both tomls + Cargo.lock) and pushes to
     origin/main.
  7. Installs the binary to ~/.local/bin/ds (codesigned on macOS).

Requirements:
  - cargo, git, install, codesign (macOS) on PATH
  - ~/.local/bin in PATH
EOF
    exit 0
fi

# ── guard: repo root ───────────────────────────────────────────────────────

cd "$REPO_ROOT"

# ── guard: branch ───────────────────────────────────────────────────────────

current_branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$current_branch" != "main" ]]; then
    die "must be on branch 'main' (currently on '$current_branch')"
fi

# ── guard: clean tree ──────────────────────────────────────────────────────

if ! git diff-index --quiet HEAD --; then
    die "working tree is dirty — commit or stash changes before bumping"
fi

# ── guard: origin/main exists ──────────────────────────────────────────────

if ! git rev-parse --verify origin/main >/dev/null 2>&1; then
    die "remote-tracking branch 'origin/main' not found — set up a remote first"
fi

# ── idempotency guard ──────────────────────────────────────────────────────

last_commit_msg="$(git log --oneline --format=%s -1)"
if [[ "$last_commit_msg" == chore:\ bump\ to\ v* ]]; then
    info "HEAD is already a version-bump commit — nothing to do."
    info "  last commit: $last_commit_msg"
    exit 0
fi

# ── parse current version ──────────────────────────────────────────────────

info "Reading current version from $VERSION_FILE1 ..."

# Extract the version string from TOML: version = "X.Y.Z"
current_version="$(grep -E '^version\s*=\s*"' "$VERSION_FILE1" | head -1 | sed 's/^version[[:space:]]*=[[:space:]]*"//;s/"//')"

if [[ -z "$current_version" ]]; then
    die "could not parse version from $VERSION_FILE1"
fi

# Strip any pre-release / build metadata suffix for the bump calculation.
# We only bump the MAJOR.MINOR.PATCH core.
base_version="${current_version%%[-+]*}"

# Parse MAJOR.MINOR.PATCH
IFS='.' read -r major minor patch <<< "$base_version"

if [[ -z "$major" || -z "$minor" || -z "$patch" ]]; then
    die "could not parse semver components from '$base_version' (full: '$current_version')"
fi

# Bump patch
new_patch=$((patch + 1))
new_version="${major}.${minor}.${new_patch}"

info "Current version: $current_version  →  new version: $new_version"

# ── write new version to both Cargo.toml files ──────────────────────────────

for f in "$VERSION_FILE1" "$VERSION_FILE2"; do
    info "Writing version $new_version to $f ..."
    # Replace the first occurrence of version = "..." with the new version.
    # Using a tmp file for portability; sed -i differs between macOS and Linux.
    tmp="$(mktemp)"
    sed "s/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/version = \"$new_version\"/" "$f" > "$tmp"
    mv "$tmp" "$f"
done

# ── verify the writes took effect ──────────────────────────────────────────

for f in "$VERSION_FILE1" "$VERSION_FILE2"; do
    written="$(grep -E '^version\s*=\s*"' "$f" | head -1 | sed 's/^version[[:space:]]*=[[:space:]]*"//;s/"//')"
    if [[ "$written" != "$new_version" ]]; then
        die "version write verification failed for $f: expected '$new_version', got '$written'"
    fi
done

# ── build (lockfile will be updated by cargo with the new versions) ────────

info "Building ds-pager-bin (release) with DS_VERSION=$new_version ..."
DS_VERSION="$new_version" cargo build -p ds-pager-bin --release

# ── commit + push (lockfile now reflects the bumped versions) ──────────────

info "Committing version bump (tomls + Cargo.lock) ..."
git add "$VERSION_FILE1" "$VERSION_FILE2" Cargo.lock
git commit -m "chore: bump to v$new_version"

info "Pushing to origin/main ..."
git push origin main

# ── install ────────────────────────────────────────────────────────────────

info "Installing to $INSTALL_PATH ..."
mkdir -p "$(dirname "$INSTALL_PATH")"
install -m 755 target/release/ds-pager "$INSTALL_PATH"

# macOS: ad-hoc codesign to avoid Gatekeeper SIGKILL
if [[ "$(uname -s)" == "Darwin" ]]; then
    info "Codesigning $INSTALL_PATH ..."
    codesign --force --sign - "$INSTALL_PATH"
fi

# ── verify installed binary reports the new version ────────────────────────

info "Verifying installed binary version ..."
reported_version="$("$INSTALL_PATH" --version 2>&1 || true)"
info "  $reported_version"

# ── report ─────────────────────────────────────────────────────────────────

echo ""
echo "✓ Done. ds bumped to v$new_version, pushed to origin/main, and installed."
echo "  Binary: $INSTALL_PATH"
echo "  Version: $reported_version"
