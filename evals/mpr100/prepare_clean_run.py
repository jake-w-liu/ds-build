#!/usr/bin/env python3
"""Create a hash-pinned, answer-key-free MPR-100 development workspace."""

from __future__ import annotations

import argparse
import hashlib
import json
import secrets
import shutil
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
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    source = args.source.expanduser().resolve()
    if not source.is_dir():
        print(f"error: source is not a directory: {source}", file=sys.stderr)
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

    manifest = {
        "run_id": run_id,
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "source": str(source),
        "workspace": str(workspace),
        "sha256": observed,
        "model": "deepseek-v4-pro",
        "administration": [
            "/headroom on",
            "/goal /structure start from AGENT_INSTRUCTIONS.txt and complete all.",
        ],
    }
    run_root.mkdir(parents=True, exist_ok=True)
    (run_root / "run_manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    print(workspace)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

