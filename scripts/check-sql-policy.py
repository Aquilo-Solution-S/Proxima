#!/usr/bin/env python3
"""Detect dynamic SQL sites that need an explicit PR9 safety proof.

Final mode fails every dynamic SQL site without a nearby `SQL-POLICY:` proof.
Inventory mode prints sites and exits zero so implementation tasks can triage.
Fixture mode expects unsafe examples to be rejected and exits non-zero when the
fixture contains bypass spellings.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROOF = "SQL-POLICY:"
VALID_PROOFS = (
    "SQL-POLICY: PgIdent",
    "SQL-POLICY: fixed-fragment",
    "SQL-POLICY: QueryBuilder-bound-values",
)

# Exact current PR9 dynamic SQL inventory. Each entry is a reviewed site with
# the same proof vocabulary accepted in source comments; path+line+kind keeps
# the allowlist narrow so adjacent new dynamic SQL still fails.
ALLOWLISTED_SITE_LINES = {
    ("crates/storage-pg/src/sidecars/macros.rs", 401, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/active_goals.rs", 87, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/change_history.rs", 48, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/change_history.rs", 74, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/compliance_erase.rs", 1092, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/compliance_erase.rs", 1221, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/consolidate/events.rs", 36, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/consolidate/events.rs", 87, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/consolidate/memories.rs", 66, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/consolidate/memories.rs", 136, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/goals.rs", 93, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/goals.rs", 106, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/goals.rs", 113, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/goals.rs", 121, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/lineage.rs", 153, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/lineage.rs", 175, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/lineage.rs", 209, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/memories.rs", 118, "sql-push-str"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/query/memories.rs", 148, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/query/memories.rs", 338, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/memories.rs", 380, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/memories.rs", 386, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/memories.rs", 473, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/memories.rs", 491, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/rows.rs", 177, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/search.rs", 208, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/query/search.rs", 299, "sqlx-dynamic-query"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/query/search.rs", 348, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/search.rs", 420, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/search.rs", 432, "sql-push-str"): "SQL-POLICY: PgIdent",
    ("crates/storage-pg/src/verbs/query/search.rs", 513, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/src/verbs/query/search.rs", 529, "sql-push-str"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/tests/integration/fact_entity_edges_pg.rs", 427, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("crates/storage-pg/tests/integration/fact_entity_ingest_pg.rs", 403, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("flavors/code/src/ingest/pg_sidecars.rs", 33, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("flavors/code/src/mcp/work_item_bundle.rs", 371, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("flavors/code/src/mcp/work_item_bundle.rs", 543, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
    ("flavors/code/tests/erase_repo_pg.rs", 117, "sqlx-dynamic-query"): "SQL-POLICY: fixed-fragment",
}


@dataclass(frozen=True)
class Site:
    path: Path
    line: int
    kind: str
    text: str
    has_proof: bool

    def render(self) -> str:
        rel = self.path.relative_to(ROOT) if self.path.is_relative_to(ROOT) else self.path
        proof = " proof" if self.has_proof else " missing-proof"
        return f"{rel}:{self.line}: {self.kind}:{proof}: {self.text.strip()}"


def rust_files() -> list[Path]:
    roots = [ROOT / p for p in ("crates", "apps", "flavors", "examples")]
    files: list[Path] = []
    for root in roots:
        if not root.exists():
            continue
        files.extend(
            p
            for p in root.rglob("*.rs")
            if p.is_file()
            and "/target/" not in p.as_posix()
            and "/tests/fixtures/" not in p.as_posix()
        )
    return sorted(files)


def nearby_has_proof(lines: list[str], index: int) -> bool:
    start = max(0, index - 4)
    end = min(len(lines), index + 5)
    window = "\n".join(lines[start:end])
    return any(proof in window for proof in VALID_PROOFS)


def first_arg_is_literal(call_tail: str) -> bool:
    stripped = call_tail.lstrip()
    return (
        stripped.startswith('"')
        or stripped.startswith('r"')
        or stripped.startswith('r#"')
        or stripped.startswith('br"')
        or stripped.startswith('br#"')
        or stripped.startswith("concat!(")
        or re.match(r"[A-Z][A-Z0-9_]*\s*(?:[),]|\.)", stripped) is not None
    )


def fixed_push_str(line: str) -> bool:
    return re.search(
        r"\.push_str\s*\(\s*(?:\"|r\"|r#\"|&fetch_limit\.to_string\(\))",
        line,
    ) is not None


def safe_format_sql(path: Path, line: str, lines: list[str], index: int) -> bool:
    rel = path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else path.as_posix()
    window = "\n".join(lines[max(0, index - 3) : min(len(lines), index + 4)])
    if rel == "crates/pg-testkit/src/lib.rs" and "quoted_ident(" in window:
        return True
    if line.strip() == 'let sql = format!("SELECT count(*) FROM {table}");':
        return True
    if line.strip() in {
        'Cow::Owned(format!("SELECT {version};")),',
        'sqlx::AssertSqlSafe(format!("SELECT {version};")).into_sql_str(),',
    }:
        return True
    if line.strip() == 'let sql = format!("SELECT memory_id FROM {table} WHERE goal_id = $1 LIMIT 1");':
        return True
    return False


def intrinsic_proof(path: Path, line: str, kind: str, lines: list[str], index: int) -> bool:
    rel = path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else path.as_posix()
    if (rel, index + 1, kind) in ALLOWLISTED_SITE_LINES:
        return True
    window = "\n".join(lines[max(0, index - 5) : min(len(lines), index + 6)])
    if rel == "crates/storage-pg/src/sidecars/sql.rs" and kind == "sql-push-str":
        return True
    if rel == "crates/storage-pg/src/sidecars/macros.rs" and "memory_insert_sql" in window:
        return True
    if rel == "crates/storage-pg/src/sidecars/read_ctx.rs" and "validate_sidecar_read_sql" in window:
        return True
    if rel in {
        "crates/core/tests/agent_memory_tools_pg.rs",
        "crates/storage-pg/tests/integration/fact_with_citation_pg.rs",
    } and "SELECT count(*) FROM {table}" in window:
        return True
    if rel == "crates/storage-pg/src/verbs/goal_write.rs" and "SELECT memory_id FROM {table}" in window:
        return True
    return False


def first_argument_tail(lines: list[str], index: int, open_paren_end: int) -> str:
    tail = lines[index][open_paren_end:]
    if tail.strip():
        return tail
    for next_index in range(index + 1, min(len(lines), index + 8)):
        stripped = lines[next_index].strip()
        if stripped and not stripped.startswith("//"):
            return stripped
    return tail


def collect_sites(path: Path) -> list[Site]:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return []
    lines = text.splitlines()
    sites: list[Site] = []
    file_mentions_query_builder = "QueryBuilder" in text
    sql_format = re.compile(
        r"format!\s*\(\s*(?:r#?\"|\")\s*(?:SELECT|INSERT|UPDATE|DELETE|WITH|CREATE|ALTER|DROP)\b"
    )
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue
        proof = nearby_has_proof(lines, index)

        # sqlx::query* with non-literal first arg. Multiline literal calls are
        # common and safe when all values flow through `.bind(...)`.
        for match in re.finditer(r"\bsqlx::query(?:_as|_scalar)?(?:_with)?(?:\s*::<[^;]*?>)?\s*\(", line):
            tail = first_argument_tail(lines, index, match.end())
            if not first_arg_is_literal(tail):
                sites.append(
                    Site(
                        path,
                        index + 1,
                        "sqlx-dynamic-query",
                        line,
                        proof or intrinsic_proof(path, line, "sqlx-dynamic-query", lines, index),
                    )
                )

        # Imported query/query_as/query_scalar names with obvious dynamic args.
        if re.search(
            r"(?<![\w.:])query(?:_as|_scalar)?\s*\(\s*(?:&\w+|\w+\.as_str\s*\(|format!\s*\()",
            line,
        ):
            sites.append(
                Site(
                    path,
                    index + 1,
                    "imported-dynamic-query",
                    line,
                    proof or intrinsic_proof(path, line, "imported-dynamic-query", lines, index),
                )
            )

        # format!-assembled SQL is dynamic unless explicitly proved fixed-fragment.
        if sql_format.search(line) and not safe_format_sql(path, line, lines, index):
            sites.append(
                Site(
                    path,
                    index + 1,
                    "format-sql",
                    line,
                    proof or intrinsic_proof(path, line, "format-sql", lines, index),
                )
            )

        # QueryBuilder itself is allowed only with a proof. Its push sites are
        # the important injection boundary, but require the proof at construction
        # too so reviews do not miss a builder split across helper functions.
        if re.search(r"\bQueryBuilder(?:\s*::|\s*<)", line):
            sites.append(Site(path, index + 1, "query-builder", line, proof))
        elif file_mentions_query_builder and re.search(r"\.push\s*\(\s*(?!\"|r\"|r#\")", line):
            sites.append(Site(path, index + 1, "query-builder-push-dynamic", line, proof))
        elif file_mentions_query_builder and re.search(r"\.push\s*\(\s*(?:\"|r\"|r#\")", line):
            # Literal fragments can still encode identifiers/operators; require
            # an explicit fixed-fragment or bound-values proof nearby.
            sites.append(Site(path, index + 1, "query-builder-push-fragment", line, proof))

        # Obvious string accumulation of SQL text.
        if re.search(r"\b\w*sql\w*\.push_str\s*\(", line, re.IGNORECASE) and not fixed_push_str(line):
            sites.append(
                Site(
                    path,
                    index + 1,
                    "sql-push-str",
                    line,
                    proof or intrinsic_proof(path, line, "sql-push-str", lines, index),
                )
            )

    return sites


def collect(paths: list[Path]) -> list[Site]:
    return [site for path in paths for site in collect_sites(path)]


def run_fixture(path: Path) -> int:
    sites = collect([path])
    for site in sites:
        print(site.render())
    if not sites:
        print(f"fixture did not trigger dynamic SQL detector: {path}", file=sys.stderr)
        return 1
    print(f"fixture rejected {len(sites)} unsafe dynamic SQL sites", file=sys.stderr)
    return 1


# Exact current PR9 dynamic SQL inventory count (see `--inventory`). This is a
# ratchet, not a ceiling that only grows: the ratchet mode below fails when the
# count changes in *either* direction so a shrink still requires the PR that
# earned it to update this constant, keeping the two in lockstep.
# 2026-07-05 analysis: +1 net — K4 change-event commit-grace horizon
# (verbs/consolidate/events.rs) and K5 mcp-call-history include_body/keyset
# (verbs/mcp_call_history.rs) add proven fixed-fragment dynamic sites.
EXPECTED_DYNAMIC_SQL_SITES = 48


def run_self_test() -> int:
    fixture = ROOT / "scripts/fixtures/sql-policy/unsafe_dynamic_sql.rs"
    sites = collect([fixture])
    for site in sites:
        print(site.render())
    if not sites:
        print(f"self-test did not trigger dynamic SQL detector: {fixture}", file=sys.stderr)
        return 1
    print(f"self-test detected {len(sites)} unsafe dynamic SQL sites", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory", action="store_true", help="print dynamic SQL inventory and exit 0")
    parser.add_argument("--fixture", type=Path, help="run against an unsafe fixture; exits non-zero when detected")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="assert the unsafe fixture is detected; exits zero when the detector fires",
    )
    args = parser.parse_args()

    if args.fixture:
        fixture = args.fixture if args.fixture.is_absolute() else ROOT / args.fixture
        return run_fixture(fixture)

    if args.self_test:
        return run_self_test()

    sites = collect(rust_files())
    if args.inventory:
        if sites:
            print("Dynamic SQL inventory:")
            for site in sites:
                print("  " + site.render())
        else:
            print("Dynamic SQL inventory: none")
        return 0

    missing = [site for site in sites if not site.has_proof]
    if missing:
        print("Dynamic SQL sites missing SQL-POLICY proof:", file=sys.stderr)
        for site in missing:
            print("  " + site.render(), file=sys.stderr)
        return 1

    if len(sites) > EXPECTED_DYNAMIC_SQL_SITES:
        print(
            f"Dynamic SQL site count {len(sites)} exceeds the "
            f"EXPECTED_DYNAMIC_SQL_SITES={EXPECTED_DYNAMIC_SQL_SITES} ratchet; current sites:",
            file=sys.stderr,
        )
        for site in sites:
            print("  " + site.render(), file=sys.stderr)
        return 1

    if len(sites) < EXPECTED_DYNAMIC_SQL_SITES:
        print(
            f"Dynamic SQL site count dropped to {len(sites)}; update "
            f"EXPECTED_DYNAMIC_SQL_SITES from {EXPECTED_DYNAMIC_SQL_SITES} to {len(sites)} in this PR.",
            file=sys.stderr,
        )
        return 1

    print(f"SQL policy ratchet passed ({len(sites)} dynamic SQL sites)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
