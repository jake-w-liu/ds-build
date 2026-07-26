#!/usr/bin/env python3
"""Deterministically score MPR-100 structure and mathematical evidence."""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


IDS = (
    "M01", "M05", "M12", "M18", "M21", "M25", "M31", "M34", "M41", "M43",
    "P51", "P54", "P61", "P65", "P71", "P72", "P81", "P85", "P91", "P97",
)
SECTIONS = ("ASSUMPTIONS", "DERIVATION", "FINAL", "CHECKS", "TOOLS", "CONFIDENCE")
PLACEHOLDERS = (
    "replace this text",
    "replace with exactly one",
    "state one mathematically checkable claim",
    "give its justification and continue",
    "provide a dimensional, limiting",
    "none, or list the cas",
)


@dataclass(frozen=True)
class Criterion:
    label: str
    alternatives: tuple[tuple[str, ...], ...]


def criterion(label: str, *alternatives: Iterable[str]) -> Criterion:
    return Criterion(label, tuple(tuple(terms) for terms in alternatives))


def canonical(text: str) -> str:
    value = text.lower()
    replacements = (
        ("−", "-"), ("–", "-"), ("π", r"\pi"), ("θ", r"\theta"),
        ("ω", r"\omega"), ("κ", r"\kappa"), ("ε", r"\varepsilon"),
        ("ħ", r"\hbar"), ("ρ", r"\rho"), ("μ", r"\mu"),
        (r"\dfrac", r"\frac"), (r"\tfrac", r"\frac"),
        (r"\operatorname{sech}", r"\sech"), (r"\operatorname{lcm}", r"\lcm"),
        (r"\operatorname{rank}", r"\rank"), (r"\mathsf{t}", "t"),
        (r"\mathsf t", "t"), (r"\top", "t"), (r"\ln", r"\log"),
    )
    for old, new in replacements:
        value = value.replace(old, new)
    value = re.sub(r"\\(?:left|right|big|bigg|Big|Bigg)", "", value)
    value = re.sub(r"\\[!,;:]", "", value)
    value = re.sub(r"\s+", "", value)
    return value


