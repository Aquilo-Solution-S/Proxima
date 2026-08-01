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

# Every reviewed dynamic-SQL site proves itself with an inline
# `SQL-POLICY:` comment (see VALID_PROOFS) within four lines of the call.
#
# This used to be a `(path, line, kind) -> proof` allowlist, and it rotted:
# by v0.0.7, 16 of its 23 entries matched no site at all. A stale pin is not
# inert — `intrinsic_proof` returns True on a pin match *before* inspecting
# any content, so a stale entry silently vouches for whatever dynamic SQL
# later lands on that line number. The live entries were no better: any
# insertion above them shifted the line and failed CI for an unrelated
# change. Proofs are content-addressed now. Do not reintroduce line pins.


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
# 2026-07-16 analysis: +2 — the HNSW plan-validation test
# (storage-pg/tests/integration/search_pg.rs) EXPLAINs the audited
# production semantic-branch SQL and applies fixed SET LOCAL fragments;
# both sites carry fixed-fragment proofs.
# 2026-07-16 analysis: +1 — the goal-state filter in query/goals.rs pushes a
# closed-enum fixed fragment (no caller text); proof comment inline.
# 2026-07-16 analysis: +1 — PgSidecarReadCtx::fetch_all_by_edge_ids runs the
# same validate_sidecar_read_sql-gated backend-owned SQL as its memory-id
# siblings (edge-id batch reads for edge payload read-back); proof inline.
# 2026-07-17 analysis: +1 — load_memories_by_ids (consolidate/memories.rs),
# the proxima://memories batch head read: the same entity_owner_union /
# read_owner_predicate fixed fragments every owner-scoped read composes;
# proof inline.
# 2026-07-21 analysis: -2 net — the goals HeadsOnly supersession filter moved
# into the shared push_goal_heads_only_predicate helper (one proven push_str
# in query/mod.rs replaces two per-verb copies), and change_history's
# high-water query was replaced by the shared read_seq_high_water; the
# remaining sites in the touched files (active_goals, change_history,
# query/goals, query/memories) now carry inline fixed-fragment proofs, so
# their line-pinned allowlist entries were removed.
# 2026-07-26 analysis: +2 — the owner-scoped search plan tests
# (search_pg.rs) EXPLAIN the exact production branch SQL from
# lexical/semantic_search_sql_for_tests; both sites are parameter-bound
# EXPLAIN prefixes over the audited builders with inline fixed-fragment
# proofs.
# 2026-07-26 refactor: +-0 — search_pg.rs was split into search_pg/ submodules;
# all four EXPLAIN sites named above now live in search_pg/plans.rs, unchanged.
# 2026-07-26 analysis: +1 — the stored-tsvector drift test (search_pg/
# stored_tsv.rs) interpolates the pre-0011 tsvector expression, held as a
# module constant, into a comparison against proxima_core.lexical_tsv. The
# only interpolated text is that constant; the inputs under test are bound.
# 2026-07-26 analysis: +1 — the code-flavor equivalent (flavors/code/tests/
# chunk_search_tsv_pg.rs) interpolates the pre-migration chunk tsvector
# expression, held as a module constant, into a comparison against the stored
# generated column. Same shape as the core drift test above: the only
# interpolated text is that constant; both inputs under test are bound.
# 2026-07-28 analysis: +1 — search.rs's owner-scope gate stopped interpolating
# entity_owner_union() and became a fixed fragment, which moves it from write!
# (uncounted) to sql.push_str (counted). Nothing is interpolated: the read set
# arrives as the bound arrays $1/$2. A counted site with a fixed fragment is a
# smaller surface than the interpolating write! it replaced, not a larger one.
# 2026-08-01 analysis: -5 — the v0.0.8 edge lane. An edge has no id and no
# payload, so the sidecar-driven edge read (whose statement text grew with the
# registered payload specs) is gone, and the edge write, lineage walk and
# compliance erase now assemble fixed fragments instead of per-request column
# lists. Fewer places where SQL is built at all.
EXPECTED_DYNAMIC_SQL_SITES = 51


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
