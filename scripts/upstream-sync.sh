#!/usr/bin/env bash
# ── upstream-sync.sh ─────────────────────────────────────────────────────────
# Selective port workflow: fetch xai-org/grok-build → review → classify →
# map paths → port by hand/agent → mark-reviewed.
#
# NEVER git merge/rebase upstream into main. Histories and brands diverge.
#
# Usage:
#   ./scripts/upstream-sync.sh setup
#   ./scripts/upstream-sync.sh fetch
#   ./scripts/upstream-sync.sh status
#   ./scripts/upstream-sync.sh review [--sha <sha>] [--open]
#   ./scripts/upstream-sync.sh map-path <upstream/path>
#   ./scripts/upstream-sync.sh show <upstream/path> [@sha]
#   ./scripts/upstream-sync.sh map-text < file.rs   # stdin → renamed text
#   ./scripts/upstream-sync.sh changed-files [from_sha] [to_sha]
#   ./scripts/upstream-sync.sh mark-reviewed <sha> [--ported N] [--skipped N] \
#       [--deferred N] [--note "..."]
#   ./scripts/upstream-sync.sh help
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

STATE_FILE="$REPO_ROOT/upstream/state.json"
LEDGER_FILE="$REPO_ROOT/upstream/LEDGER.md"
REVIEWS_DIR="$REPO_ROOT/upstream/reviews"
DEFAULT_URL="https://github.com/xai-org/grok-build.git"
DEFAULT_REMOTE="upstream"
DEFAULT_BRANCH="main"