FACTS: dict[str, tuple[Criterion, ...]] = {
    "M01": (
        criterion("determinant lemma", (r"\detb", r"\deta", r"1+\alpha", r"v^{t}a^{-1}u")),
        criterion("Sherman-Morrison inverse", ("b^{-1}", r"a^{-1}", r"\frac{\alpha", r"v^{t}a^{-1}", r"1+\alpha")),
        criterion("right null vector", ("rightnull", r"a^{-1}u"), ("kernel", r"a^{-1}u"), (r"\kerb", r"a^{-1}u")),
        criterion("left null vector", ("leftnull", r"a^{-t}v"), ("leftnull", r"a^{-\mathsf{t}}v"), (r"\kerb^{t}", r"a^{-t}v")),
    ),
    "M05": (
        criterion("state trajectory", ("x_1", r"k(e^{-t}-e^{-2t})", "x_2", r"e^{-2t}")),
        criterion("maximizing time", ("t_", r"\log2"), ("t=", r"\log2")),
        criterion("maximum magnitude", (r"\frac{|k|}{4}",), ("|k|/4",)),
        criterion("stable spectrum and transient", ("eigenvalue", "-1", "-2", "transient", "|k|")),
    ),
    "M12": (
        criterion("tangent substitution", (r"x=\tan\theta", r"\log(\sec^2\theta)")),
        criterion("log-cosine reduction", (r"\int_0^{\pi/2}", r"\log(\cos\theta)")),
        criterion("closed form", (r"\pi\log2",)),
        criterion("operation justification", ("dominated", "differentiat"), ("integrable", "parameter")),
    ),
    "M18": (
        criterion("exact solution", (r"\frac{1-e^{-x/\varepsilon}}{1-e^{-1/\varepsilon}}",)),
        criterion("outer and layer", ("outer", "y_0=1", "x=0", r"x/\varepsilon")),
        criterion("inner and composite", ("inner", r"1-e^{-x/\varepsilon}", "composite"), ("y=1-e^{-x/\varepsilon}", r"y_{\rmcomp}"), ("y=1-e^{-x}", r"y_{\rmcomp}")),
        criterion("exponential comparison", (r"o(e^{-1/\varepsilon})",), (r"o\!\left(e^{-1/\varepsilon}\right)",), ("exponentiallysmall",)),
    ),
    "M21": (
        criterion("biased hitting probability", (r"\frac{1-(q/p)^i}{1-(q/p)^n}",)),
        criterion("biased mean time", (r"\frac{nh_i-i}{p-q}",), (r"\frac{i-nh_i}{q-p}",), ("t_i=(nh_i-i)/(p-q)",)),
        criterion("fair hitting probability", ("p=q", r"h_i=\frac{i}{n}"), ("p=q", "h_i=i/n")),
        criterion("fair mean time", ("p=q", "t_i=i(n-i)")),
    ),
    "M25": (
        criterion("stationary distribution", (r"\frac1{43}(15,20,8)",), (r"(15/43,20/43,8/43)",)),
        criterion("detailed balance", (r"\pi_0p_{01}=\pi_1p_{10}", r"\pi_1p_{12}=\pi_2p_{21}"), (r"\pi_0/3=\pi_1/4", r"\pi_1/5=\pi_2/2")),
        criterion("mean returns", (r"\frac{43}{15}", r"\frac{43}{20}", r"\frac{43}{8}")),
        criterion("stationarity", (r"\pip=\pi",), ("stationary", "15:20:8")),
    ),
    "M31": (
        criterion("reducing substitution", ("z=y-x", "z'=z^2-2")),
        criterion("closed-form solution", (r"y=x-\sqrt2\tanh(\sqrt2x)",), (r"y(x)=x-\sqrt{2}\tanh(\sqrt{2}x)",), (r"x-\sqrt2\tanh(\sqrt2x)",)),
        criterion("initial condition", ("y(0)=0",)),
        criterion("direct residual", ("direct", "y^2-2xy+x^2-1"), ("residual", "=0")),
    ),
    "M34": (
        criterion("damped frequency", (r"\omega_d=\sqrt{\omega_0^2-\gamma^2}",)),
        criterion("causal Green function", (r"\theta(t)", r"\frac{e^{-\gamma t}\sin(\omega_dt)}{\omega_d}")),
        criterion("convolution solution", (r"\int_0^t", "g(t-s)", "f(s)")),
        criterion("jump normalization", ("g(0", "=0", "g'(0", "=1")),
    ),
    "M41": (
        criterion("closed count", (r"\frac{3^n-3}{4}", "n", "odd"), ("(3^n-3)/4", "odd")),
        criterion("even case", ("n", "even", "0")),
        criterion("generating function", (r"(\sinh z)^3", r"\frac{\sinh(3z)-3\sinh z}{4}"), (r"(\sinh z)^3", r"\sinh(3z)-3\sinh z", "/4")),
        criterion("automatic surjectivity", ("surject", "odd", "nonempty")),
    ),
    "M43": (
        criterion("solvability criterion", (r"\gcd(m,n)", "divides", "b-a"), (r"a\equivb\pmod", r"\gcd(m,n)"), (r"\gcd(m,n)\mid(b-a)",)),
        criterion("uniqueness modulus", (r"\lcm(m,n)",), (r"\frac{mn}{\gcd(m,n)}",)),
        criterion("applied class", (r"x\equiv185\pmod{420}",)),
        criterion("residue check", ("185", "84", "17", "60", "5")),
    ),
    "P51": (
        criterion("equation of motion", (r"\ddot\theta=\sin\theta", r"\omega^2\cos\theta-\frac ga"), (r"\ddot\theta=\sin\theta", r"\omega^2\cos\theta-g/a")),
        criterion("equilibria", (r"\theta=0", r"\theta=\pi", r"\cos\theta=\frac{g}{a\omega^2}"), (r"\theta=0", r"\theta=\pi", r"\cos\theta_")),
        criterion("vertical stability", ("bottom", "stable", r"\sqrt{\frac ga-\omega^2}", "top", "unstable"), ("bottom", "stable", r"\sqrt{g/a-\omega^2}", "top", "unstable")),
        criterion("off-axis stability", ("off", "stable", r"\omega|\sin\theta_",), ("nonvertical", "stable", r"\omega\sin\theta_")),
    ),
    "P54": (
        criterion("variable-mass equation", (r"m\dotv=\mu u-cv",)),
        criterion("dragged solution", (r"\frac{\mu u}{c}", r"1-(m/m_0)^{c/\mu}"), (r"\frac{\mu u}{c}", r"1-(\frac{m}{m_0})^{c/\mu}")),
        criterion("initial condition", ("v(m_0)=0",)),
        criterion("rocket limit", (r"u\log\frac{m_0}{m}", r"c\to0")),
    ),
    "P61": (
        criterion("axial potential", (r"\frac{\sigma}{2\varepsilon_0}", r"\sqrt{z^2+r^2}-z")),
        criterion("axial field", (r"\frac{\sigma}{2\varepsilon_0}", r"1-\frac{z}{\sqrt{z^2+r^2}}")),
        criterion("near-plane limit", (r"e_z\to\frac{\sigma}{2\varepsilon_0}",), (r"e_z\to\sigma/(2\varepsilon_0)",)),
        criterion("far-field point charge", (r"q=\pi r^2\sigma", r"\frac{q}{4\pi\varepsilon_0z}"), (r"q=\pi r^2\sigma", r"q/(4\pi\varepsilon_0z)")),
    ),
    "P65": (
        criterion("cutoff frequency", (r"\omega_c=\frac{\pi c", r"f_c=\frac{c", "2a"), (r"f_c=\frac{c_0}{2a}",)),
        criterion("propagation constant", (r"\beta=\sqrt{\frac{\omega^2}{c", r"-\left(\frac{\pi}{a}\right)^2}"), (r"\beta^2=\frac{\omega^2}{c", r"-\frac{\pi^2}{a^2}"), (r"\beta=\sqrt{\frac{\omega^2}{c_0^2}-\frac{\pi^2}{a^2}}",)),
        criterion("velocity product", (r"v_pv_g=c",)),
        criterion("propagating regime", (r"\omega>\omega_c", "real"), ("belowcutoff", "evanescent")),
    ),
    "P71": (
        criterion("wave numbers", (r"k=\frac{\sqrt{2m(e+v_0)}}{\hbar}", r"\kappa=\frac{\sqrt{-2me}}{\hbar}"), (r"k^2+\kappa^2=\frac{2mv_0}{\hbar^2}",)),
        criterion("parity equations", (r"k\tan(ka)=\kappa", r"-k\cot(ka)=\kappa")),
        criterion("odd-state threshold", (r"a\sqrt{2mv_0}/\hbar>\pi/2",), (r"v_0>\frac{\pi^2\hbar^2}{8ma^2}",)),
        criterion("even-state existence", ("even", "always", "v_0>0"), ("even", "any", "attractive"), ("even", "always", "exists")),
    ),
    "P72": (
        criterion("generalized Rabi frequency", (r"\omega_r=\sqrt{\delta^2+\omega^2}",), (r"\sqrt{\Delta^2+\Omega^2}",)),
        criterion("transition probability", (r"\frac{\omega^2}{\delta^2+\omega^2}", r"\sin^2",), (r"\frac{\Omega^2}{\Delta^2+\Omega^2}", r"\sin^2")),
        criterion("resonant result", (r"\Delta=0", r"\sin^2(\Omega t/2)")),
        criterion("pi pulse", (r"t_\pi=\frac{\pi}{|\Omega|}",), (r"t_\pi=\pi/\Omega",)),
    ),
    "P81": (
        criterion("internal energy", (r"u=\frac{n\varepsilon}{e^x+1}",), (r"u=n\varepsilon\frac{e^{-x}}{1+e^{-x}}",)),
        criterion("heat capacity", (r"c=nk_bx^2\frac{e^x}{(1+e^x)^2}",), (r"c=\frac{nk_bx^2}{4\cosh^2(x/2)}",)),
        criterion("maximum equation", (r"x\tanh(x/2)=2",)),
        criterion("maximum value", ("2.399357", r"t_{\max}=\frac{\varepsilon}{k_bx")),
    ),
    "P85": (
        criterion("fluctuation identity", (r"\langle", r"(\delta e)^2", r"k_bt^2c_v"), (r"\operatorname{var}(e)=k_bt^2c_v",)),
        criterion("partition derivatives", (r"\frac{\partial^2\logz}{\partial\beta^2}",), (r"-\frac{\partial u}{\partial\beta}",)),
        criterion("relative standard deviation", (r"\frac{\sigma_e}{\langlee\rangle}", r"n^{-1/2}")),
        criterion("relative variance", (r"\frac{\operatorname{var}(e)}{\langlee\rangle^2}", r"n^{-1}")),
    ),
    "P91": (
        criterion("proper-time worldline", (r"t(\tau)=\frac ca\sinh(a\tau/c)", r"x(\tau)=\frac{c^2}{a}[\cosh(a\tau/c)-1]")),
        criterion("velocity", (r"v(\tau)=c\tanh(a\tau/c)",)),
        criterion("hyperbola", (r"(x+c^2/a)^2-c^2t^2=(c^2/a)^2",)),
        criterion("nonrelativistic limit", (r"x\sim\frac12at^2", r"v\simat"), (r"x\simat^2/2", r"v\simat"), (r"x\simat^2/2", r"v\sima\tau", r"t\sim\tau")),
    ),
    "P97": (
        criterion("minimal Pi relation", (r"\frac{f}{\rho u^2r^2}=\phi", r"\mathrm{re}", r"\mathrm{ma}"), (r"c_d=\phi", r"\mathrm{re}", r"\mathrm{ma}")),
        criterion("dimensionless groups", (r"\mathrm{re}=\frac{\rho ur}{\mu}", r"\mathrm{ma}=\frac uc")),
        criterion("Stokes regime", (r"f=6\pi\mu ru",), (r"\frac{f}{\rho u^2r^2}=\frac{6\pi}{\mathrm{re}}",)),
        criterion("high-Re regime", ("high", r"\mathrm{re}", "incompressible", r"f\sim\rho u^2r^2"), (r"\mathrm{re}\gg1", r"\mathrm{ma}\ll1", r"f\sim\rho u^2r^2")),
    ),
}


