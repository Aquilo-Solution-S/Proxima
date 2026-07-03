#!/usr/bin/env python3
"""Validate and dump the schema-id allocation ledger.

The registry permits one Rust payload type to register the same wire schema for
multiple payload kinds. This checker treats that as one allocation. A normalized
schema id reused by different Rust payload types is a collision.
"""
from __future__ import annotations

import argparse
import re
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PAYLOAD_TRAITS = {
    "FactPayload": "Fact",
    "AbstractionPayload": "Abstraction",
    "PerspectivePayload": "Perspective",
    "GoalPayload": "Goal",
    "EdgePayload": "Edge",
    "CitedObjectPayload": "CitedObject",
    "CitationMappingPayload": "CitationMapping",
}

IMPL_RE = re.compile(
    r"\bimpl\s+(?P<trait>(?:[\w:]+::)?"
    + "|".join(PAYLOAD_TRAITS)
    + r")\s+for\s+(?P<rust_type>[A-Za-z_][A-Za-z0-9_:<>]*)"
)
SCHEMA_ID_RE = re.compile(r"\bconst\s+SCHEMA_ID\s*:\s*&'?static\s+str\s*=\s*(?P<expr>[^;]+);")
SCHEMA_VERSION_RE = re.compile(r"\bconst\s+SCHEMA_VERSION\s*:\s*u32\s*=\s*(?P<expr>\d+);")
STRING_CONST_RE = re.compile(
    r"(?m)^\s*(?:pub\s+)?const\s+(?P<name>[A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*\"(?P<value>[^\"]+)\";"
)


@dataclass(frozen=True)
class Registration:
    path: Path
    line: int
    package: str
    trait: str
    kind: str
    rust_type: str
    schema_id: str
    schema_version: int

    @property
    def normalized_id(self) -> str:
        return normalize_id(self.schema_id)

    def location(self, root: Path) -> str:
        rel = self.path.relative_to(root) if self.path.is_relative_to(root) else self.path
        return f"{rel}:{self.line}"


def normalize_id(schema_id: str) -> str:
    return "".join(schema_id.split()).casefold()


def production_src_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for child in ("crates", "apps", "flavors", "examples"):
        base = root / child
        if not base.exists():
            continue
        files.extend(
            path
            for path in base.rglob("src/**/*.rs")
            if path.is_file() and "/target/" not in path.as_posix()
        )
    return sorted(files)


def package_name_for(path: Path) -> str:
    for parent in path.parents:
        manifest = parent / "Cargo.toml"
        if manifest.exists():
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
            package = data.get("package")
            if isinstance(package, dict) and isinstance(package.get("name"), str):
                return package["name"]
    raise ValueError(f"no package Cargo.toml found for {path}")


def mask_cfg_test_modules(text: str) -> str:
    masked = list(text)
    rx = re.compile(r"(?m)^\s*#\s*\[cfg\(test\)\]\s*\n\s*mod\s+\w+\s*\{")
    for match in list(rx.finditer(text)):
        start = match.start()
        brace = text.find("{", match.start(), match.end())
        if brace == -1:
            continue
        depth = 0
        end = len(text)
        for index in range(brace, len(text)):
            char = text[index]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    end = index + 1
                    break
        for index in range(start, end):
            if masked[index] != "\n":
                masked[index] = " "
    return "".join(masked)


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def brace_delta(line: str) -> int:
    return line.count("{") - line.count("}")


def trait_name(raw_trait: str) -> str:
    return raw_trait.rsplit("::", 1)[-1]


def resolve_schema_id(expr: str, package: str, aliases: dict[str, str]) -> str | None:
    expr = expr.strip()
    literal = re.fullmatch(r'"([^"]+)"', expr)
    if literal:
        return literal.group(1)
    macro = re.fullmatch(r'(?:[\w:]+::)?proxima_schema_id!\("([^"]+)"\)', expr)
    if macro:
        return f"{package}/{macro.group(1)}"
    return aliases.get(expr)


