#!/usr/bin/env python3
"""Regenerate XOR-encrypted template bytes for ds-agent/src/prompt/prompt_encrypted.rs."""

import os

def xor_encrypt(data: bytes, seed: int) -> bytes:
    return bytes((b ^ (seed + i) & 0xFF) for i, b in enumerate(data))

SEEDS = [0x5A, 0x7B, 0x3D]

TEMPLATES = [
    ("BASE_PROMPT_ENC", "templates/prompt.md"),
    ("CODEX_PROMPT_ENC", "templates/apply_patch_prompt.md"),
    ("SUBAGENT_PROMPT_ENC", "templates/subagent_prompt.md"),
]

BASE = "crates/codegen/ds-agent"

def format_bytes(data: bytes, indent: str = "    ") -> str:
    lines = []
    chunk = []
    for b in data:
        chunk.append(str(b))
        if len(chunk) >= 16:
            lines.append(indent + ", ".join(chunk) + ",")
            chunk.clear()
    if chunk:
        lines.append(indent + ", ".join(chunk) + ",")
    return "\n".join(lines)

def main():
    sizes = {}
    for var_name, path in TEMPLATES:
        with open(os.path.join(BASE, path), "rb") as f:
            raw = f.read()
            sizes[var_name] = len(raw)

    out = []
    out.append("// Auto-generated -- do not edit.")
    out.append("// Regenerate: python3 scripts/encrypt_templates.py")
    out.append("// XOR-encrypted prompt templates (key = position-dependent seed).")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append("pub(crate) const PROMPT_SEEDS: [u8; 3] = [0x5A, 0x7B, 0x3D];")
    out.append("")

    for i, (var_name, path) in enumerate(TEMPLATES):
        with open(os.path.join(BASE, path), "rb") as f:
            raw = f.read()
        encrypted = xor_encrypt(raw, SEEDS[i])
        out.append(f"#[rustfmt::skip]")
        out.append(f"pub(crate) const {var_name}: [u8; {len(encrypted)}] = [")
        out.append(format_bytes(encrypted))
        out.append("];")
        out.append("")

    output_path = os.path.join(BASE, "src/prompt/prompt_encrypted.rs")
    with open(output_path, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f"Wrote {output_path}")
    for var_name, path in TEMPLATES:
        print(f"  {var_name}: {sizes[var_name]} bytes → encrypted")

if __name__ == "__main__":
    main()