BLOCK_RE = re.compile(
    r"%<MPR:BEGIN id=([A-Z]\d{2})>(.*?)%<MPR:END id=\1>",
    re.DOTALL,
)


def extract_section(block: str, name: str) -> str | None:
    match = re.search(
        rf"%<MPR:{name}>\s*(.*?)\s*%<MPR:{name}_END>",
        block,
        re.DOTALL,
    )
    return match.group(1) if match else None


def structure_failures(block: str) -> list[str]:
    failures: list[str] = []
    bodies = {name: extract_section(block, name) for name in SECTIONS}
    missing = [name for name, body in bodies.items() if body is None]
    if missing:
        failures.append("missing section markers: " + ", ".join(missing))
        return failures
    assert all(body is not None for body in bodies.values())
    if any(not body.strip() for body in bodies.values() if body is not None):
        failures.append("one or more required sections are empty")
    lowered = block.lower()
    if any(token in lowered for token in PLACEHOLDERS):
        failures.append("template placeholder remains")
    if block.count(r"\boxed") != 1:
        failures.append(f"expected exactly one boxed result, found {block.count(r'\\boxed')}")
    if bodies["DERIVATION"].count(r"\item") < 2:  # type: ignore[union-attr]
        failures.append("derivation has fewer than two numbered items")
    if r"\item" not in bodies["CHECKS"]:  # type: ignore[operator]
        failures.append("independent checks are not itemized")
    if "tools used:" not in bodies["TOOLS"].lower():  # type: ignore[union-attr]
        failures.append("tool disclosure is missing")
    confidence_match = re.search(
        r"Confidence:\}?\s*(?:\\\()?([01](?:\.\d+)?)(?:\\\))?",
        bodies["CONFIDENCE"],  # type: ignore[arg-type]
        re.IGNORECASE,
    )
    if confidence_match is None:
        failures.append("confidence is missing or not numeric")
    else:
        confidence = float(confidence_match.group(1))
        if not 0.0 <= confidence <= 1.0:
            failures.append("confidence is outside [0,1]")
    return failures


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("submission", type=Path)
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    parser.add_argument(
        "--require-at-least",
        type=float,
        metavar="SCORE",
        help="exit nonzero unless the score reaches this threshold",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.require_at_least is not None and not 0 <= args.require_at_least <= 100:
        print("error: --require-at-least must be in [0,100]", file=sys.stderr)
        return 2
    try:
        text = args.submission.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        print(f"error: cannot read {args.submission}: {exc}", file=sys.stderr)
        return 2

    matches = list(BLOCK_RE.finditer(text))
    observed_ids = tuple(match.group(1) for match in matches)
    global_failures: list[str] = []
    if observed_ids != IDS:
        global_failures.append(
            f"problem IDs/order mismatch: expected {list(IDS)}, got {list(observed_ids)}"
        )

    results = []
    total = 0.0
    blocks = {match.group(1): match.group(2) for match in matches}
    for problem_id in IDS:
        block = blocks.get(problem_id)
        if block is None:
            results.append(
                {
                    "id": problem_id,
                    "score": 0.0,
                    "structure_failures": ["solution block missing"],
                    "missing_evidence": [item.label for item in FACTS[problem_id]],
                }
            )
            continue

        failures = structure_failures(block)
        problem_score = 0.0 if failures else 1.0
        normalized = canonical(block)
        missing_evidence: list[str] = []
        for item in FACTS[problem_id]:
            matched = any(
                all(canonical(term) in normalized for term in alternative)
                for alternative in item.alternatives
            )
            if matched:
                problem_score += 1.0
            else:
                missing_evidence.append(item.label)
        total += problem_score
        results.append(
            {
                "id": problem_id,
                "score": problem_score,
                "structure_failures": failures,
                "missing_evidence": missing_evidence,
            }
        )

    report = {
        "submission": str(args.submission.resolve()),
        "score": total,
        "maximum": 100.0,
        "global_failures": global_failures,
        "problems": results,
    }
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"SCORE {total:.1f}/100.0")
        for failure in global_failures:
            print(f"GLOBAL FAIL: {failure}")
        for result in results:
            details = list(result["structure_failures"])
            details.extend(f"missing: {label}" for label in result["missing_evidence"])
            suffix = " | " + "; ".join(details) if details else ""
            print(f"{result['id']} {result['score']:.1f}/5.0{suffix}")

    if global_failures:
        return 1
    if args.require_at_least is not None and total < args.require_at_least:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