die() { echo "ERROR: $*" >&2; exit 1; }
info() { echo "→ $*" >&2; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# ── state helpers ────────────────────────────────────────────────────────────

state_get() {
  local key="$1"
  python3 - "$STATE_FILE" "$key" <<'PY'
import json, sys
path, key = sys.argv[1], sys.argv[2]
with open(path) as f:
    data = json.load(f)
val = data.get(key)
print("" if val is None else val)
PY
}

state_set() {
  local key="$1" val="$2"
  python3 - "$STATE_FILE" "$key" "$val" <<'PY'
import json, sys
path, key, val = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    data = json.load(f)
if val == "" or val == "null":
    data[key] = None
else:
    data[key] = val
with open(path, "w") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY
}

remote_name() { state_get upstream_remote; [[ -n "$(state_get upstream_remote)" ]] || echo "$DEFAULT_REMOTE"; }
remote_url()  { local u; u="$(state_get upstream_url)"; echo "${u:-$DEFAULT_URL}"; }
remote_branch(){ local b; b="$(state_get upstream_branch)"; echo "${b:-$DEFAULT_BRANCH}"; }

# ── path / text mapping ──────────────────────────────────────────────────────

# Map an upstream repo-relative path to the DS path.
map_path() {
  local p="$1"
  # Special-case product rename first.
  p="${p//prod\/mc\/cli-chat-proxy-types/prod/mc/cli-proxy-types}"
  # Longest prefix first: xai-grok- before xai-
  p="${p//crates\/build\/xai-proto-build/crates/build/ds-proto-build}"
  p="${p//crates\/codegen\/xai-grok-/crates/codegen/ds-}"
  p="${p//crates\/common\/xai-grok-/crates/common/ds-}"
  p="${p//crates\/codegen\/xai-/crates/codegen/ds-}"
  p="${p//crates\/common\/xai-/crates/common/ds-}"
  printf '%s\n' "$p"
}

# Rename identifiers in a text stream (stdin → stdout).
# Order matters: longer / more specific tokens first.
map_text() {
  python3 - <<'PY'
import sys, re

text = sys.stdin.read()

# (pattern, replacement) — applied in order. Word-ish where needed.
subs = [
    (r"xai_grok_", "ds_"),
    (r"xai-grok-", "ds-"),
    (r"xai_grok", "ds"),
    (r"XaiGrok", "Ds"),
    (r"xai_proto_build", "ds_proto_build"),
    (r"xai-proto-build", "ds-proto-build"),
    (r"xai_", "ds_"),
    (r"xai-", "ds-"),
    (r"::xai::", "::ds::"),
    (r"\bxai::", "ds::"),
    (r"GROK_HOME", "DS_HOME"),
    (r"~/\.grok", "~/.ds"),
    (r"/\.grok/", "/.ds/"),
    (r'"\.grok"', '".ds"'),
    (r"'\.grok'", "'.ds'"),
]

for pat, rep in subs:
    text = re.sub(pat, rep, text)

sys.stdout.write(text)
PY
}

# Heuristic classification of a change bullet or file path.
classify_item() {
  local s
  s="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  # security first
  if [[ "$s" =~ (security|ssrf|sandbox|authz|injection|path.?traversal|secret|credential) ]]; then
    echo "PORT-HIGH"
    return
  fi
  # product / brand / phone-home skips
  if [[ "$s" =~ (oauth|enterprise.?stt|billing|mixpanel|sentry|phone-?home|telemetry.?phone|wss url|voice bearer|xai\.com|api\.x\.ai|branding|welcome.?logo) ]]; then
    echo "SKIP"
    return
  fi
  if [[ "$s" =~ (fix\(|bug|crash|hang|leak|race|deadlock|correctness|invariant|drain|no-wait|headless) ]]; then
    echo "PORT-HIGH"
    return
  fi
  if [[ "$s" =~ (refactor|split .* into|rename|docs? only|readme|changelog) ]]; then
    echo "DEFER"
    return
  fi
  echo "PORT-REVIEW"
}

# ── commands ─────────────────────────────────────────────────────────────────

cmd_help() {
  sed -n '2,30p' "$0" | sed 's/^# \?//'
}

cmd_setup() {
  need git
  need python3
  mkdir -p "$REVIEWS_DIR"
  local remote url
  remote="$(remote_name)"
  url="$(remote_url)"
  if git remote get-url "$remote" >/dev/null 2>&1; then
    info "remote '$remote' already exists: $(git remote get-url "$remote")"
    local cur
    cur="$(git remote get-url "$remote")"
    if [[ "$cur" != "$url" ]]; then
      info "updating URL → $url"
      git remote set-url "$remote" "$url"
    fi
  else
    info "adding remote $remote → $url"
    git remote add "$remote" "$url"
  fi
  [[ -f "$STATE_FILE" ]] || die "missing $STATE_FILE"
  info "setup OK. Next: ./scripts/upstream-sync.sh fetch && ./scripts/upstream-sync.sh status"
}

cmd_fetch() {
  need git
  local remote branch
  remote="$(remote_name)"
  branch="$(remote_branch)"
  git remote get-url "$remote" >/dev/null 2>&1 \
    || die "remote '$remote' missing — run: ./scripts/upstream-sync.sh setup"
  info "fetching $remote ($branch) ..."
  git fetch "$remote" "$branch" --tags
  local tip
  tip="$(git rev-parse --short "$remote/$branch")"
  info "upstream tip: $tip ($(git log -1 --format=%s "$remote/$branch"))"
}

cmd_status() {
  need git
  local remote branch last tip
  remote="$(remote_name)"
  branch="$(remote_branch)"
  git rev-parse --verify "$remote/$branch" >/dev/null 2>&1 \
    || die "no $remote/$branch — run fetch first"
  tip="$(git rev-parse "$remote/$branch")"
  last="$(state_get last_reviewed_sha)"

  echo "Upstream remote : $remote ($(git remote get-url "$remote"))"
  echo "Upstream branch : $branch"
  echo "Upstream tip    : $(git rev-parse --short "$tip")  $(git log -1 --format='%ci %s' "$tip")"
  if [[ -z "$last" ]]; then
    echo "Last reviewed   : (none — first review recommended)"
    echo "Pending         : FULL tip (no baseline)"
  else
    echo "Last reviewed   : $(git rev-parse --short "$last" 2>/dev/null || echo "$last")"
    if [[ "$last" == "$tip" ]]; then
      echo "Pending         : none (up to date with tip)"
    else
      echo "Pending commits :"
      git log --oneline "${last}..${tip}" | sed 's/^/  /' || true
      echo "Files touched (name-status):"
      git diff --name-status "${last}..${tip}" | head -80 | sed 's/^/  /'
      local n
      n="$(git diff --name-only "${last}..${tip}" | wc -l | tr -d ' ')"
      echo "  … total files: $n"
    fi
  fi
  echo "Last ported SHA : $(state_get last_ported_sha)"
  echo "Policy          : $(state_get policy)"
}

cmd_changed_files() {
  need git
  local remote branch from to
  remote="$(remote_name)"
  branch="$(remote_branch)"
  from="${1:-$(state_get last_reviewed_sha)}"
  to="${2:-$(git rev-parse "$remote/$branch")}"
  if [[ -z "$from" ]]; then
    # First review: files changed in tip commit vs its first parent (or all if root).
    if git rev-parse --verify "${to}^" >/dev/null 2>&1; then
      from="$(git rev-parse "${to}^")"
    else
      from="$(git hash-object -t tree /dev/null)"
    fi
  fi
  git diff --name-status "$from" "$to"
}

cmd_map_path() {
  [[ $# -ge 1 ]] || die "usage: map-path <upstream/path>"
  map_path "$1"
}

cmd_map_text() {
  map_text
}

cmd_show() {
  need git
  local path="${1:-}"
  local rev="${2:-}"
  [[ -n "$path" ]] || die "usage: show <upstream/path> [@sha|sha]"
  local remote branch
  remote="$(remote_name)"
  branch="$(remote_branch)"
  rev="${rev:-$remote/$branch}"
  rev="${rev#@}"
  git cat-file -e "${rev}:${path}" 2>/dev/null \
    || die "path not in $rev: $path"
  git show "${rev}:${path}" | map_text
}

cmd_review() {
  need git
  need python3
  local remote branch tip last sha open_flag=0
  remote="$(remote_name)"
  branch="$(remote_branch)"
  tip="$(git rev-parse "$remote/$branch" 2>/dev/null)" \
    || die "no $remote/$branch — run fetch first"
  last="$(state_get last_reviewed_sha)"
  sha="$tip"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --sha) sha="$2"; shift 2 ;;
      --open) open_flag=1; shift ;;
      *) die "unknown arg: $1" ;;
    esac
  done
  sha="$(git rev-parse "$sha")"

  local short from
  short="$(git rev-parse --short "$sha")"
  if [[ -n "$last" ]]; then
    from="$last"
  elif git rev-parse --verify "${sha}^" >/dev/null 2>&1; then
    from="$(git rev-parse "${sha}^")"
  else
    from="$(git hash-object -t tree /dev/null)"
  fi

  mkdir -p "$REVIEWS_DIR"
  local out="$REVIEWS_DIR/${short}.md"
  local date_utc
  date_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  info "writing review dossier → $out"
  info "range: $(git rev-parse --short "$from")..$short"

  {
    echo "# Upstream review: \`$short\`"
    echo
    echo "- Generated: \`$date_utc\`"
    echo "- Upstream range: \`$(git rev-parse --short "$from")\` → \`$short\`"
    echo "- Tip subject: $(git log -1 --format=%s "$sha")"
    echo "- URL: https://github.com/xai-org/grok-build/commit/$sha"
    echo
    echo "## Commits"
    echo
    echo '```'
    git log --oneline "${from}..${sha}" || git log -1 --oneline "$sha"
    echo '```'
    echo
    echo "## Upstream commit messages (full)"
    echo
    git log --format='### %h %s%n%n%b' "${from}..${sha}" || git log -1 --format='### %h %s%n%n%b' "$sha"
    echo
    echo "## Bullet triage (from commit bodies)"
    echo
    echo "| Class | Item |"
    echo "|-------|------|"
    # Extract "- foo" bullets from commit bodies in range
    git log --format='%b' "${from}..${sha}" 2>/dev/null \
      | grep -E '^[[:space:]]*[-*] ' \
      | sed 's/^[[:space:]]*[-*][[:space:]]*//' \
      | while IFS= read -r bullet; do
          [[ -z "$bullet" ]] && continue
          cls="$(classify_item "$bullet")"
          # escape pipes for markdown table
          safe="${bullet//|/\\|}"
          echo "| \`$cls\` | $safe |"
        done || true
    echo
    echo "## Changed files (mapped)"
    echo
    echo "| Status | Upstream path | DS path | Local exists | Heuristic |"
    echo "|--------|---------------|---------|--------------|-----------|"
    git diff --name-status "$from" "$sha" | while IFS=$'\t' read -r st upath rest; do
      # handle renames: status like R100, fields: old new
      if [[ "$st" == R* ]]; then
        upath_old="$upath"
        upath_new="$rest"
        dpath="$(map_path "$upath_new")"
        exists="no"
        [[ -e "$REPO_ROOT/$dpath" ]] && exists="yes"
        cls="$(classify_item "$upath_new")"
        echo "| \`$st\` | \`$upath_old\` → \`$upath_new\` | \`$dpath\` | $exists | \`$cls\` |"
      else
        dpath="$(map_path "$upath")"
        exists="no"
        [[ -e "$REPO_ROOT/$dpath" ]] && exists="yes"
        cls="$(classify_item "$upath $st")"
        echo "| \`$st\` | \`$upath\` | \`$dpath\` | $exists | \`$cls\` |"
      fi
    done
    echo
    echo "## Diffstat"
    echo
    echo '```'
    git diff --stat "$from" "$sha" | tail -40
    echo '```'
    echo
    echo "## Port checklist"
    echo
    echo "For each \`PORT-HIGH\` / \`PORT-REVIEW\` item:"
    echo
    echo "1. \`./scripts/upstream-sync.sh show <upstream/path> @$short > /tmp/up.rs\`"
    echo "2. Open mapped DS path; study intent (do not blind-overwrite)."
    echo "3. Port the *behavior* with DS names (\`ds_\`, \`DS_HOME\`, DeepSeek defaults)."
    echo "4. \`cargo build -p ds-pager-bin\` (and targeted tests)."
    echo "5. Record decision under **Decisions** below."
    echo
    echo "## Decisions"
    echo
    echo "| Item | Decision (port/skip/defer) | DS commit / notes |"
    echo "|------|----------------------------|-------------------|"
    echo "| | | |"
    echo
    echo "## After ports complete"
    echo
    echo '```bash'
    echo "./scripts/upstream-sync.sh mark-reviewed $short \\"
    echo "  --ported N --skipped N --deferred N \\"
    echo "  --note \"one-line summary\""
    echo '```'
  } > "$out"

  echo "Wrote $out"
  if [[ "$open_flag" -eq 1 ]]; then
    if command -v open >/dev/null 2>&1; then
      open "$out" 2>/dev/null || true
    fi
  fi
  # Also print a short console summary
  echo
  echo "=== triage summary ==="
  git log --format='%b' "${from}..${sha}" 2>/dev/null \
    | grep -E '^[[:space:]]*[-*] ' \
    | sed 's/^[[:space:]]*[-*][[:space:]]*//' \
    | while IFS= read -r bullet; do
        [[ -z "$bullet" ]] && continue
        printf '%-12s %s\n' "$(classify_item "$bullet")" "$bullet"
      done || true
}

cmd_mark_reviewed() {
  need python3
  local sha="" ported=0 skipped=0 deferred=0 note=""
  [[ $# -ge 1 ]] || die "usage: mark-reviewed <sha> [--ported N] [--skipped N] [--deferred N] [--note '...']"
  sha="$1"; shift
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --ported) ported="$2"; shift 2 ;;
      --skipped) skipped="$2"; shift 2 ;;
      --deferred) deferred="$2"; shift 2 ;;
      --note) note="$2"; shift 2 ;;
      *) die "unknown arg: $1" ;;
    esac
  done

  # Resolve short or long sha if we have the object
  if git cat-file -e "${sha}^{commit}" 2>/dev/null; then
    sha="$(git rev-parse "$sha")"
  fi
  local short date_utc
  short="$(echo "$sha" | cut -c1-7)"
  date_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  note="${note:-reviewed}"

  state_set last_reviewed_sha "$sha"
  state_set last_reviewed_at "$date_utc"
  if [[ "$ported" -gt 0 ]]; then
    state_set last_ported_sha "$sha"
    state_set last_ported_at "$date_utc"
  fi

  local review_link="reviews/${short}.md"
  [[ -f "$REVIEWS_DIR/${short}.md" ]] || review_link="—"

  # Replace placeholder row if present; else append as last table row.
  python3 - "$LEDGER_FILE" "$date_utc" "$short" "$note" "$ported" "$skipped" "$deferred" "$review_link" <<'PY'
