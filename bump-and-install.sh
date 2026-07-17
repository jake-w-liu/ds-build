#!/usr/bin/env bash
set -euo pipefail

# ── bump-and-install.sh ──────────────────────────────────────────────────────
# Bump the product patch version, make *every* first-party crate match, rebuild
# from a clean version bake, push to origin/main, install both local binaries,
# and refuse to exit unless verification proves everything is up to date.
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
# Flow:
#   bump all first-party crate versions → force ds-version rebuild →
#   release build → commit + push → install both paths → verify
# ──────────────────────────────────────────────────────────────────────────────

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
# Canonical product version (CLI display reads this crate).
VERSION_FILE_CANON="crates/codegen/ds-version/Cargo.toml"
INSTALL_PATH="$HOME/.local/bin/ds"
INSTALL_PATH_ALT="$HOME/.ds/bin/ds"

# ── helpers ──────────────────────────────────────────────────────────────────

die() {
    echo "ERROR: $*" >&2
    exit 1
}

info() {
    echo "→ $*"
}

toml_version() {
    # First package-level version = "..." in a Cargo.toml
    grep -E '^version\s*=\s*"' "$1" | head -1 | sed 's/^version[[:space:]]*=[[:space:]]*"//;s/"//'
}

set_toml_version() {
    local f="$1"
    local ver="$2"
    local tmp
    tmp="$(mktemp)"
    # Only rewrite the package version line (first version = "..." at BOL).
    # Do not touch dependency version = "..." lines (they are indented or after [dependencies]).
    awk -v ver="$ver" '
        BEGIN { done = 0 }
        !done && /^version[[:space:]]*=[[:space:]]*"/ {
            print "version = \"" ver "\""
            done = 1
            next
        }
        { print }
    ' "$f" > "$tmp"
    mv "$tmp" "$f"
}

# First-party crates that must share the product version on every bump.
# Includes every package under crates/ and prod/ whose package name starts
# with ds- (plus the shipping binary aliases).
list_product_cargo_tomls() {
    find "$REPO_ROOT/crates" "$REPO_ROOT/prod" -name Cargo.toml -print 2>/dev/null \
        | sort \
        | while read -r f; do
            name="$(grep -E '^name\s*=' "$f" | head -1 | sed 's/^name[[:space:]]*=[[:space:]]*"//;s/"//')"
            case "$name" in
                ds-*|ptyctl|ptyctl-cli)
                    printf '%s\n' "$f"
                    ;;
            esac
        done
}

# ── --help ───────────────────────────────────────────────────────────────────

if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
    cat <<'EOF'
bump-and-install.sh — bump product version, sync every first-party crate,
                      rebuild, push main, install & verify

Usage:
  ./bump-and-install.sh [--help]

What it does:
  1. Guards: branch=main, clean tree, origin/main present.
  2. Reads product semver from crates/codegen/ds-version/Cargo.toml.
  3. Idempotent if HEAD is already "chore: bump to v*".
  4. Bumps patch and writes the same version into EVERY first-party
     ds-* (and ptyctl) Cargo.toml under crates/ and prod/.
  5. Release-builds with DS_VERSION set (refreshes ds-version git bake).
  6. Commits all version tomls + Cargo.lock; pushes origin/main.
  7. Installs to ~/.local/bin/ds and ~/.ds/bin/ds (codesigned on macOS).
  8. Verifies:
       - every product Cargo.toml reports the new version
       - both install paths exist and match each other (post-codesign)
       - `ds --version` and `ds version --json` both contain the new version
       - reported string starts with "ds <new_version>"

Requirements:
  - cargo, git, install, codesign (macOS), python3/jq optional
  - ~/.local/bin on PATH
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

# ── parse current product version ──────────────────────────────────────────

[[ -f "$VERSION_FILE_CANON" ]] || die "missing $VERSION_FILE_CANON"
info "Reading product version from $VERSION_FILE_CANON ..."
current_version="$(toml_version "$VERSION_FILE_CANON")"
[[ -n "$current_version" ]] || die "could not parse version from $VERSION_FILE_CANON"

base_version="${current_version%%[-+]*}"
IFS='.' read -r major minor patch <<< "$base_version"
[[ -n "$major" && -n "$minor" && -n "$patch" ]] \
    || die "could not parse semver components from '$base_version' (full: '$current_version')"

new_patch=$((patch + 1))
new_version="${major}.${minor}.${new_patch}"
info "Current product version: $current_version  →  new version: $new_version"

# ── collect + bump every first-party crate ─────────────────────────────────

# Bash 3.2-safe (macOS default): no mapfile.
PRODUCT_TOMLS=()
while IFS= read -r _toml; do
    PRODUCT_TOMLS+=("$_toml")
