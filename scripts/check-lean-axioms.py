#!/usr/bin/env python3
"""Pin the Causa kernel's declared axiom surface.

`docs/lean/Causa` is spec-mode Lean: every primitive is `axiom`/`inductive`,
and the kernel is deliberately axiom-heavy (the axiom-minimal target is a
long-term direction, not a v0.0.5 gate). What this check guards against is a
SILENT change to that axiom set — a new `axiom` slipped in without review, or
one quietly removed — landing without anyone noticing that the kernel's
trusted base moved.

Mechanism: this script first runs `lake build` in `docs/lean` (an
incremental no-op when the kernel is already built, so CI running it right
after its own kernel-build step pays nothing), then runs
`docs/lean/scripts/PrintAxioms.lean` via `lake env lean`. That Lean file
walks the built environment and prints every safe (non-`unsafe`) `axiom`
declared under the `Causa` namespace, sorted, one per line (see its header
for why `unsafe` compiler-internal stand-ins and Lean's own built-in axioms
are excluded). The output is diffed against the checked-in
`scripts/lean-axioms.allowlist.txt`. A non-empty diff in EITHER direction
(axiom added or removed) fails.

Building first is load-bearing, not a convenience: extraction reads BUILD
ARTIFACTS (.lake oleans), so on a stale tree a bare run would otherwise diff
yesterday's kernel and pass falsely. `--skip-build` exists as an escape
hatch for callers that provably just built (and for the second leg of
`--self-test`'s determinism probe), but the default stays build-first,
fail-closed.

Regenerate the allowlist after an intentional kernel change:

    python3 scripts/check-lean-axioms.py --write

(equivalent to `cd docs/lean && lake build && lake env lean
scripts/PrintAxioms.lean > ../../scripts/lean-axioms.allowlist.txt`, but goes
through this script's own build+extraction/normalization so the file it
writes is exactly what a bare check run will later compare against.)
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEAN_DIR = ROOT / "docs" / "lean"
PRINT_AXIOMS_SCRIPT = Path("scripts") / "PrintAxioms.lean"  # relative to LEAN_DIR
ALLOWLIST = ROOT / "scripts" / "lean-axioms.allowlist.txt"


def run_lake(args: list[str]) -> subprocess.CompletedProcess[str]:
    """Run a `lake` subcommand in `docs/lean`, converting spawn failures into
    a diagnosable `SystemExit`."""
    try:
        return subprocess.run(
            ["lake", *args],
            cwd=LEAN_DIR,
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError as err:
        raise SystemExit(
            f"could not run `lake {' '.join(args)}` ({err}); is elan/lake on PATH?"
        ) from err


def build_kernel() -> None:
    """Build the kernel so extraction reads CURRENT artifacts, not stale ones.

    Extraction reads build artifacts (`.lake` oleans); on a stale tree,
    skipping this step would diff yesterday's kernel against the allowlist
    and pass falsely. An incremental `lake build` on an already-built tree
    is a fast no-op, so always-build is the fail-closed default.
    """
    result = run_lake(["build"])
    if result.returncode != 0:
        raise SystemExit(
            "`lake build` failed in docs/lean — the kernel must compile before "
            "its axiom surface can be pinned:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    print("built docs/lean (lake build) before extraction", file=sys.stderr)


def extract_axioms() -> list[str]:
    """Run the Lean axiom printer and return the sorted, deduped name list.

    Callers must ensure the kernel is built first (`build_kernel()`); this
    reads the built environment. Raises `SystemExit` with a diagnosable
    message if the extraction itself fails.
    """
    if not (LEAN_DIR / PRINT_AXIOMS_SCRIPT).exists():
        raise SystemExit(f"missing axiom printer: {LEAN_DIR / PRINT_AXIOMS_SCRIPT}")
    result = run_lake(["env", "lean", str(PRINT_AXIOMS_SCRIPT)])
    if result.returncode != 0:
        raise SystemExit(
            "axiom extraction failed even though the kernel built — "
            "PrintAxioms.lean itself is likely broken:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    names = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not names:
        raise SystemExit(
            "axiom extraction returned zero axioms — this almost certainly means "
            "the extractor broke (e.g. the `Causa` import failed silently) rather "
            "than the kernel losing every axiom; refusing to treat that as a "
            "green diff against the allowlist"
        )
    return sorted(set(names))


def read_allowlist() -> list[str]:
    if not ALLOWLIST.exists():
        return []
    return sorted(
        line.strip()
        for line in ALLOWLIST.read_text(encoding="utf-8").splitlines()
        if line.strip()
    )


def write_allowlist(names: list[str]) -> None:
    ALLOWLIST.write_text("\n".join(names) + "\n", encoding="utf-8")


def run_self_test(skip_build: bool) -> int:
    """Assert the extractor is alive: two independent runs agree and are
    non-empty. This does not assert a specific axiom set (that would just be
    a copy of the allowlist check) — it guards against the extraction
    mechanism itself silently degenerating (e.g. returning nothing because a
    Lean/Lake upgrade changed `env.constants`' shape and the filter no longer
    matches anything). One build suffices for both extraction legs — the
    second leg deliberately re-runs only the extraction, which is the part
    whose determinism is under test.
    """
    if not skip_build:
        build_kernel()
    first = extract_axioms()
    second = extract_axioms()
    if first != second:
        print(
            "self-test failed: axiom extraction is non-deterministic between "
            f"runs: {first} != {second}",
            file=sys.stderr,
        )
        return 1
    print(f"self-test detected {len(first)} axioms: {', '.join(first)}", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="assert the axiom extractor produces a stable, non-empty result",
    )
    parser.add_argument(
        "--write",
        action="store_true",
        help="regenerate scripts/lean-axioms.allowlist.txt from the current kernel build",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help=(
            "skip the `lake build` step and extract from existing artifacts; "
            "only safe when the caller provably just built docs/lean — the "
            "default (build-first) is the fail-closed path"
        ),
    )
    args = parser.parse_args()

    if args.self_test:
        return run_self_test(args.skip_build)

    if not args.skip_build:
        build_kernel()
    current = extract_axioms()

    if args.write:
        write_allowlist(current)
        print(f"wrote {len(current)} axioms to {ALLOWLIST.relative_to(ROOT)}")
        return 0

    pinned = read_allowlist()
    if current == pinned:
        print(f"lean axiom set unchanged ({len(current)} axioms)")
        return 0

    added = sorted(set(current) - set(pinned))
    removed = sorted(set(pinned) - set(current))
    print("lean axiom set changed — update the allowlist if this is intentional:", file=sys.stderr)
    if added:
        print("  added:", file=sys.stderr)
        for name in added:
            print(f"    + {name}", file=sys.stderr)
    if removed:
        print("  removed:", file=sys.stderr)
        for name in removed:
            print(f"    - {name}", file=sys.stderr)
    print(
        f"regenerate with: python3 {Path(__file__).relative_to(ROOT)} --write",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
