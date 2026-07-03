#!/usr/bin/env python3
"""Synthetic pgvector ANN baseline for Proxima owner-filtered search.

The harness uses only stdlib Python plus `psql`. It creates a disposable
`ann_bench` schema in the target database.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlsplit, urlunsplit


HOT_OWNER = "00000000-0000-4000-8000-000000000001"
COLD_OWNER = "00000000-0000-4000-8000-000000000002"
NOISE_OWNER_BASE = "00000000-0000-4000-8000-1000000000"
MODEL_ID = "ann-bench-v1"
PSQL_COMMAND_TAGS = {"BEGIN", "COMMIT", "ROLLBACK", "SET"}
MIN_VECTOR_CANDIDATE_OVERFETCH = 512
VECTOR_CANDIDATE_OVERFETCH_PER_RESULT = 64


@dataclass(frozen=True)
class Case:
    name: str
    owner_id: str | None
    bucket_offset: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate a pgvector HNSW baseline JSON for Proxima ANN work."
    )
    parser.add_argument(
        "--database-url",
        default=os.environ.get(
            "DATABASE_URL", "postgres://proxima:proxima@localhost:5434/proxima"
        ),
        help="Postgres URL. Defaults to DATABASE_URL or the dev compose URL.",
    )
    parser.add_argument("--output", default="bench/ann-baseline.json")
    parser.add_argument(
        "--artifact-kind",
        choices=["baseline", "candidate", "final"],
        default="baseline",
        help="Label for the JSON artifact being generated.",
    )
    parser.add_argument("--dimension", type=int, default=1024)
    parser.add_argument("--hot-rows", type=int, default=900)
    parser.add_argument("--cold-rows", type=int, default=120)
    parser.add_argument("--noise-rows", type=int, default=900)
    parser.add_argument("--noise-owners", type=int, default=6)
    parser.add_argument("--buckets", type=int, default=8)
    parser.add_argument("--queries-per-case", type=int, default=5)
    parser.add_argument("--k", type=int, default=10)
    parser.add_argument("--seed", type=int, default=60604)
    parser.add_argument(
        "--force-hnsw",
        dest="disable_seqscan",
        action=argparse.BooleanOptionalAction,
        default=False,
        help=(
            "Set enable_seqscan=off for ANN probes. The planner may still choose "
            "the owner btree plus sort for selective filters."
        ),
    )
    parser.add_argument(
        "--ef-search",
        type=int,
        default=None,
        help="Optional hnsw.ef_search for candidate experiments. Omit for baseline.",
    )
    parser.add_argument(
        "--iterative-scan",
        choices=["off", "strict_order", "relaxed_order"],
        default=None,
        help="Optional hnsw.iterative_scan for candidate experiments. Omit for baseline.",
    )
    return parser.parse_args()


def psql(
    database_url: str,
    sql: str,
    *,
    input_text: str | None = None,
    single_transaction: bool = False,
) -> str:
    args = [
        "psql",
        database_url,
        "-X",
        "-v",
        "ON_ERROR_STOP=1",
        "-q",
        "-t",
        "-A",
        "-c",
        sql,
    ]
    if single_transaction:
        args.insert(3, "-1")
    proc = subprocess.run(
        args,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"psql failed ({proc.returncode})\nSQL:\n{sql}\nSTDERR:\n{proc.stderr}"
        )
    return proc.stdout.strip()


def vector_literal(vec: list[float]) -> str:
    return "[" + ",".join(f"{value:.6f}" for value in vec) + "]"


def normalize(vec: list[float]) -> list[float]:
    norm = math.sqrt(sum(value * value for value in vec))
    if norm == 0:
        return vec
    return [value / norm for value in vec]


def centers(seed: int, buckets: int, dimension: int) -> list[list[float]]:
    rng = random.Random(seed)
    return [
        normalize([rng.gauss(0.0, 1.0) for _ in range(dimension)])
        for _ in range(buckets)
    ]


def noisy_vector(center: list[float], seed: int, noise: float) -> list[float]:
    rng = random.Random(seed)
    return normalize([value + rng.gauss(0.0, noise) for value in center])


def setup_schema(database_url: str, dimension: int) -> None:
    psql(
        database_url,
        f"""
        CREATE EXTENSION IF NOT EXISTS vector;
        DROP SCHEMA IF EXISTS ann_bench CASCADE;
        CREATE SCHEMA ann_bench;
        CREATE TABLE ann_bench.embeddings (
            id bigint PRIMARY KEY,
            owner_kind text NOT NULL,
            owner_id uuid NOT NULL,
            model_id text NOT NULL,
            bucket integer NOT NULL,
            vec vector({dimension}) NOT NULL
        );
        """,
    )


def owner_for_noise(index: int, noise_owners: int) -> str:
    return f"{NOISE_OWNER_BASE}{index % noise_owners:02d}"


def copy_dataset(args: argparse.Namespace, bucket_centers: list[list[float]]) -> dict[str, int]:
    rows: list[str] = []
    next_id = 1

    def append_rows(count: int, owner_id: str, id_seed: int, noise: float) -> None:
        nonlocal next_id
        for idx in range(count):
            bucket = idx % args.buckets
            vec = noisy_vector(bucket_centers[bucket], args.seed + id_seed + idx, noise)
            rows.append(
                "\t".join(
                    [
                        str(next_id),
                        "tenant",
                        owner_id,
                        MODEL_ID,
                        str(bucket),
                        vector_literal(vec),
                    ]
                )
            )
            next_id += 1

    append_rows(args.hot_rows, HOT_OWNER, 10_000, 0.045)
    append_rows(args.cold_rows, COLD_OWNER, 20_000, 0.045)
    for idx in range(args.noise_rows):
        owner_id = owner_for_noise(idx, args.noise_owners)
        bucket = idx % args.buckets
        vec = noisy_vector(bucket_centers[bucket], args.seed + 30_000 + idx, 0.055)
        rows.append(
            "\t".join(
                [
                    str(next_id),
                    "tenant",
                    owner_id,
                    MODEL_ID,
                    str(bucket),
                    vector_literal(vec),
                ]
            )
        )
        next_id += 1

    copy_sql = """
        COPY ann_bench.embeddings
            (id, owner_kind, owner_id, model_id, bucket, vec)
        FROM STDIN WITH (FORMAT text);
    """
    psql(args.database_url, copy_sql, input_text="\n".join(rows) + "\n")
    psql(
        args.database_url,
        """
        CREATE INDEX ann_bench_owner_idx
            ON ann_bench.embeddings (owner_kind, owner_id);
        CREATE INDEX ann_bench_vec_hnsw
            ON ann_bench.embeddings USING hnsw (vec vector_cosine_ops);
        ANALYZE ann_bench.embeddings;
        """,
    )
    return {
        "hot": args.hot_rows,
        "cold": args.cold_rows,
        "noise": args.noise_rows,
        "total": args.hot_rows + args.cold_rows + args.noise_rows,
    }


def ann_settings(args: argparse.Namespace) -> list[str]:
    settings: list[str] = []
    if args.disable_seqscan:
        settings.append("SET LOCAL enable_seqscan = off")
    if args.ef_search is not None:
        settings.append(f"SET LOCAL hnsw.ef_search = {args.ef_search}")
    if args.iterative_scan is not None:
        settings.append(f"SET LOCAL hnsw.iterative_scan = {args.iterative_scan}")
    return settings


def exact_settings() -> list[str]:
    return [
        "SET LOCAL enable_indexscan = off",
        "SET LOCAL enable_indexonlyscan = off",
        "SET LOCAL enable_bitmapscan = off",
    ]


def candidate_overfetch(k: int) -> int:
    return max(
        MIN_VECTOR_CANDIDATE_OVERFETCH,
        min(k, 50) * VECTOR_CANDIDATE_OVERFETCH_PER_RESULT,
    )


def select_sql(where_sql: str, query_vec: list[float], k: int) -> str:
    overfetch = candidate_overfetch(k)
    return f"""
        WITH candidates AS MATERIALIZED (
            SELECT id, owner_kind, owner_id, model_id
            FROM ann_bench.embeddings
            WHERE {where_sql}
        ),
        eligible_entities AS MATERIALIZED (
            SELECT DISTINCT ON (id) id, owner_kind, owner_id
            FROM candidates
            ORDER BY id
        ),
        vector_candidates AS MATERIALIZED (
            SELECT emb.id
            FROM ann_bench.embeddings emb
            JOIN eligible_entities c
              ON c.id = emb.id
             AND c.owner_kind = emb.owner_kind
             AND c.owner_id IS NOT DISTINCT FROM emb.owner_id
            WHERE emb.model_id = 'ann-bench-v1'
            ORDER BY emb.vec <=> '{vector_literal(query_vec)}'::vector
            LIMIT {overfetch}
        )
        SELECT id
        FROM vector_candidates
        LIMIT {k}
    """


def transactional(sql: str, settings: list[str]) -> str:
    body = ";\n".join([*settings, sql])
    return body + ";"


def artifact_notes(kind: str) -> list[str]:
    if kind == "baseline":
        return [
            "No schema change is implied by this artifact.",
            "Iterative-scan, ef_search, halfvec, and partition decisions require benchmark review.",
        ]
    if kind == "final":
        return [
            "No schema change is implied by this artifact.",
            "Final artifact records the selected query-local HNSW settings.",
        ]
    return [
        "No schema change is implied by this artifact.",
        "Candidate artifact is evidence only until reviewed.",
    ]


def path_description(kind: str) -> str:
    if kind == "baseline":
        return "current planner over shared HNSW and owner btree indexes"
    return "planner with configured query-local HNSW settings over shared HNSW and owner btree indexes"


def ids_for(
    database_url: str,
    where_sql: str,
    query_vec: list[float],
    k: int,
    settings: list[str],
) -> list[int]:
    out = psql(
        database_url,
        transactional(select_sql(where_sql, query_vec, k), settings),
        single_transaction=True,
    )
    out = strip_command_tags(out)
    if not out:
        return []
    return [int(line) for line in out.splitlines() if line.strip()]


def explain_for(
    database_url: str,
    where_sql: str,
    query_vec: list[float],
    k: int,
    settings: list[str],
) -> dict[str, Any]:
    explain_sql = "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) " + select_sql(
        where_sql, query_vec, k
    )
    out = psql(database_url, transactional(explain_sql, settings), single_transaction=True)
    out = strip_command_tags(out)
    return json.loads(out)[0]


def strip_command_tags(out: str) -> str:
    return "\n".join(
        line for line in out.splitlines() if line.strip() not in PSQL_COMMAND_TAGS
    ).strip()


def walk_plan(node: dict[str, Any]) -> list[dict[str, Any]]:
    nodes = [node]
    for child in node.get("Plans", []):
        nodes.extend(walk_plan(child))
    return nodes


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = (len(ordered) - 1) * pct
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return ordered[low]
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def summarize_case(
    args: argparse.Namespace,
    case: Case,
    bucket_centers: list[list[float]],
) -> dict[str, Any]:
    where_sql = "model_id = 'ann-bench-v1'"
    if case.owner_id is not None:
        where_sql += f" AND owner_kind = 'tenant' AND owner_id = '{case.owner_id}'::uuid"

    recalls: list[float] = []
    latencies: list[float] = []
    shared_hits: list[int] = []
    shared_reads: list[int] = []
    index_names: set[str] = set()
    node_types: set[str] = set()

    for idx in range(args.queries_per_case):
        bucket = (idx + case.bucket_offset) % args.buckets
        query_vec = noisy_vector(
            bucket_centers[bucket],
            args.seed + 90_000 + case.bucket_offset * 100 + idx,
            0.025,
        )
        exact_ids = ids_for(
            args.database_url,
            where_sql,
            query_vec,
            args.k,
            exact_settings(),
        )
        ann_ids = ids_for(
            args.database_url,
            where_sql,
            query_vec,
            args.k,
            ann_settings(args),
        )
        exact_set = set(exact_ids)
        recall = len(exact_set.intersection(ann_ids)) / max(1, len(exact_set))
        recalls.append(recall)

        explain = explain_for(
            args.database_url,
            where_sql,
            query_vec,
            args.k,
            ann_settings(args),
        )
        plan = explain["Plan"]
        latencies.append(float(explain.get("Execution Time", 0.0)))
        for node in walk_plan(plan):
            node_type = node.get("Node Type")
            if node_type:
                node_types.add(node_type)
            index_name = node.get("Index Name")
            if index_name:
                index_names.add(index_name)
            shared_hits.append(int(node.get("Shared Hit Blocks", 0)))
            shared_reads.append(int(node.get("Shared Read Blocks", 0)))

    return {
        "name": case.name,
        "filter": "all owners" if case.owner_id is None else f"owner_id = {case.owner_id}",
        "queries": args.queries_per_case,
        "recall_at_k": {
            "k": args.k,
            "avg": round(statistics.fmean(recalls), 4),
            "min": round(min(recalls), 4),
        },
        "latency_ms": {
            "p50": round(percentile(latencies, 0.50), 4),
            "p95": round(percentile(latencies, 0.95), 4),
        },
        "planner": {
            "index_names": sorted(index_names),
            "node_types": sorted(node_types),
            "hnsw_index_used": "ann_bench_vec_hnsw" in index_names,
        },
        "buffers": {
            "shared_hits_avg": round(statistics.fmean(shared_hits), 2) if shared_hits else 0.0,
            "shared_reads_avg": round(statistics.fmean(shared_reads), 2) if shared_reads else 0.0,
        },
    }


def scalar(database_url: str, sql: str) -> str:
    return psql(database_url, sql).splitlines()[0]


def sizes(database_url: str) -> dict[str, int]:
    out = psql(
        database_url,
        """
        SELECT
            pg_relation_size('ann_bench.embeddings')::bigint,
            pg_relation_size('ann_bench.ann_bench_vec_hnsw')::bigint,
            pg_total_relation_size('ann_bench.embeddings')::bigint
        """,
    )
    table_size, index_size, total_size = [int(part) for part in out.split("|")]
    return {
        "table_bytes": table_size,
        "hnsw_index_bytes": index_size,
        "total_relation_bytes": total_size,
    }


def redacted_url(database_url: str) -> str:
    parsed = urlsplit(database_url)
    netloc = parsed.netloc
    if "@" in netloc:
        _, host = netloc.rsplit("@", 1)
        netloc = f"***:***@{host}"
    return urlunsplit((parsed.scheme, netloc, parsed.path, parsed.query, parsed.fragment))


def main() -> int:
    args = parse_args()
    started = time.perf_counter()
    bucket_centers = centers(args.seed, args.buckets, args.dimension)

    setup_schema(args.database_url, args.dimension)
    row_counts = copy_dataset(args, bucket_centers)

    cases = [
        Case("unfiltered", None, 0),
        Case("owner_filtered_hot", HOT_OWNER, 1),
        Case("owner_filtered_cold", COLD_OWNER, 2),
    ]
    case_results = [summarize_case(args, case, bucket_centers) for case in cases]

    storage_sizes = sizes(args.database_url)
    result = {
        "schema_version": 1,
        "benchmark": f"proxima-ann-hnsw-{args.artifact_kind}",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "database": {
            "url": redacted_url(args.database_url),
            "postgres_version": scalar(args.database_url, "SHOW server_version"),
            "pgvector_version": scalar(
                args.database_url,
                "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
            ),
        },
        "dataset": {
            "dimension": args.dimension,
            "row_counts": row_counts,
            "owner_shape": {
                "hot_owner": HOT_OWNER,
                "cold_owner": COLD_OWNER,
                "noise_owners": args.noise_owners,
            },
            "buckets": args.buckets,
            "seed": args.seed,
        },
        "baseline_path": {
            "description": path_description(args.artifact_kind),
            "disable_seqscan_for_ann": args.disable_seqscan,
            "ef_search": args.ef_search,
            "iterative_scan": args.iterative_scan,
            "query_shape": "production-style candidates -> eligible_entities -> vector_candidates CTE with production overfetch",
            "vector_candidate_overfetch": candidate_overfetch(args.k),
        },
        "storage": storage_sizes,
        "memory_residency_proxy": {
            "source": "relation size plus EXPLAIN buffer hits/reads",
            "hnsw_index_bytes": storage_sizes["hnsw_index_bytes"],
        },
        "cases": case_results,
        "elapsed_seconds": round(time.perf_counter() - started, 3),
        "cp0": {
            "status": args.artifact_kind,
            "notes": artifact_notes(args.artifact_kind),
        },
    }

    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    with open(args.output, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(args.output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"ann_bench.py: {exc}", file=sys.stderr)
        raise SystemExit(1)
