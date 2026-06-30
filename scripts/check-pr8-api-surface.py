#!/usr/bin/env python3
"""PR8 API boundary checks."""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


failures: list[str] = []


def fail(path: str, message: str) -> None:
    failures.append(f"{path}: {message}")


def line_failures(path: str, pattern: re.Pattern[str], message: str) -> None:
    for lineno, line in enumerate(read(path).splitlines(), start=1):
        if pattern.search(line):
            fail(path, f"{lineno}: {message}: {line.strip()}")


root_forbidden = [
    "PgStorage",
    "PgPool",
    "ingest_fact_in_tx",
    "append_derived_with_edges_in_tx",
    "attach_citation_in_tx",
    "load_fact_text_in_tx",
]
for symbol in root_forbidden:
    line_failures(
        "crates/proxima/src/lib.rs",
        re.compile(rf"^\s*pub\s+use\b.*\b{re.escape(symbol)}\b"),
        f"root export of {symbol} is forbidden",
    )

flavor_sdk = "crates/proxima/src/flavor.rs"
for symbol in ["ingest_fact", "ingest_fact_with_citation_atomic"]:
    line_failures(
        flavor_sdk,
        re.compile(rf"^\s*pub\s+use\b.*\b{re.escape(symbol)}\b"),
        f"Flavor SDK raw PgPool write export of {symbol} is forbidden",
    )

code_lib = "flavors/code/src/lib.rs"
for module in ["ingest", "repos", "store"]:
    line_failures(
        code_lib,
        re.compile(rf"^\s*pub\s+mod\s+{module}\s*;"),
        f"code flavor raw {module} module must not be public SDK",
    )
for symbol in [
    "append_code_slice",
    "build_engine",
    "build_engine_with",
    "close_local_git_batch",
    "ingest_commit",
    "ingest_file_revision",
    "register_repo",
    "erase_repo",
    "start_run",
    "sweep_orphaned_runs",
    "update_cursor",
]:
    line_failures(
        code_lib,
        re.compile(rf"^\s*pub\s+use\b.*\b{re.escape(symbol)}\b"),
        f"code flavor root raw helper export of {symbol} is forbidden",
    )

for path in (ROOT / "flavors/code/src").rglob("*.rs"):
    rel = path.relative_to(ROOT).as_posix()
    text = path.read_text(encoding="utf-8")
    if "impl McpTool for" in text:
        fail(rel, "code flavor tools must implement transport-neutral Tool, not McpTool")
    for match in re.finditer(r"pub\s+fn\s+\w+\s*\([^)]*PgPool", text, re.S):
        lineno = text[: match.start()].count("\n") + 1
        window = "\n".join(text.splitlines()[max(0, lineno - 5) : lineno])
        if "host-api" not in window and "test" not in window and "debug_assertions" not in window:
            fail(rel, f"{lineno}: public code-flavor function exposes PgPool outside host/test cfg")

for rel in [
    "examples/embedded-minimal/Cargo.toml",
    "examples/embedded-minimal/src/main.rs",
    "examples/embedded-minimal/src/flavor.rs",
]:
    text = read(rel)
    for forbidden in ["proxima_core", "proxima-storage-pg", "proxima_storage_pg"]:
        if forbidden in text:
            fail(rel, f"embedded-minimal must use proxima root + proxima::flavor, not {forbidden}")

storage_pg = "crates/storage-pg/src/lib.rs"
storage_pg_lines = read(storage_pg).splitlines()
for i, line in enumerate(storage_pg_lines, start=1):
    if re.search(r"pub\s+fn\s+pool\s*\(\s*&self\s*\)\s*->\s*&PgPool", line):
        window = "\n".join(storage_pg_lines[max(0, i - 4) : i])
        if "test-fixtures" not in window and "#[cfg(test" not in window:
            fail(storage_pg, f"{i}: public PgStorage::pool outside test cfg")

line_failures(
    "crates/mcp-server/src/server.rs",
    re.compile(r"pub\s+fn\s+pool\s*\(\s*&self\s*\)"),
    "public McpToolHost::pool is forbidden",
)
line_failures(
    "crates/core/src/engine/builder.rs",
    re.compile(r"pub\s+fn\s+compose\s*\("),
    "public infallible Engine::compose is forbidden; use try_compose or compose_or_panic_for_tests",
)

sidecars = "crates/storage-pg/src/sidecars.rs"
sidecar_text = read(sidecars)
for match in re.finditer(r"(?:pub\s+)?fn\s+load_(?:batch|memory_payload)\s*\([^)]*&\s*PgPool", sidecar_text, re.S):
    lineno = sidecar_text[: match.start()].count("\n") + 1
    snippet = " ".join(sidecar_text[match.start() : match.end()].split())
    fail(sidecars, f"{lineno}: public PgMemoryPayload API exposes &PgPool: {snippet}")
for lineno, line in enumerate(sidecar_text.splitlines(), start=1):
    if "pub " in line and "&PgPool" in line and "From<&" not in line:
        fail(sidecars, f"{lineno}: public sidecar API exposes &PgPool: {line.strip()}")

raw_pool_patterns = [
    "ctx.extension::<PgPool>",
    "ctx.extension::<sqlx::PgPool>",
    "ctx.extensions.get::<sqlx::PgPool>",
    "McpToolExtensions::with(pool)",
    "McpToolExtensions::with(sqlx::PgPool)",
    "pg_pool(ctx)",
]
raw_pool_roots = [
    ROOT / "crates/mcp-server/src",
    ROOT / "crates/proxima/src",
    ROOT / "flavors/code/src",
]
for root in raw_pool_roots:
    for path in root.rglob("*.rs"):
        rel = path.relative_to(ROOT).as_posix()
        text = path.read_text(encoding="utf-8")
        for pattern in raw_pool_patterns:
            if pattern in text:
                fail(rel, f"raw PgPool tool-context pattern remains: {pattern}")

excluded_sql_parts = (
    "/tests/",
    "tools/dev-migrate/",
    "examples/",
)
for path in (ROOT / "flavors/code/src").rglob("*.rs"):
    rel = path.relative_to(ROOT).as_posix()
    if any(part in rel for part in excluded_sql_parts):
        continue
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if "proxima_core." in line:
            fail(rel, f"{lineno}: raw SQL references proxima_core.*: {line.strip()}")

if failures:
    print("PR8 API surface check failed:")
    for item in failures:
        print(f"- {item}")
    sys.exit(1)

print("PR8 API surface check passed")