def scan_file(path: Path, root: Path) -> tuple[list[Registration], list[str]]:
    try:
        raw = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return [], []
    text = mask_cfg_test_modules(raw)
    package = package_name_for(path)
    aliases = {m.group("name"): m.group("value") for m in STRING_CONST_RE.finditer(text)}
    lines = text.splitlines()
    registrations: list[Registration] = []
    diagnostics: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = IMPL_RE.search(line)
        if match is None:
            index += 1
            continue

        start_index = index
        start_line = line_number(text, sum(len(l) + 1 for l in lines[:start_index]))
        depth = brace_delta(line)
        saw_open_brace = "{" in line
        body = [line]
        index += 1
        while index < len(lines) and (not saw_open_brace or depth > 0):
            current = lines[index]
            body.append(current)
            depth += brace_delta(current)
            if "{" in current:
                saw_open_brace = True
            index += 1

        body_text = "\n".join(body)
        id_match = SCHEMA_ID_RE.search(body_text)
        version_match = SCHEMA_VERSION_RE.search(body_text)
        rel = path.relative_to(root) if path.is_relative_to(root) else path
        trait = trait_name(match.group("trait"))
        rust_type = match.group("rust_type").rsplit("::", 1)[-1]
        if id_match is None:
            diagnostics.append(f"{rel}:{start_line}: {trait} for {rust_type}: missing SCHEMA_ID")
            continue
        if version_match is None:
            diagnostics.append(
                f"{rel}:{start_line}: {trait} for {rust_type}: missing SCHEMA_VERSION"
            )
            continue
        schema_id = resolve_schema_id(id_match.group("expr"), package, aliases)
        if schema_id is None:
            diagnostics.append(
                f"{rel}:{start_line}: {trait} for {rust_type}: unresolved SCHEMA_ID expression {id_match.group('expr').strip()!r}"
            )
            continue
        registrations.append(
            Registration(
                path=path,
                line=start_line,
                package=package,
                trait=trait,
                kind=PAYLOAD_TRAITS[trait],
                rust_type=rust_type,
                schema_id=schema_id,
                schema_version=int(version_match.group("expr")),
            )
        )
    return registrations, diagnostics


def collect(root: Path) -> tuple[list[Registration], list[str]]:
    registrations: list[Registration] = []
    diagnostics: list[str] = []
    for path in production_src_files(root):
        found, errors = scan_file(path, root)
        registrations.extend(found)
        diagnostics.extend(errors)
    return registrations, diagnostics


def validate(registrations: list[Registration], root: Path) -> list[str]:
    diagnostics: list[str] = []
    by_normalized: dict[str, list[Registration]] = {}
    for registration in registrations:
        by_normalized.setdefault(registration.normalized_id, []).append(registration)
        if registration.schema_id != registration.schema_id.strip():
            diagnostics.append(
                f"{registration.location(root)}: schema id has leading/trailing whitespace: {registration.schema_id!r}"
            )
        if any(char.isspace() for char in registration.schema_id):
            diagnostics.append(
                f"{registration.location(root)}: schema id contains whitespace: {registration.schema_id!r}"
            )
        if registration.schema_id != registration.schema_id.casefold():
            diagnostics.append(
                f"{registration.location(root)}: schema id must be lowercase: {registration.schema_id!r}"
            )

    for normalized_id, group in sorted(by_normalized.items()):
        rust_types = {registration.rust_type for registration in group}
        if len(rust_types) <= 1:
            continue
        locations = ", ".join(registration.location(root) for registration in group)
        display_ids = ", ".join(sorted({registration.schema_id for registration in group}))
        diagnostics.append(
            f"schema id collision {normalized_id!r}: {display_ids} used by {', '.join(sorted(rust_types))} at {locations}"
        )
    return diagnostics


