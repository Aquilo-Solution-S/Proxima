#!/usr/bin/env python3
from pathlib import Path
import sys

REQUIRED_SUBSTRINGS = {
    Path("AGENTS.md"): [
        "Current build/runtime-opt-in REST projection",
    ],
    Path("README.md"): [
        "## Authority",
        "The Lean kernel",
    ],
    Path("docs/12-tool-manifest.md"): [
        "> **Status:** current + deferred sections.",
        "Deferred rows are design intent, not implementation claims.",
    ],
    Path("docs/13-compliance.md"): [
        "> **Status:** current.",
        "Deferred rows are design intent, not implementation claims.",
    ],
    Path("docs/14-protocol-surface.md"): [
        "> **Status:** current + deferred sections.",
        "Deferred rows are design intent, not implementation claims.",
    ],
    Path("docs/15-deployment.md"): [
        "> **Status:** current + deferred sections.",
        "Deferred rows are design intent, not implementation claims.",
    ],
    Path("docs/17-rest-surface.md"): [
        "> **Status:** current.",
    ],
    Path("docs/reference/compliance-status.md"): [
        "# Compliance Status",
        "| Area | Status | Public claim |",
        "deferred",
        "not a current public guarantee",
    ],
}

failures = []
for path, expected_chunks in REQUIRED_SUBSTRINGS.items():
    if not path.exists():
        failures.append(f"{path}: missing required status-checked file")
        continue
    text = path.read_text(encoding="utf-8")
    for chunk in expected_chunks:
        if chunk not in text:
            failures.append(f"{path}: missing required status text: {chunk}")

if failures:
    print("\n".join(failures))
    sys.exit(1)
