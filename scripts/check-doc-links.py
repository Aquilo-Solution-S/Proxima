#!/usr/bin/env python3
from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[1]
DOCS_ROOT = ROOT / "docs"
LINK_RE = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")


def slug(text: str) -> str:
    text = unquote(text)
    text = re.sub(r"<[^>]+>", "", text)
    text = text.strip().lower()
    text = re.sub(r"[`*_]", "", text)
    text = re.sub(r"[^a-z0-9\s-]", "", text)
    text = re.sub(r"\s+", "-", text)
    text = re.sub(r"-+", "-", text)
    return text.strip("-")


def anchors(path: Path) -> set[str]:
    found = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = HEADING_RE.match(line)
        if match:
            found.add(slug(match.group(2)))
    return found


def is_public_site_doc(path: Path) -> bool:
    try:
        relative = path.relative_to(DOCS_ROOT)
    except ValueError:
        return False
    return not relative.parts or relative.parts[0] != "superpowers"


def markdown_files() -> list[Path]:
    files = [
        ROOT / name
        for name in [
            "README.md",
            "CONTRIBUTING.md",
            "SECURITY.md",
            "CHANGELOG.md",
            "RELEASING.md",
        ]
        if (ROOT / name).exists()
    ]
    files.extend(sorted((ROOT / "docs").glob("**/*.md")))
    files.extend(sorted((ROOT / "apps").glob("*/README.md")))
    files.extend(sorted((ROOT / "crates").glob("*/README.md")))
    files.extend(sorted((ROOT / "flavors").glob("*/README.md")))
    return files


def is_ignored_scheme(target: str) -> bool:
    return target.startswith(("http://", "https://", "mailto:", "proxima://"))


def main() -> int:
    paths = [Path(arg) for arg in sys.argv[1:]] if len(sys.argv) > 1 else markdown_files()
    failures: list[str] = []
    cache: dict[Path, set[str]] = {}
    for raw_path in paths:
        path = (ROOT / raw_path).resolve() if not raw_path.is_absolute() else raw_path.resolve()
        if not path.exists():
            failures.append(f"{raw_path}: file does not exist")
            continue
        in_fence = False
        for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            stripped = line.strip()
            if stripped.startswith("```"):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            for target in LINK_RE.findall(line):
                if is_ignored_scheme(target):
                    continue
                clean = target.split()[0]
                dest, _, fragment = clean.partition("#")
                fragment = slug(fragment) if fragment else ""
                if not dest:
                    target_path = path
                else:
                    target_path = (path.parent / unquote(dest)).resolve()
                if is_public_site_doc(path):
                    try:
                        target_path.relative_to(DOCS_ROOT)
                    except ValueError:
                        failures.append(
                            f"{path.relative_to(ROOT)}:{lineno}: site doc link escapes docs_dir {target}; "
                            "use an absolute repository URL or an in-site page"
                        )
                        continue
                if not target_path.exists():
                    failures.append(f"{path.relative_to(ROOT)}:{lineno}: missing link target {target}")
                    continue
                if is_public_site_doc(path) and target_path.is_dir():
                    failures.append(
                        f"{path.relative_to(ROOT)}:{lineno}: site doc link target is a directory {target}; "
                        "link a Markdown page or use an absolute repository URL"
                    )
                    continue
                if fragment and target_path.suffix.lower() == ".md":
                    cache.setdefault(target_path, anchors(target_path))
                    if fragment not in cache[target_path]:
                        failures.append(
                            f"{path.relative_to(ROOT)}:{lineno}: missing anchor #{fragment} "
                            f"in {target_path.relative_to(ROOT)}"
                        )
    for failure in failures:
        print(failure)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
