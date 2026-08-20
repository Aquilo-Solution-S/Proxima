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
# 2026-08-17 analysis: +9 — C2 hot-path EXPLAIN guards
# (storage-pg/tests/hot_path_plans.rs, flavors/code/tests/hot_path_plans_pg.rs)
# prefix EXPLAIN (FORMAT JSON, COSTS OFF) onto production builders; each site
# is parameter-bound with an inline fixed-fragment proof.
# 2026-08-19 analysis: +1 — the code-flavor commit/summary lexical-authority
# test compares each generated search_tsv against a SQL expression assembled
# only from its fixed table/field cases; the row identity remains bound.
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
# 2026-08-01 analysis: -5 — the v0.0.7 edge reset. An edge has no id and no
# payload, so the sidecar-driven edge read (whose statement text grew with the
# registered payload specs) is gone, and the edge write, lineage walk and
# compliance erase now assemble fixed fragments instead of per-request column
# lists. Fewer places where SQL is built at all.
# 2026-08-04 analysis: +3 — the per-flavor ledger split. The migration facade's
# one-time cutover interpolates the flavor migrator's compiled-in tracking
# table name (and its create_schemas entries) into CREATE SCHEMA / CREATE
# TABLE / INSERT..SELECT, exactly as SQLx itself interpolates the configured
# table name; nothing reaches these strings from a caller.
# 2026-08-11 analysis: +9 — the entity-owner union stopped being restated.
# Twelve reads carried their own copy of the memories-∪-goals union that
# `entity_owner_union()` exists to hold; nine of them were plain literals and
# become counted sites by interpolating it (the other three already built
# their SQL). This is the same trade already accepted on 2026-07-17 for
# `load_memories_by_ids`, nine times over: the only interpolated text is a
# crate-private `&'static str` with no path from any caller, and the
# alternative is twelve independent spellings of the ownership model, which
# `check_entity_owner_union` in check-architecture-guardrails.py now forbids.
# The union text is byte-identical at every site, so no statement changed.
# 2026-08-14 analysis: +8 — the v0.0.8 search-path work builds its SQL. The
# index-first / window-dedup builders in verbs/query/search.rs compose the
# candidate CTE and the vector scan from fixed fragments behind
# `sql.push_str` (seven sites landed with that wave; the count was not
# bumped then because one of them was also missing its proof comment, and
# that failure exits before this ratchet is checked — both are fixed in the
# same change as this entry). The eighth is the kind-specialized head
# filter's fact-only fragment. Every added site pushes a plain string
# literal with no caller-reachable interpolation and carries a
# `SQL-POLICY: fixed-fragment` proof, and the emitted statements are pinned
# byte-for-byte by the golden tests in the same file.
# 2026-08-14 analysis (round 2): -2 — the search head filter stopped restating
# the fact-head liveness test. `push_search_head_filter` (verbs/query/search.rs)
# carried the same `m.fact_entity_id IS NULL OR EXISTS (…fact_entities…)`
# predicate twice: once as the kind-specialized fact-only arm, once as the
# mixed arm's first disjunct. It is now the module const FACT_HEAD_TEST,
# interpolated by `write!` — which this detector does not count, while
# `push_str` with a next-line literal does — so exactly those two `sql.push_str`
# sites disappear and nothing is added (verified by diffing `--inventory`
# against 19f63010: two removals, zero additions). The remaining push_str sites
# in that function (the `m2.memory_id IS NULL` anti-join tail and the legacy
# `NOT EXISTS` spelling) are untouched and keep their inline fixed-fragment
# proofs; the two that moved to `write!` interpolate one crate-private
# `&'static str` with no path from any caller, exactly as they did as literals,
# and the emitted statements are byte-identical — all ten `*_GOLDEN` literals
# in search.rs are unchanged from 19f63010.
# 2026-08-15 analysis: +4 — the semantic branch now ranks before it decides
# eligibility, which splits its assembly into named pieces. Three of the four
# are `sql.push_str` with a next-line string literal and no caller-reachable
# interpolation at all: `push_ann_live` (the head join and per-memory
# collapse), `push_rank_first_eligible` (the collapse and its written tie
# rule), and `push_ann_restriction`, whose argument is the module const
# ANN_RESTRICTION_SQL reached only through `CandidateShape.ann_restriction`,
# which no caller outside this module can set. The fourth pushes the audited
# candidate builder's own return value: `common_candidates_sql` composes it
# from the same fixed fragments and `$n` placeholders it always did, with the
# only identifiers arriving through `PgIdent`, and it is written after the
# scan CTE rather than at the head of the statement solely because the branches
# are now restricted to that scan. All four carry inline
# `SQL-POLICY: fixed-fragment` proofs and every emitted arm stays pinned
# byte-for-byte by the `*_GOLDEN` literals in the same file — including
# SEMANTIC_BRANCH_LEGACY_GOLDEN and SEMANTIC_WINDOW_DEDUP_GOLDEN, the two
# escape-hatch arms, which are unchanged from 0a12aa0f.
#
# 2026-08-15 analysis (round 2): +1 — the rank-first plan guard
# (search_pg/plans.rs `rank_first_probes_memories_for_the_window_instead_of_\
# scanning_the_owner`). Test-only, and the same shape the two plan-test sites
# above it already carry: an `EXPLAIN (FORMAT JSON, COSTS OFF)` prefix
# concatenated onto the audited production builder's own return value, with
# every caller value still arriving as a bind. Nothing in the interpolation is
# reachable from a request. It exists because the redesign's 22x is a bet on
# the planner probing `memories` for the ANN window rather than enumerating
# the owner, and both plans return the same rows — so a planner that picks the
# slow one is not a wrong answer and no assertion in the suite noticed it.
#
# 2026-08-15 analysis (sweep fixes): -8 net, and the direction is the finding.
# Twelve sites go away because the sweep fixes replace runtime-composed SQL
# with statements that have nothing left to compose. The de-union of the
# memory-keyed owner readers accounts for five (`active_goals.rs` x1,
# `consolidate/memories.rs` x3, `derive_append.rs` x1): those readers built a
# polymorphic entity-owner union at run time to serve one key type, and a
# direct probe of `memories` needs no builder. Spelling the owner predicate
# with `=` where NULL-impossibility is provable from a CHECK removes three
# more in `fact_embeddings/text.rs`; the read-owner scope arm removes the two
# `sql.push_str` sites in `query/goals.rs`; and the claim rewrite retires two
# `format!`-built statements in `fact_embeddings/jobs.rs` for one module
# const.
#
# Nothing is added in production. Each rewrite ships as the only spelling of
# its statement rather than as one arm of a flag, so the four new sites are
# all test-only plan guards (`edge_prefilter_pg.rs` x2,
# `fact_embeddings_pg/claims.rs`, `owner_scope_pg.rs`) — `EXPLAIN` prefixes
# over the audited builders' own return values, the shape the plan-test sites
# above already carry. Same reason as the rank-first guard: the rewrite and
# what it replaced return the same rows, so only a plan assertion can catch
# the statement silently losing the index the rewrite exists to reach.
#
# 66 -> 71 with the lexical GIN-first split (migration 0019). One production
# site: `run_substring` in `verbs/query/search.rs`, which sends the substring
# band's own statement — the same `AssertSqlSafe` over an audited builder's
# return value that `run_lexical` beside it already is. The band moved into
# its own statement because no core index can serve `LIKE '%...%'`, and one
# unservable arm in a disjunction costs the whole statement its index path.
#
# The other four are test-only, all in `search_pg/plans.rs`: the corpus the
# new plan guard needs (a bulk INSERT and its ANALYZE) and two `EXPLAIN`
# prefixes over the two builders the guard compares. That guard is the only
# thing standing between the gate and its old position above the candidate
# CTE — both spellings return identical rows, so nothing else in the suite
# would notice the index becoming unreachable again.
# 52 -> 55 with the owner-pinned Memory sidecar. All three are production,
# and all three are the same statement in three places: "reach an
# owner-pinned sidecar by its OWN owner_id rather than through the Memory
# it hangs off". The table name is the only dynamic part and it comes from
# the frozen sidecar registry through `PgIdent::table`, exactly as the
# memory-keyed sidecar sweeps beside them already do — every value is bound.
#
# `compliance_erase::delete_owner_pinned_sidecars` and
# `compliance_export::owner_pinned_sidecar_rows` replace what used to be a
# single hardcoded `proxima_core.mcp_call_logged_v1` statement each, so the
# count rises while the hardcoded table name disappears. The third,
# `sidecars::read_ctx::fetch_all_by_memory_ids_owner_pinned`, is the read
# half: backend-generated SQL from `memory_select_batch_owner_pinned_sql`,
# whose `proxima_core.memory` join IS the owner rule and is therefore the
# one sidecar read allowed to name a core table.
EXPECTED_DYNAMIC_SQL_SITES = 55


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
