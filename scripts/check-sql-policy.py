#!/usr/bin/env python3
"""Detect dynamic SQL sites that need an explicit safety proof.

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
    # The projection generator. `crates/storage-pg/src/projection.rs`
    # builds one statement per projected schema out of `FlavorContract`
    # declarations — every spliced name goes through `PgIdent` INSIDE the
    # generator, and every value is a bind ($1 memory id, $2 language, $3
    # schema id). The execution sites hold an opaque `String` off the frozen
    # registry, so `PgIdent` is not the proof AT THE SITE; the proof is that
    # the string has exactly one producer and that producer is audited. This
    # is deliberately narrower than `fixed-fragment`: it names the generator
    # as the thing to re-read, not the call site.
    #
    # `crates/storage-pg/src/integrity.rs` is the second producer under this
    # proof, on the same terms: `unprojected_rows_sql` / `undeclared_rows_sql`
    # splice only the sidecar table, its declared memory-key column and the
    # projection table, each through `PgIdent`, and bind the schema id / table
    # name. The proof is the same one — one audited producer per string — and
    # the same file is the thing to re-read.
    #
    # `crates/storage-pg/src/access/scope_surfaces.rs` is the third:
    # `generate_probe` splices a flavor's `ScopeDecl` — registry table, id
    # column, owner kind and owner id columns — each through `PgIdent`, and
    # the probe binds all three values it compares. Its execution site holds
    # an `Arc<str>` resolved off the frozen registry, so again the proof is
    # that the string has exactly one producer and that producer is audited.
    "SQL-POLICY: generated",
)

# Every reviewed dynamic-SQL site proves itself with an inline
# `SQL-POLICY:` comment (see VALID_PROOFS) within four lines of the call.
#
# Proofs are content-addressed. Do not reintroduce a `(path, line, kind) ->
# proof` allowlist: a line pin is not inert — `intrinsic_proof` returns True
# on a pin match *before* inspecting any content, so a pin that has drifted
# off its site silently vouches for whatever dynamic SQL later lands on that
# line number, and any insertion above a live pin shifts the line and fails
# CI for an unrelated change.


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


# WHAT THIS NUMBER IS NOT
# =======================
# Read the ratchet as a review tripwire on a SAMPLE of the tree's dynamic SQL,
# not as a census of it. `collect_sites` scans line by line, so two large
# classes of macro-built statement are structurally invisible to it, and the
# arithmetic in the log below moves for reasons that are sometimes about
# formatting rather than about safety. The 2026-08-1x entry for
# `verbs/forget.rs` is a worked example of exactly that, kept in place.
#
#   1. WRAPPED `format!`. The detector's rule requires `format!(` and the
#      opening quote of its template to be on the SAME line. Measured at this
#      commit against the detector's own keyword set (SELECT/INSERT/UPDATE/
#      DELETE/WITH/CREATE/ALTER/DROP), the tree holds 53 `format!` calls whose
#      template begins with one of those verbs. The per-line rule can see 4
#      (and `safe_format_sql` then vouches two of those away, so only 2 stand
#      in the 76). Forty-nine are invisible for no reason but line breaks —
#      and rustfmt, not the author, decides where most of those breaks fall.
#      Widen the verb set to include SET/TRUNCATE/EXPLAIN/ANALYZE and it is
#      67 statements against 17 visible — widening the verb set widens the
#      rule too, and most of the newly visible ones are the single-line
#      EXPLAIN templates in the plan-pin tests. (Reproduce: whole-file regex
#      with re.DOTALL, comment lines stripped. The exact figures are
#      sensitive to the verb set, which is the point.)
#
#   2. `sqlx::raw_sql`. Not a pattern `collect_sites` looks for at all. There
#      are 14 call sites. The clearest one is `pgvector.rs`'s
#      `set_hnsw_search_sql`, which assembles `SET LOCAL hnsw.ef_search = {};
#      SET LOCAL hnsw.iterative_scan = {}` with `format!` and `write!`, and is
#      handed to `raw_sql` at `lib.rs:699` and two other reads. That statement
#      is safe — the interpolated values are integers and a closed enum off
#      `PgTuning` — but it is safe because of what it interpolates, not
#      because this script noticed it. And the stakes at a `raw_sql` site are
#      HIGHER than at a `query()` site, which is why the class matters: that
#      two-statement payload is possible at all because `raw_sql` runs the
#      SIMPLE query protocol, which permits `;`-chained statements. An
#      injection there escalates to arbitrary extra statements; the extended
#      protocol behind `query()`/`query_as` forbids chaining outright.
#
#   3. `push_str` assembly the receiver-name heuristic misses. The
#      `sql-push-str` rule fires only when the receiver's name matches
#      `\w*sql\w*`, and `intrinsic_proof` blanket-vouches every such site in
#      `sidecars/sql.rs`. A builder named anything else assembles statements
#      under both radars.
#
# WHERE THE SAFETY ACTUALLY COMES FROM
# ====================================
# The real boundary is `crates/storage-pg/src/pg_ident.rs`. `is_ident_part`
# (:50-58) admits a string only if it is non-empty, at most 63 bytes, starts
# with an ASCII letter or `_`, and contains nothing but ASCII alphanumerics
# and `_`. Nothing that passes that filter can close a quote, open a comment
# or introduce a statement, which is what makes a `PgIdent` substitution
# `%I`-equivalent. Every identifier spliced by the four generators —
# `series_leg_sql`, `dedupe_lookup_sql`, `remap_sql`, `forget_leg_sql` —
# routes through it, and the identifiers themselves come
# from `const` contracts that `try_freeze` validated and that
# `every_column_a_declaration_names_is_a_column_the_catalog_has` resolved
# against `information_schema`.
#
# So: this constant is worth keeping and worth arguing over, because the log
# below is where someone has to write down what they did and why. It is not
# evidence that dynamic SQL in this tree is bounded at 76 places. Making the
# detector see wrapped `format!` and `raw_sql` would re-baseline roughly 120
# sites.
#
# Exact current dynamic SQL inventory count (see `--inventory`). This is a
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
# 2026-08-01 analysis: -5 — the edge reset. An edge has no id and no
# payload, so the sidecar-driven edge read (whose statement text grew with the
# registered payload specs) is gone, and the edge write, lineage walk and
# owner erase now assemble fixed fragments instead of per-request column
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
# 2026-08-14 analysis: +8 — the search-path work builds its SQL. The
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
# in that function (the `m2.memory_id IS NULL` anti-join tail and the
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
# `owner_erase::delete_owned_surfaces` renders one `DELETE` per declared
# `EraseLeg::Owned` surface, filtered on that surface's own `owner_id`.
# `owner_export::export_statement`'s owner-pinned arm — the one taken when
# `owner_column` is `Some` — renders the matching `SELECT ... WHERE
# s.owner_id`. Each stands where a single hardcoded
# `proxima_core.mcp_call_logged_v1` statement would otherwise be, so the
# count rises while the hardcoded table name disappears. The third,
# `sidecars::read_ctx::fetch_all_by_memory_ids_owner_pinned`, is the read
# half: backend-generated SQL from `memory_select_batch_owner_pinned_sql`,
# whose `proxima_core.memory` join IS the owner rule and is therefore the
# one sidecar read allowed to name a core table.
#
# 55 -> 56 with the embedding byte-parity gate
# (`storage-pg/tests/flavor_contract_acceptance.rs`). Test-only, and the
# dynamism is the point of the test: it reads the `(table, column)` pair a
# flavor #0 `EmbeddingRecipe` resolves to, and compares the bytes against
# what the shipped `embed_text_column` drain returns for the same row. A
# hardcoded statement would be a second hand-written map of exactly the
# thing the recipe exists to stop hand-writing, so it would pass while the
# recipe pointed somewhere else. Both fragments are `&'static str` off the
# compiled-in contract; the id is bound.
# 56 -> 66 with the projection. Ten sites, six of them the
# generator's own statements and their two DROP inverses:
#
# - `projection.rs` x2: the `DROP TABLE` / `DROP INDEX` inverses every
#   artifact carries. Both splice a `PgIdent`-validated name and nothing
#   else, which is why they carry `SQL-POLICY: PgIdent` rather than the new
#   `generated` proof — there is no bind and no generator indirection.
# - `sidecars/frozen.rs` x2: the write path and the hydrate rebuild, both
#   executing `projection_insert_sql`'s output for the schema the frozen
#   entry names. This is the collapse's whole point: ONE producer of that
#   statement, reached from every verb that writes searchable text.
# - `access/owner_columns.rs` x1: `projection_transfer_sql`, the `UPDATE
#   ... SET owner_id` that makes the projection follow a transferred series.
#   The table list is the frozen registry's, never a literal.
# - `flavors/code` x3 (`search_chunks.rs`, `search_commits.rs` x2): the
#   flavor's GIN arms became `LazyLock<String>` because they now RENDER
#   their score bands from the contract's `Band` constants instead of
#   carrying float literals. The text is still a compiled-in fragment with
#   every value bound, so the proof stays `fixed-fragment`; it is dynamic
#   only in the sense that a `format!` produced it once at first use.
#
# The last two are test-only: `search_sidecar.rs` and
# `flavors/code/tests/common/mod.rs` each run `projection_insert_sql` to
# keep the projection for rows they hand-seed. A restated INSERT there
# would be a second copy of the vector expression the generator exists to
# be the only copy of — the fixture would agree with itself and disagree
# with production.
#
# 66 -> 67 with the identity re-proof
# (`storage-pg/tests/search_projection_identity.rs`). Test-only, and the
# same reason as the two fixtures above: it hand-seeds rows and keeps
# their projection with the generator's own statement, because a test that
# proved the projection returns the pre-projection results while writing
# its vector by hand would have proved nothing about production.
# 67 -> 73 with the search collapse. Six sites, none of them a new
# mechanism:
#
#   +1 `verbs/query/search.rs` — the ranked arm and the substring arm are two
#      statements, `RankedRow` and `SubstringRow`, rather than one statement
#      whose shape a flag switched. Both carry fixed-fragment proofs.
#   +3 `flavors/code/src/mcp/search_chunks.rs` (1) and `search_commits.rs`
#      (2) — the three `LIKE` arms are `LazyLock<String>` rather than
#      `&'static str`, because each renders part of itself from the flavor's
#      DECLARATION, and the two arms differ in WHICH part:
#        * `COMMIT_LIKE_SQL` / `SUMMARY_LIKE_SQL` interpolate the declared
#          band floor as well as the schema id, and a band floor is a
#          `format!("{:.2}", ..)` no `const` can produce.
#        * `CHUNK_LIKE_SQL` interpolates only the declared schema id, which
#          IS a `&'static str` const — chunk search bands nothing, it adds
#          literal bonuses. A `const` still cannot build the statement:
#          `concat!` takes literals, not const items, so the only `const`
#          spelling is a second copy of the id the declaration exists to be
#          the one copy of.
#      Trading three literals for three `LazyLock<String>`s is the cost of
#      the arms being declared rather than hand-written; each is built once,
#      from data with no caller text in it, and carries a
#      `PgIdent`/fixed-fragment proof.
#   +2 test-only plan pins: `storage-pg/tests/hot_path_plans.rs` EXPLAINs the
#      substring statement and `flavors/code/tests/hot_path_plans_pg.rs`
#      EXPLAINs the three `LIKE` arms to pin the owner predicate on the
#      flavor's own projection. Both EXPLAIN the audited production builders'
#      output, which is the same shape as the 2026-07-16 HNSW pin above.
#
# 73 -> 74. One more test-only EXPLAIN in
# `flavors/code/tests/hot_path_plans_pg.rs`, covering the commit and
# commit-summary ranked arms: each bound `p.owner_id` with nothing reading it
# back, so `AND $4::uuid[] IS NOT NULL` passed the entire workspace. The
# owner predicate is pinned on every arm, not only on the chunk ranked arm
# and the three `LIKE` arms.
#
# 74 -> 76. Two test-only sites in
# `crates/storage-pg/tests/projection_maintenance.rs`, running the
# GENERATOR's own statement twice against one seeded memory: once with a
# schema id the memory does not carry, which must write nothing, and once
# with its own, which must write one row. That is the pin for
# `projection_insert_sql`'s `AND m.schema_id = $3` guard, and it cannot be
# written any other way — the whole property under test is what the
# generated statement does with a bind, so a hand-restated INSERT would
# prove nothing about it. Both carry `SQL-POLICY: generated`, whose meaning
# is exactly this: the string has one producer and that producer is audited.
#
# 76 -> 75 with the compliance clean. Net of a bigger reshuffle than
# the number suggests. The owner export lost NINE hand-written SQL constants
# and four dynamic sidecar builders and gained ONE generator
# (`export_statement`), and the owner erase lost five hand-assembled table
# sweeps for two generic loops. Generating a statement per DECLARED surface
# instead of writing one per remembered table is not more dynamic SQL — it
# is the same statement shapes with one producer each, which is what this
# ratchet was built to reward. The differential harness contributes ZERO
# sites on purpose: it dumps every base relation
# through one constant statement that lets Postgres assemble the per-relation
# query with `format(..., %I)` inside `query_to_xml`, so the obvious spelling
# — a `format!` per name read from `information_schema` — never enters the
# tree. A harness should cost nothing to keep.
# 75 -> 76, policy landing on the contract. The arithmetic is stated rather
# than absorbed, because the standing rule is that this number should not
# rise.
#
#   -1  crates/storage-pg/src/access/owner_columns.rs, the projection
#       registry walk. It was the transfer path's ONLY registry-derived
#       statement — one generated UPDATE over the frozen sidecar
#       registry's projection tables — and it is subsumed by the leg loop
#       below, which reaches the projection because the projection is a
#       declared `Follow` surface like any other.
#   +1  the same file, `series_leg_sql` under the generated-leg loop. ONE
#       call site that replaces NINE hand-written statements over nine
#       remembered tables: `cooled`, `memory`, `sketch`, three embedding
#       tables differing only in a name, every flavor's projection, and the
#       `DELETE FROM ingest_keys`. The statement is chosen by the surface's
#       resolved `TransferLeg` and the only values substituted into it are
#       the table name, the key column and the owner column — all
#       `&'static str` from a `const` contract that `try_freeze` validated
#       and that `every_column_a_declaration_names_is_a_column_the_catalog_has`
#       resolved against `information_schema`. %I-equivalent substitution.
#   +1  `dedupe_lookup_sql`, the "does the destination already hold these
#       bytes" probe, generated from `blob`'s declared `dedupe_key` instead
#       of restating its three columns in SQL. Same substitution class, and
#       `every_dedupe_key_is_a_uniqueness_the_schema_enforces` asks
#       `pg_index` whether the declared key is the constraint the arm
#       exists to avoid colliding with.
#   +1  `remap_sql` under `remap_handle_refs`, generated from the declared
#       `remaps`. This is the site that pays for itself twice: the function
#       it replaces carried a doc comment reading "This is
#       `TransferRule::FollowOrDedupe { remaps }` executed" above two
#       hardcoded UPDATEs, while the declaration named three columns. Prose
#       claiming a declaration drives code that it does not drive is the
#       exact defect this removes.
#   -1  crates/storage-pg/src/verbs/forget.rs, and this one is a DETECTOR
#       ARTEFACT, not a reduction.
#
#       At the base commit the file held four counted sites; it now holds
#       three. The site that left is the stamped-sidecar delete's
#       `let sql = format!("DELETE FROM {tbl} WHERE t = $1", ...)` — a
#       SINGLE-LINE `format!`, and therefore visible to the per-line rule in
#       `collect_sites`. Its replacement, `forget_leg_sql`, builds the same
#       class of statement with a `format!(` whose template string sits on
#       the NEXT line. The regex is applied per line, so it matches nothing.
#       The execution site is intact and the two unrelated sites in the file
#       are untouched.
#
#       So the minus one is bought by wrapping a macro call. Nothing about
#       the tree is safer or smaller for it.
#
#       The four hand-written `DELETE`s over `embedding_jobs`,
#       `embedding_heads`, `embeddings` and `sketch` are NOT part of this
#       minus one. They are plain string literals — `sqlx::query("DELETE
#       FROM proxima_core.embeddings WHERE entity_id = $1")` — which this
#       detector does not count and should not. Absorbing them into the
#       declared-leg loop is a real win in the code and worth exactly zero
#       sites; folding it into the arithmetic would make a mechanical
#       accident read as an earned improvement.
#
# Net +1, and the trade is thirteen static statements over remembered tables
# for three generated ones over declared surfaces. That is the ratchet's
# purpose, not a concession to it: dynamic SQL should be DELIBERATE, and a
# statement whose only variable is an identifier from a frozen, freeze-
# validated, catalog-checked contract is the most deliberate SQL in the
# tree. What the trade buys is that a flavor adding a `Follow` surface
# cannot be silently not-followed — `FlavorRegistryError::UnmovableSurface`
# refuses it at boot, and `UnforgettableSurface` does the same for a surface
# that claims forget destroys its rows.
#
# 76 -> 82: the six sites of the shared-catalog contention regression tests
# (`boot_pg.rs` role-DDL migrator + admin-side CREATE/ALTER/DROP ROLE). All
# six splice one identifier the test itself mints from `Uuid::now_v7()`
# under a fixed prefix; the dynamism exists only because Postgres has no
# bind parameters for role names in DDL.
#
# 82 -> 83: one site in `projection_maintenance.rs`, which runs the
# projection generator's own output against a sidecar whose memory-key
# column is not `t`. The generator reads that column from
# `KeyShape::MemoryT { column }`; the emitted text is asserted in unit
# tests, but only executing it proves the statement is valid Postgres
# against a table with that column and that the row lands. The statement is
# the one `attach_projections` stores, so the site carries the
# `SQL-POLICY: generated` proof like every other execution of it.
# 83 -> 90: the seven sites of the write session's own sidecar read
# (`sidecars/read_ctx.rs::read_own_sidecar_in_tx`) — one `QueryBuilder`
# constructor, two `PgIdent` pushes (table, predicate column), four fixed
# fragments. They are a net REDUCTION in dynamic SQL reachable by a caller,
# not an increase: the affordance exists so a flavor doing a read-model
# precondition check inside the write transaction never holds a
# `&mut Transaction` and never writes its own statement. Every value is a
# bound `SidecarAtom` and both identifiers go through `PgIdent`, so the
# builder can only emit a single-table `SELECT` over a registered sidecar.
#
# 90 -> 92: the two further sites of that read's OWNER SCOPE — one `PgIdent`
# push for the owner column the table's `Surface` declares, one fixed `=`
# fragment. The bound value is the resolved permit's owner, never a caller's:
# the scope is stamped server-side and a caller's predicates are `AND`-ed
# after it, so they can narrow the owner's rows and never widen past them.
# 92 -> 94: the declaration-fidelity sweep. Both new sites are in
# `verbs/query/series_handle.rs`, and they are one fixed fragment split in
# two around one identifier. The series-head lookup joined its sidecar back
# to `proxima_core.memory` with a literal `m.t = s.t`, so a sidecar keyed on
# any other column — `pg_sidecar!` accepts any `key:` ident — could not be
# found through it at all: an ingest looking for its own current head found
# none and started a second series. The sidecar side is now the column the
# registration declares, pushed through `PgIdent`, which turns one
# `builder.push(" s JOIN … WHERE …")` into a fragment, a `PgIdent` push, and
# a second fragment. `m.t` and `h.t` stay literal: they are the kernel
# tables' own keys.
#
# The sweep's other three fixes add ZERO sites, and that is the shape to
# expect rather than a coincidence. `fact_embeddings/text.rs` (the drain's
# text read, now filtered on `MemoryEmbedUnit::key_column`),
# `verbs/forget.rs`'s stamped-sidecar dump (now keyed off the frozen
# registry's declared column instead of `s.t`), and the `owner_id` literals
# in `verbs/owner_erase.rs` / `verbs/owner_export.rs` (now
# `Surface::owner_column`) all interpolate one more validated identifier
# into a `format!` the per-line rule already counted. Replacing a literal
# name with a declared one is invisible to this detector by construction,
# which is worth stating: the number moves for the shape of the assembly,
# not for the fidelity of it.
#
# 94 -> 96: the two execution sites of `PgSidecarRegistryFrozen::
# integrity_check` (`sidecars/frozen.rs`), one per class it checks. Both run a
# statement from `crates/storage-pg/src/integrity.rs`, both carry
# `SQL-POLICY: generated`, and both bind the only value they compare against
# ($1: the schema id, then the table name).
#
# The M4 half of the same slice adds ZERO sites, and for the reason the
# `fact_embeddings/text.rs` note above gives rather than by luck: the trigger
# generator is written in `projection.rs`'s own shape — a multi-line `format!`
# whose template starts on the line after the macro — so the per-line rule does
# not see it, exactly as it does not see `projection_artifacts`' `CREATE TABLE`.
# The DDL it emits is executed by `sqlx migrate`, not by a call site in the
# tree, so there is no execution site to count either. Two generators of DDL,
# both audited the same way, both invisible here. That is a property of the
# detector, not a claim about the code, and it is written down so nobody reads
# the flat number as coverage.
#
# 96 -> 97: one test-only site in `storage-pg/tests/migrations.rs`, which
# EXECUTES `declaration_trigger`'s output against a sidecar table keyed on
# `note_memory_id` rather than `t`. The emitted text is asserted in unit
# tests; only running it shows that `to_jsonb(NEW) ->> TG_ARGV[0]` reads the
# DECLARED column out of the row — every sidecar core ships keys on `t`, so a
# guard that hardcoded `NEW.t` would pass everything else in that file. Same
# shape as the `projection_maintenance.rs` site at 82 -> 83, and it carries
# the same `SQL-POLICY: generated` proof.
#
# 97 -> 99: two test-only sites in `storage-pg/tests/migrations.rs` disable
# and re-enable each reference-integrity trigger in turn. Both identifiers
# come exclusively from the closed literal tuple matrix beside the calls, and
# each site carries `SQL-POLICY: fixed-fragment`; executing both directions is
# what proves the boot marker refuses a missing trigger and accepts its repair.
# 99 -> 102: three advisory/bookkeeping sites that select between a closed
# pair of `&'static str` constants declared beside the call. SQL cannot
# parameterize a function name, so the owner and source fences each need one
# spelling for `pg_advisory_xact_lock` and one for
# `pg_advisory_xact_lock_shared`; the third picks between the two bulk-erase
# selection-count statements. Nothing caller-supplied reaches the statement
# text at any of the three — every varying value is a bind — and each carries
# `SQL-POLICY: fixed-fragment`. The alternative was to inline both spellings at
# each call, which duplicates the bind chain and the ordering argument written
# above it; keeping one call chain per fence is what these sites buy.
#
# 102 -> 106: four cold-record sites in `storage-pg/src/verbs/forget.rs` make
# preservation a function of the frozen flavor contract rather than a table
# list compiled into storage: dump a declared cascaded detail relation, probe
# ordinary-sidecar and detail-row shapes behind savepoints, and restore the
# declared detail rows. Every schema/table/key fragment passes through
# `PgIdent`; every row value is a bind. Literal SQL cannot express this lane
# because out-of-tree flavors supply the relation and key declarations, while
# the contract stamp is precisely what hydration must verify before writing.
#
# 106 -> 111: the third direction of the declaration invariant, stamp <= rows.
# Three `DROP TRIGGER` inverses in `storage-pg/src/integrity.rs` — the presence
# trigger's, the key-repoint trigger's, and the split that gave the existing
# declaration trigger its own — one reader in `sidecars/frozen.rs` running the
# `MissingStampedSidecarRows` count, and one fixture statement in
# `verbs/forget_pg_tests.rs` that seeds the row a stamp promises. Every
# relation and column fragment passes through `PgIdent` off the frozen
# registry; the only runtime value is the memory key, which is a bind. Literal
# SQL cannot express these because the guarded set is exactly the
# non-owner-pinned sidecars an out-of-tree flavor declares.
#
# 111 -> 112: `a_key_repoint_onto_an_undeclared_memory_is_refused` installs the
# generated `_on_update` trigger on a hand-made table, because every sidecar
# core ships is append-only and would refuse the UPDATE before the guard runs.
# Same generator, same `PgIdent` path as its `BEFORE INSERT` sibling one test
# above.
#
# 112 -> 114: the orphan guard — direction 3 against a DELETE. One `DROP
# TRIGGER` inverse in `storage-pg/src/integrity.rs` for the new family, and
# two in `a_damaged_presence_trigger_fails_the_boot_guardrail`: the fixed
# `DROP` it damages the database with, and the generator's own artifact it
# repairs the database with between cases. Same `PgIdent` path as the three
# families above it.
#
# 114 -> 116: the boot guardrail renders `pg_get_triggerdef` and compares it
# whole, and that rendering is a function of `search_path` and
# `quote_all_identifiers` as well as of the trigger. Two sites: the `SET LOCAL`
# loop in `ensure_declaration_triggers` that pins both for its own read, and
# the `ALTER DATABASE <uuid-name> SET ...` loop in
# `the_boot_guardrail_ignores_the_callers_session_settings` that hands it a
# database where both are hostile. Both splice only literals from the array
# immediately above them, plus — in the test — the uuid-derived database name
# the test itself just created.
#
# 116 -> 118: the declared lifecycle-scope fence. One `sqlx::query(sql)` in
# `access/owner_columns.rs::lock_scope_fence`, choosing between two adjacent
# `&'static str` constants because SQL cannot parameterize a lock function
# name — the same shape as the owner and source fences three functions above
# it. One `sqlx::query_scalar` in `verbs/memory_timeseries.rs`, executing the
# liveness probe `access/scope_surfaces.rs` GENERATED from a flavor's
# `ScopeDecl`: every identifier in it passed through `PgIdent` at generation
# time, and the three values it compares are binds. Literal SQL cannot express
# it because the registry table and its columns are named by an out-of-tree
# flavor's declaration.
EXPECTED_DYNAMIC_SQL_SITES = 117


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
