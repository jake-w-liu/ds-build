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
#   - Working tree must be clean, except a mechanically verified interrupted
#     pre-commit bump (only product TOMLs/Cargo.lock at exactly HEAD patch + 1)
#   - origin/main must exist as a remote-tracking branch
#
# Recovery/idempotency:
#   If HEAD is already the matching version-bump commit, rerun the second bake,
#   push, install, codesign, and all verification gates. This repairs an earlier
#   interruption instead of falsely reporting success.
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
PSST_SKILL_SOURCE="$REPO_ROOT/crates/codegen/ds-shell/skills/psst-gpt"
PSST_SKILL_USER="$HOME/.ds/skills/psst-gpt"
PSST_SKILL_REPO="$REPO_ROOT/.ds/skills/psst-gpt"

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
    if grep -qE '^version[[:space:]]*=' "$f"; then
        # Only rewrite the package version line (first version = "..." at BOL).
        # Do not touch dependency version = "..." lines.
        awk -v ver="$ver" '
            BEGIN { done = 0 }
            !done && /^version[[:space:]]*=[[:space:]]*"/ {
                print "version = \"" ver "\""
                done = 1
                next
            }
            { print }
        ' "$f" > "$tmp"
    else
        # Package has no version field (rare path-only crates): insert after name=.
        awk -v ver="$ver" '
            BEGIN { done = 0 }
            !done && /^name[[:space:]]*=/ {
                print
                print "version = \"" ver "\""
                done = 1
                next
            }
            { print }
        ' "$f" > "$tmp"
    fi
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

sync_psst_skill() {
    local destination="$1"
    local source_file rel target mode
    [[ -d "$PSST_SKILL_SOURCE" ]] || die "missing bundled skill source: $PSST_SKILL_SOURCE"
    while IFS= read -r source_file; do
        rel="${source_file#"$PSST_SKILL_SOURCE"/}"
        target="$destination/$rel"
        mkdir -p "$(dirname "$target")"
        mode=644
        [[ "$source_file" == *.sh ]] && mode=755
        install -m "$mode" "$source_file" "$target"
    done < <(find "$PSST_SKILL_SOURCE" -type f -print | sort)
}