done < <(list_product_cargo_tomls)
[[ ${#PRODUCT_TOMLS[@]} -gt 0 ]] || die "no product Cargo.toml files found"

info "Bumping ${#PRODUCT_TOMLS[@]} first-party crate(s) to $new_version ..."
for f in "${PRODUCT_TOMLS[@]}"; do
    rel="${f#"$REPO_ROOT"/}"
    old="$(toml_version "$f")"
    set_toml_version "$f" "$new_version"
    written="$(toml_version "$f")"
    [[ "$written" == "$new_version" ]] \
        || die "version write failed for $rel: expected '$new_version', got '$written'"
    if [[ "$old" != "$new_version" ]]; then
        info "  $rel: $old → $new_version"
    else
        info "  $rel: already $new_version"
    fi
done

# ── force a clean version bake (commit hash + semver) ──────────────────────

info "Forcing ds-version rebuild so git SHA is current ..."
# Ensure build.rs re-runs even if sources look unchanged.
touch crates/codegen/ds-version/build.rs crates/codegen/ds-version/src/lib.rs
cargo clean -p ds-version 2>/dev/null || true

info "Building ds-pager-bin (release) with DS_VERSION=$new_version ..."
DS_VERSION="$new_version" cargo build -p ds-pager-bin --release

# ── commit + push ──────────────────────────────────────────────────────────

info "Committing version bump (all product tomls + Cargo.lock) ..."
# Stage every first-party Cargo.toml we rewrote + lockfile.
git add Cargo.lock
for f in "${PRODUCT_TOMLS[@]}"; do
    git add "$f"
done

# Also stage bump script itself if it changed in a prior WIP — not usually.
if ! git diff --cached --quiet; then
    git commit -m "chore: bump to v$new_version"
else
    die "nothing staged after version bump — unexpected"
fi

info "Pushing to origin/main ..."
git push origin main

# ── install both paths ─────────────────────────────────────────────────────

info "Installing to $INSTALL_PATH and $INSTALL_PATH_ALT ..."
mkdir -p "$(dirname "$INSTALL_PATH")" "$(dirname "$INSTALL_PATH_ALT")"
install -m 755 target/release/ds-pager "$INSTALL_PATH"
install -m 755 target/release/ds-pager "$INSTALL_PATH_ALT"

if [[ "$(uname -s)" == "Darwin" ]]; then
    info "Codesigning install paths ..."
    codesign --force --sign - "$INSTALL_PATH"
    codesign --force --sign - "$INSTALL_PATH_ALT"
fi

# ── verification (refuse to claim success unless everything matches) ───────

info "Verifying product Cargo.toml versions ..."
for f in "${PRODUCT_TOMLS[@]}"; do
    written="$(toml_version "$f")"
    [[ "$written" == "$new_version" ]] \
        || die "post-build version drift in ${f#"$REPO_ROOT"/}: expected $new_version, got $written"
done

info "Verifying install path hashes match ..."
if command -v shasum >/dev/null 2>&1; then
    h1="$(shasum -a 256 "$INSTALL_PATH" | awk '{print $1}')"
    h2="$(shasum -a 256 "$INSTALL_PATH_ALT" | awk '{print $1}')"
    [[ "$h1" == "$h2" ]] || die "install path hash mismatch: $INSTALL_PATH vs $INSTALL_PATH_ALT"
    info "  sha256=$h1"
else
    info "  shasum not available; skipping hash equality check"
fi

info "Verifying CLI version output ..."
reported_v="$("$INSTALL_PATH" --version 2>&1 | head -1 || true)"
reported_json="$("$INSTALL_PATH" version --json 2>&1 || true)"
info "  --version: $reported_v"
info "  version --json: $reported_json"

[[ "$reported_v" == *"$new_version"* ]] \
    || die "ds --version did not contain $new_version (got: $reported_v)"
[[ "$reported_json" == *"$new_version"* ]] \
    || die "ds version --json did not contain $new_version (got: $reported_json)"

# Must not still advertise the previous product version as the primary number.
if [[ "$reported_v" == *"$current_version"* && "$current_version" != "$new_version" ]]; then
    # Allow old version only if it appears as a substring of the new one (won't for patch bumps).
    if [[ "$reported_v" != ds\ "$new_version"* ]]; then
        die "ds --version still looks like old version (got: $reported_v)"
    fi
fi

# Working tree for tracked files should be clean after commit.
if ! git diff-index --quiet HEAD --; then
    die "tracked files dirty after bump commit — unexpected leftover changes"
fi

# Confirm remote has the bump.
local_sha="$(git rev-parse HEAD)"
remote_sha="$(git rev-parse origin/main)"
[[ "$local_sha" == "$remote_sha" ]] \
    || die "HEAD ($local_sha) != origin/main ($remote_sha) after push"

# ── report ─────────────────────────────────────────────────────────────────

echo ""
echo "✓ Done. Everything is up to date."
echo "  Version:     $new_version"
echo "  Commit:      $(git rev-parse --short HEAD)"
echo "  Crates:      ${#PRODUCT_TOMLS[@]} first-party packages"
echo "  Binary:      $INSTALL_PATH"
echo "  Binary (alt):$INSTALL_PATH_ALT"
echo "  ds --version: $reported_v"
echo "  ds version --json: $reported_json"