def render_ledger(registrations: list[Registration], root: Path) -> str:
    groups: dict[tuple[str, str], list[Registration]] = {}
    for registration in registrations:
        groups.setdefault((registration.normalized_id, registration.rust_type), []).append(
            registration
        )
    rows = ["schema_id\tschema_versions\trust_type\tkinds\tlocations"]
    for _, group in sorted(groups.items(), key=lambda item: (item[1][0].schema_id, item[0][1])):
        schema_id = sorted({registration.schema_id for registration in group})[0]
        versions = ",".join(str(v) for v in sorted({r.schema_version for r in group}))
        kinds = ",".join(sorted({registration.kind for registration in group}))
        locations = ",".join(registration.location(root) for registration in group)
        rows.append(f"{schema_id}\t{versions}\t{group[0].rust_type}\t{kinds}\t{locations}")
    return "\n".join(rows)


def run(root: Path, print_ledger: bool) -> int:
    registrations, diagnostics = collect(root)
    diagnostics.extend(validate(registrations, root))
    if print_ledger:
        print(render_ledger(registrations, root))
    if diagnostics:
        print("schema-id check failed:", file=sys.stderr)
        for diagnostic in diagnostics:
            print(f"  {diagnostic}", file=sys.stderr)
        return 1
    print(
        f"schema-id ledger OK: {len({(r.normalized_id, r.rust_type) for r in registrations})} allocations, {len(registrations)} registrations"
    )
    return 0


def write_fixture(root: Path, body: str) -> None:
    crate = root / "crates" / "demo"
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "demo-flavor"\nversion = "0.0.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    (crate / "src" / "lib.rs").write_text(body, encoding="utf-8")


def self_test() -> int:
    ok_fixture = """
struct Shared;
impl FactPayload for Shared {
    const SCHEMA_ID: &'static str = proxima_schema_id!("shared-v1");
    const SCHEMA_VERSION: u32 = 1;
}
impl PerspectivePayload for Shared {
    const SCHEMA_ID: &'static str = proxima_schema_id!("shared-v1");
    const SCHEMA_VERSION: u32 = 1;
}
"""
    multiline_impl_fixture = """
struct Multiline;
impl FactPayload for Multiline
{
    const SCHEMA_ID: &'static str = proxima_schema_id!("multiline-v1");
    const SCHEMA_VERSION: u32 = 1;
}
"""
    collision_fixture = """
struct A;
impl FactPayload for A {
    const SCHEMA_ID: &'static str = "demo-flavor/collision-v1";
    const SCHEMA_VERSION: u32 = 1;
}
struct B;
impl FactPayload for B {
    const SCHEMA_ID: &'static str = "Demo-Flavor/ collision-v1 ";
    const SCHEMA_VERSION: u32 = 1;
}
"""
    unresolved_fixture = """
struct A;
impl FactPayload for A {
    const SCHEMA_ID: &'static str = UNKNOWN_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;
}
"""
    cases = [
        ("same payload type may register multiple kinds", ok_fixture, False),
        ("impl body may open on the next line", multiline_impl_fixture, False),
        ("different payload types may not collide by normalized id", collision_fixture, True),
        ("unresolved schema expression fails", unresolved_fixture, True),
    ]
    failures: list[str] = []
    for name, fixture, should_fail in cases:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_fixture(root, fixture)
            registrations, diagnostics = collect(root)
            diagnostics.extend(validate(registrations, root))
            failed = bool(diagnostics)
            if failed != should_fail:
                failures.append(f"{name}: expected fail={should_fail}, diagnostics={diagnostics}")
    if failures:
        print("schema-id self-test failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("schema-id self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true", help="run fixture checks")
    parser.add_argument("--print-ledger", action="store_true", help="print TSV allocation ledger")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    return run(args.root.resolve(), args.print_ledger)


if __name__ == "__main__":
    raise SystemExit(main())