import sys
path, date, short, note, ported, skipped, deferred, link = sys.argv[1:]
note = note.replace("|", "\\|")
row = f"| {date} | `{short}` | {note} | {ported} | {skipped} | {deferred} | {link} |"
with open(path) as f:
    text = f.read()
placeholder = "| _(none yet)_ | | | | | | |"
if placeholder in text:
    text = text.replace(placeholder, row, 1)
else:
    lines = text.splitlines(keepends=True)
    # Find the ledger table: first line starting with "| Reviewed"
    start = next((i for i, l in enumerate(lines) if l.startswith("| Reviewed")), None)
    if start is None:
        text = text.rstrip() + "\n\n" + row + "\n"
    else:
        # Walk rows until a non-table line; insert before that.
        end = start + 1
        while end < len(lines) and lines[end].startswith("|"):
            end += 1
        lines.insert(end, row + "\n")
        text = "".join(lines)
with open(path, "w") as f:
    f.write(text)
print(row)
PY

  info "marked reviewed: $short"
  info "state: last_reviewed_sha=$(state_get last_reviewed_sha)"
  info "ledger updated: $LEDGER_FILE"
}

# ── dispatch ─────────────────────────────────────────────────────────────────

cmd="${1:-help}"
shift || true
case "$cmd" in
  setup)          cmd_setup "$@" ;;
  fetch)          cmd_fetch "$@" ;;
  status)         cmd_status "$@" ;;
  review)         cmd_review "$@" ;;
  map-path)       cmd_map_path "$@" ;;
  map-text)       cmd_map_text "$@" ;;
  show)           cmd_show "$@" ;;
  changed-files)  cmd_changed_files "$@" ;;
  mark-reviewed)  cmd_mark_reviewed "$@" ;;
  help|-h|--help) cmd_help ;;
  *) die "unknown command: $cmd (try: help)" ;;
esac
