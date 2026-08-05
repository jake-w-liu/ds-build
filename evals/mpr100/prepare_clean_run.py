#!/usr/bin/env python3
"""Create a hash-pinned, answer-key-free MPR-100 development workspace and an
isolated, fail-closed launcher.

The generated `run.sh` launches `ds` with:
  --sandbox strict  --no-memory  --disable-web-search  --agent-profile <profile>
plus `DS_SANDBOX_FAIL_CLOSED=1` so the run REFUSES to start if the sandbox
could not actually be applied (unsupported platform / apply error) instead of
silently proceeding unsandboxed.

The run manifest records RUNTIME-OBSERVED values (ds binary version + baked
commit, profile path + hash, administered model, launch argv, sandbox/memory/
web-search policy) and is updated with the final artifact SHA-256 when the run
finishes, so a submission is reproducible end to end.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import secrets
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


EXPECTED_SHA256 = {
    "AGENT_INSTRUCTIONS.txt": (
        "c98027900b05dbeb8742fac15eee28d8bfcd550c8b624f12d798fd173fdbc82b"
    ),
    "mpr100_answer_sheet_development.tex": (
        "73de373fe2d74a626cc13836a3818c6e0241f82b8ae0296c62eea8e9cf3850d2"
    ),
    "mpr100_questions_development.pdf": (
        "6eb97bca2772bad52ef84eaff07affdd9329e59a273f75c9141145e3bb7cf0a1"
    ),
    "mpr100_questions_development.tex": (
        "444bbe95698361a7fb708fc41525f93feed8b55b16dc426a37fcf72ca8ab2d54"
    ),
}

# Default artifact file whose SHA-256 is appended to the manifest post-run.
ARTIFACT_NAME = "mpr100_answer_sheet_development.tex"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "source",
        type=Path,
        help="directory containing the four original development-set files",
    )
    parser.add_argument(
        "--runs-root",
        type=Path,
        default=Path.home() / "Downloads" / "mpr100_runs",
        help="parent for isolated runs (default: ~/Downloads/mpr100_runs)",
    )
    parser.add_argument(
        "--run-id",
        help="explicit run ID; default is a UTC timestamp plus a random suffix",
    )
    parser.add_argument(
        "--agent-profile",
        type=Path,
        required=True,
        help="path to the mpr-researcher.md agent profile (required: the /goal "
        "/structure command does NOT select a profile by itself)",
    )
    parser.add_argument(
        "--ds-bin",
        type=Path,
        default=Path("ds"),
        help="path to the ds binary (default: ds on PATH)",
    )
    parser.add_argument(
        "--model",
        default="deepseek-v4-pro",
        help="model administered for the run (recorded in the manifest)",
    )
    parser.add_argument(
        "--extra-ds-args",
        default="",
        help="extra ds CLI arguments appended to the launcher (quoted, shell-split)",
    )
    return parser.parse_args()


def probe_ds(binary: Path) -> dict[str, str]:
    """Record the ds binary's runtime-observed version + baked commit."""
    info: dict[str, str] = {}
    try:
        version = subprocess.run(
            [str(binary), "--version"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        ).stdout.strip()
        info["ds_version"] = version
    except Exception as err:  # noqa: BLE001 - record, don't fail the prepare
        info["ds_version"] = f"<unavailable: {err}>"
    try:
        j = subprocess.run(
            [str(binary), "version", "--json"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        ).stdout.strip()
        info["ds_version_json"] = j
    except Exception as err:  # noqa: BLE001
        info["ds_version_json"] = f"<unavailable: {err}>"
    return info


def main() -> int:
    args = parse_args()
    source = args.source.expanduser().resolve()
    if not source.is_dir():
        print(f"error: source is not a directory: {source}", file=sys.stderr)
        return 2

    profile = args.agent_profile.expanduser().resolve()
    if not profile.is_file():
        print(f"error: --agent-profile is not a file: {profile}", file=sys.stderr)
        return 2

    observed: dict[str, str] = {}
    for name, expected_hash in EXPECTED_SHA256.items():
        path = source / name
        if not path.is_file():
            print(f"error: missing benchmark input: {path}", file=sys.stderr)
            return 2
        actual_hash = sha256(path)
        if actual_hash != expected_hash:
            print(
                f"error: corpus hash mismatch for {name}: "
                f"expected {expected_hash}, got {actual_hash}",
                file=sys.stderr,
            )
            return 2
        observed[name] = actual_hash

    run_id = args.run_id
    if run_id is None:
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        run_id = f"{timestamp}-{secrets.token_hex(4)}"
    if not run_id or run_id in {".", ".."} or "/" in run_id or "\\" in run_id:
        print("error: run ID must be one safe path component", file=sys.stderr)
        return 2

    runs_root = args.runs_root.expanduser().resolve()
    run_root = runs_root / run_id
    workspace = run_root / "workspace"
    try:
        workspace.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        print(f"error: run already exists: {run_root}", file=sys.stderr)
        return 2

    for name in EXPECTED_SHA256:
        shutil.copy2(source / name, workspace / name)

    copied_names = sorted(path.name for path in workspace.iterdir())
    expected_names = sorted(EXPECTED_SHA256)
    if copied_names != expected_names:
        print(
            f"error: clean-workspace invariant failed: {copied_names}",
            file=sys.stderr,
        )
        return 2

    ds_info = probe_ds(args.ds_bin)
    manifest = {
        "run_id": run_id,
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "source": str(source),
        "workspace": str(workspace),
        "sha256": observed,
        "model": args.model,
        "agent_profile": str(profile),
        "agent_profile_sha256": sha256(profile),
        "ds_bin": str(args.ds_bin.expanduser().resolve()),
        "ds_version": ds_info.get("ds_version"),
        "ds_version_json": ds_info.get("ds_version_json"),
        "sandbox": "strict (fail-closed: DS_SANDBOX_FAIL_CLOSED=1)",
        "memory": "disabled (--no-memory)",
        "web_search": "disabled (--disable-web-search)",
        "administration": [
            "/headroom on",
            "/goal /structure start from AGENT_INSTRUCTIONS.txt and complete all.",
        ],
        "launch_argv": [
            str(args.ds_bin.expanduser().resolve()),
            "--cwd",
            str(workspace),
            "--sandbox",
            "strict",
            "--no-memory",
            "--disable-web-search",
            "--agent-profile",
            str(profile),
            "--model",
            args.model,
            *[t for t in args.extra_ds_args.split() if t],
        ],
        "artifact_sha256": None,  # filled by run.sh on completion
    }
    run_root.mkdir(parents=True, exist_ok=True)
    manifest_path = run_root / "run_manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    # Launcher: isolated, fail-closed, manifest-finalizing.
    artifact = workspace / ARTIFACT_NAME
    launcher = run_root / "run.sh"
    launcher.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
# Isolated MPR-100 run launcher — generated by prepare_clean_run.py.
# Refuses to start if the strict sandbox cannot be applied.
WORKSPACE={json.dumps(str(workspace))}
RUN_ROOT={json.dumps(str(run_root))}
ARTIFACT={json.dumps(str(artifact))}
export DS_SANDBOX_FAIL_CLOSED=1
cd "$WORKSPACE"
{json.dumps(str(args.ds_bin.expanduser().resolve()))} \\
  --cwd "$WORKSPACE" \\
  --sandbox strict \\
  --no-memory \\
  --disable-web-search \\
  --agent-profile {json.dumps(str(profile))} \\
  --model {json.dumps(args.model)} \\
  {' '.join(args.extra_ds_args.split())} \\
  "$@"
status=$?
if [ -f "$ARTIFACT" ]; then
  python3 - "$RUN_ROOT/run_manifest.json" "$ARTIFACT" <<'PY'
import hashlib, json, sys
manifest_path, artifact_path = sys.argv[1], sys.argv[2]
digest = hashlib.sha256(open(artifact_path, "rb").read()).hexdigest()
with open(manifest_path, encoding="utf-8") as f:
    manifest = json.load(f)
manifest["artifact_sha256"] = digest
manifest["finished_utc"] = __import__("datetime").datetime.now(
    __import__("datetime").timezone.utc
).isoformat()
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2, sort_keys=True)
    f.write("\\n")
print(f"artifact_sha256: {{digest}}")
PY
fi
exit $status
""",
        encoding="utf-8",
    )
    launcher.chmod(0o755)

    print(workspace)
    print(f"launcher: {launcher}")
    print(
        "NOTE: run the launcher (not a bare `ds` invocation) so the isolation "
        "flags, fail-closed sandbox, and manifest finalization are guaranteed.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