verify_psst_skill() {
    local destination="$1"
    local source_file rel target
    while IFS= read -r source_file; do
        rel="${source_file#"$PSST_SKILL_SOURCE"/}"
        target="$destination/$rel"
        [[ -f "$target" ]] || die "installed psst skill missing $target"
        cmp -s "$source_file" "$target" \
            || die "installed psst skill differs from source: $target"
    done < <(find "$PSST_SKILL_SOURCE" -type f -print | sort)
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
  3. Recovers safely if HEAD is already the matching "chore: bump to v*".
  4. Bumps patch and writes the same version into EVERY first-party
     ds-* (and ptyctl) Cargo.toml under crates/ and prod/.
  5. Release-builds with DS_VERSION set (refreshes Cargo.lock + version bake).
  6. Commits all version tomls + Cargo.lock (+ bump script if dirty).
  7. Rebuilds again so the baked git SHA matches the bump commit.
  8. Pushes origin/main; installs to ~/.local/bin/ds and ~/.ds/bin/ds
     (codesigned on macOS).
  9. Verifies:
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

# Record state now; after reading the product manifest set below, a narrowly
# defined interrupted pre-commit bump may resume. All other dirt is rejected.
tracked_dirty=false
if ! git diff-index --quiet HEAD --; then
    tracked_dirty=true
fi
untracked_files="$(git ls-files --others --exclude-standard)"

# ── guard: origin/main exists ──────────────────────────────────────────────

if ! git rev-parse --verify origin/main >/dev/null 2>&1; then
    die "remote-tracking branch 'origin/main' not found — set up a remote first"
fi

# ── recovery detection ─────────────────────────────────────────────────────

last_commit_msg="$(git log --oneline --format=%s -1)"
resume_existing_bump=false
if [[ "$last_commit_msg" == chore:\ bump\ to\ v* ]]; then
    resume_existing_bump=true
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

# Bash 3.2-safe (macOS default): no mapfile.
PRODUCT_TOMLS=()
while IFS= read -r _toml; do
    PRODUCT_TOMLS+=("$_toml")
done < <(list_product_cargo_tomls)
[[ ${#PRODUCT_TOMLS[@]} -gt 0 ]] || die "no product Cargo.toml files found"

resume_precommit_bump=false
if [[ "$resume_existing_bump" == true ]]; then
    [[ "$tracked_dirty" == false && -z "$untracked_files" ]] \
        || die "HEAD is a bump commit but the working tree is dirty — refusing ambiguous recovery"
    new_version="$current_version"
    expected_bump_msg="chore: bump to v$new_version"
    [[ "$last_commit_msg" == "$expected_bump_msg" ]] \
        || die "HEAD bump subject '$last_commit_msg' does not match canonical version '$new_version'"
    info "Resuming verification/install for existing bump: $new_version"
elif [[ "$tracked_dirty" == true ]]; then
    [[ -z "$untracked_files" ]] \
        || die "working tree has untracked files — commit/remove them before bumping: $untracked_files"
    head_version="$(git show "HEAD:$VERSION_FILE_CANON" | grep -E '^version\s*=\s*"' | head -1 | sed 's/^version[[:space:]]*=[[:space:]]*"//;s/"//')"
    head_base="${head_version%%[-+]*}"
    IFS='.' read -r head_major head_minor head_patch <<< "$head_base"
    [[ "$head_major" =~ ^[0-9]+$ && "$head_minor" =~ ^[0-9]+$ && "$head_patch" =~ ^[0-9]+$ ]] \
        || die "cannot verify interrupted bump against HEAD version '$head_version'"
    expected_interrupted_version="${head_major}.${head_minor}.$((head_patch + 1))"
    [[ "$current_version" == "$expected_interrupted_version" ]] \
        || die "dirty tree is not a verified interrupted patch bump: HEAD=$head_version current=$current_version"
    while IFS= read -r dirty_path; do
        [[ -n "$dirty_path" ]] || continue
        allowed=false
        [[ "$dirty_path" == "Cargo.lock" ]] && allowed=true
        for f in "${PRODUCT_TOMLS[@]}"; do
            if [[ "$dirty_path" == "${f#"$REPO_ROOT"/}" ]]; then
                allowed=true
                expected_manifest="$(mktemp)"
                git show "HEAD:$dirty_path" > "$expected_manifest"
                set_toml_version "$expected_manifest" "$current_version"
                if ! cmp -s "$f" "$expected_manifest"; then
                    unlink "$expected_manifest"
                    die "dirty manifest '$dirty_path' contains changes beyond the expected package-version rewrite"
                fi
                unlink "$expected_manifest"
                break
            fi
        done
        [[ "$allowed" == true ]] \
            || die "dirty path '$dirty_path' is outside a recoverable interrupted version bump"
    done < <(git diff --name-only HEAD)
    for f in "${PRODUCT_TOMLS[@]}"; do
        written="$(toml_version "$f")"
        [[ "$written" == "$current_version" ]] \
            || die "interrupted bump drift in ${f#"$REPO_ROOT"/}: expected $current_version, got $written"
    done
    resume_precommit_bump=true
    new_version="$current_version"
    info "Resuming interrupted pre-commit bump to $new_version"
elif [[ -n "$untracked_files" ]]; then
    die "working tree has untracked files — commit/remove them before bumping: $untracked_files"
else
    new_patch=$((patch + 1))
    new_version="${major}.${minor}.${new_patch}"
    info "Current product version: $current_version  →  new version: $new_version"
fi

# ── collect + bump every first-party crate ─────────────────────────────────

if [[ "$resume_existing_bump" == false && "$resume_precommit_bump" == false ]]; then
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
else
    info "Verifying the existing bump is lockstepped before recovery ..."
    for f in "${PRODUCT_TOMLS[@]}"; do
        written="$(toml_version "$f")"
        [[ "$written" == "$new_version" ]] \
            || die "existing bump drift in ${f#"$REPO_ROOT"/}: expected $new_version, got $written"
    done
fi

# ── first release build (refreshes Cargo.lock under the new versions) ─────

if [[ "$resume_existing_bump" == false ]]; then
    info "Forcing ds-version rebuild so git SHA is current ..."
    # Ensure build.rs re-runs even if sources look unchanged.
    touch crates/codegen/ds-version/build.rs crates/codegen/ds-version/src/lib.rs
    cargo clean -p ds-version 2>/dev/null || true

    info "Building ds-pager-bin (release) with DS_VERSION=$new_version ..."
    DS_VERSION="$new_version" cargo build -p ds-pager-bin --release

    # ── commit + push ──────────────────────────────────────────────────────

    info "Committing version bump (all product tomls + Cargo.lock) ..."
    # Stage every first-party Cargo.toml we rewrote + lockfile.
    git add Cargo.lock
    for f in "${PRODUCT_TOMLS[@]}"; do
        git add "$f"
    done
    # Include the bump script itself when it changed (so tooling ships with the bump).
    if [[ -f bump-and-install.sh ]] && ! git diff --quiet -- bump-and-install.sh 2>/dev/null; then
        git add bump-and-install.sh
    fi

    if ! git diff --cached --quiet; then
        git commit -m "chore: bump to v$new_version"
    else
        die "nothing staged after version bump — unexpected"
    fi
fi

# ── second bake: installed binary must carry the *bump commit* SHA ─────────
# The first build ran before the commit, so its baked short-SHA was the parent.
# Rebuild now so `ds --version` reports the commit users just pulled.

info "Rebuilding ds-pager-bin so version string matches bump commit $(git rev-parse --short HEAD) ..."
touch crates/codegen/ds-version/build.rs crates/codegen/ds-version/src/lib.rs
cargo clean -p ds-version 2>/dev/null || true
DS_VERSION="$new_version" cargo build -p ds-pager-bin --release

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
    codesign --verify --strict --verbose=2 "$INSTALL_PATH"
    codesign --verify --strict --verbose=2 "$INSTALL_PATH_ALT"
fi

info "Installing bundled psst-gpt skill to user scope ..."
sync_psst_skill "$PSST_SKILL_USER"
if [[ -d "$REPO_ROOT/.ds/skills" ]]; then
    info "Refreshing higher-priority repo-local psst-gpt skill shadow ..."
    sync_psst_skill "$PSST_SKILL_REPO"
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

info "Verifying CLI version output and baked HEAD SHA ..."
expected_short_sha="$(git rev-parse --short HEAD)"
expected_display="$new_version ($expected_short_sha)"
verify_cli_version() {
    local binary="$1"
    local label="$2"
    local version_output json_output json_current expected_cli
    if ! version_output="$("$binary" --version 2>&1)"; then
        die "$label --version exited nonzero: $version_output"
    fi
    if ! json_output="$("$binary" version --json 2>&1)"; then
        die "$label version --json exited nonzero: $json_output"
    fi
    expected_cli="ds $expected_display"
    [[ "$version_output" == "$expected_cli" \
        || "$version_output" == "$expected_cli [alpha]" \
        || "$version_output" == "$expected_cli [stable]" ]] \
        || die "$label --version mismatch: expected '$expected_cli' with an optional recognized channel label, got '$version_output'"
    json_current="$(printf '%s\n' "$json_output" | sed -n 's/.*"currentVersion"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
    [[ "$json_current" == "$expected_display" ]] \
        || die "$label JSON currentVersion mismatch: expected '$expected_display', got '$json_current' (raw: $json_output)"
    printf '%s\n%s\n' "$version_output" "$json_output"
}
version_probe="$(verify_cli_version "$INSTALL_PATH" "primary install")"
reported_v="$(printf '%s\n' "$version_probe" | sed -n '1p')"
reported_json="$(printf '%s\n' "$version_probe" | sed -n '2p')"
verify_cli_version "$INSTALL_PATH_ALT" "alternate install" >/dev/null
info "  --version: $reported_v"
info "  version --json: $reported_json"

info "Verifying installed psst-gpt skill content ..."
verify_psst_skill "$PSST_SKILL_USER"
if [[ -d "$REPO_ROOT/.ds/skills" ]]; then
    verify_psst_skill "$PSST_SKILL_REPO"
fi

# Tracked tree should match HEAD after commit. Refresh the index first so
# mtime-only noise from the rebuild doesn't trip a false dirty check;
# ignore untracked files (e.g. local scratch dirs).
git update-index --refresh -q >/dev/null 2>&1 || true
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    git status --porcelain --untracked-files=no >&2 || true
    die "tracked files dirty after bump commit — unexpected leftover changes"
fi

# Confirm remote has the bump.
local_sha="$(git rev-parse HEAD)"
remote_sha="$(git rev-parse origin/main)"
[[ "$local_sha" == "$remote_sha" ]] \
    || die "HEAD ($local_sha) != origin/main ($remote_sha) after push"
remote_live_sha="$(git ls-remote --exit-code origin refs/heads/main | awk 'NR == 1 { print $1 }')"
[[ "$local_sha" == "$remote_live_sha" ]] \
    || die "HEAD ($local_sha) != live origin/main ($remote_live_sha) after push"

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
