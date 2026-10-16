#!/usr/bin/env python3
"""Enforce the twelve public pages and their current-status markers.

Content correctness remains a review judgment; matching prose is not evidence
that an implementation claim is true.
"""
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PAGES = {
    "index", "concepts", "decisions", "get-started", "agents", "build-a-flavor",
    "host-integration", "protocol", "operate", "configuration", "data-and-erasure",
    "contributing",
}
expected = {f"docs/{page}.md" for page in PAGES}
inventory = subprocess.run(
    ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "docs"],
    cwd=ROOT, text=True, capture_output=True, check=True,
).stdout.splitlines()
actual = {name for name in inventory if name.endswith(".md") and (ROOT / name).is_file()}
failures = [f"missing public page: {name}" for name in sorted(expected - actual)]
failures += [f"unexpected public page: {name}" for name in sorted(actual - expected)]
for name in sorted(expected & actual):
    if not re.search(r"^> Status: current\.?$", (ROOT / name).read_text(), re.MULTILINE):
        failures.append(f"{name}: missing '> Status: current.' marker")
for failure in failures:
    print(failure, file=sys.stderr)
if not failures:
    print(f"public docs: {len(actual)} current pages")
sys.exit(bool(failures))
