# Phase 0 — Triage

The assessment is free and the honest answer is sometimes "no". Declining a
codebase you cannot improve costs one conversation. Accepting it costs a week and
ends with a broken product and a number you cannot defend.

Produce a go/no-go verdict with reasons before touching anything.

## What to look at

Spend the assessment reading, not editing. Aim for enough evidence to answer the
five questions below with citations.

```bash
python3 scripts/slopfix.py doctor
python3 scripts/slopfix.py baseline --target 0 --out /tmp/slopfix-triage.json
python3 scripts/slopfix.py census --top 15
python3 scripts/slopfix.py smells --severity blocking --top 40
```

Then read, by hand:

- the entry points (`main`, server bootstrap, route table, CLI definition);
- the two or three largest files, and the two or three most-imported files;
- the test suite: does it exist, does it run, does it assert behaviour or just
  that functions are callable;
- the build and CI configuration: can the project even be built and run;
- `git log --oneline | head -50` for how the code arrived.

## The five questions

**1. Is there a definable set of behaviours to preserve?**
If nobody can say what the application is supposed to do, there is no contract to
protect and no way to prove you did not break it. A product with a live user base
and no spec is still fine — the behaviour is whatever it currently does, and phase
2 writes it down. A half-finished prototype where half the endpoints were never
wired up is not: you would be inventing the spec, not preserving it.

**2. Does it build and run?**
You cannot verify behaviour preservation on something that does not execute. If
the build is broken, fixing the build is a prerequisite engagement, quoted
separately. Say so.

**3. Is the bloat actually redundancy?**
Run the census. A codebase that is large because it does a lot is not reducible,
and promising a percentage on it is dishonest. Look for the signature of slop:
the same concept implemented in many files, hand-rolled infrastructure, long
near-identical blocks. If the census finds little and the smells are sparse, the
honest verdict is "this is big, not bloated".

**4. Can the changes be verified?**
No tests plus no ability to run the app plus no staging environment means every
change is unverifiable. That is a stop. Tests can be *added* during the work —
that is normal and expected — but there must be some way to observe behaviour.

**5. Is the surface area sane for the time available?**
Three hundred endpoints in a week is not a reduction engagement, it is a triage
engagement. Either narrow the scope explicitly, or extend the time, or decline.

## Grounds to decline

State the reason plainly and stop. Do not soften it into a plan.

- No definable behaviour contract, and the user cannot supply one.
- Does not build or run, and fixing that is out of scope.
- The census shows the size is inherent, not redundant.
- No way to observe behaviour, so no change can be verified.
- The bloat is concentrated in generated code, vendored dependencies, or data
  files — none of which count, and removing them changes nothing real.
- The user wants the line count down but will not accept behaviour-preserving
  constraints, or wants features removed without deciding which.
- The scope cannot be covered in the time available and the user will not narrow
  it.

A partial decline is legitimate and often correct: "the API layer is reducible by
roughly 40%, the data pipeline is not, and here is the evidence for both."

## Setting the target

Only after a go verdict. Derive the target from the census, never from a round
number that sounds good.

1. Take the census estimate of removable duplicate lines.
2. Add the lines in modules you have concretely identified for library
   replacement — count them, do not estimate.
3. Add dead code you can actually prove is dead.
4. Subtract what you expect to *add*: new tests, the consolidated
   implementations, adapters at call sites.
5. Discount for risk. A codebase with a real test suite supports an aggressive
   target; one where you must write characterisation tests first supports much
   less.

Quote the result as a range with the evidence behind it, and record the number
you commit to. `slopfix baseline --target N` writes it into the manifest so the
final report scores against what was promised, not against what turned out to be
easy.

## Output

Write the verdict to `.slopfix/triage.md`:

- go / no-go / partial, and the reasons with file citations;
- measured baseline, with the counter identity;
- census summary: duplicate lines available, concepts duplicated, hand-rolled
  frameworks identified;
- blocking smells that must be fixed regardless of the reduction;
- verification capability: what tests exist, what can be run, what gaps must be
  filled first;
- the proposed target, its derivation, and its risk discount;
- explicit exclusions: what you are not touching and why.
