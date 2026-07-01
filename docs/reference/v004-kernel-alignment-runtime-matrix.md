# v0.0.4 Kernel Alignment Runtime Matrix

Status map for post-PR9 runtime checks. Kernel authority remains
`docs/lean/Causa`; this page lists Rust/runtime evidence only.

| Area | Kernel rule | Runtime surface | Evidence / ratchet |
|---|---|---|---|
| Compliance erase | hard deletion only after abandonment proof | `Engine::erase_abandoned_group_owner`, `Engine::erase_dropped_personal_owner`, source-scope variants; `ComplianceErase*` Host API DTOs | `crates/core/tests/compliance_kernel_contract.rs`; `crates/storage-pg/tests/integration/compliance_erase_pg.rs`; `scripts/check-pr9-ratchets.py` witness/audit-field checks |
| Compliance audit/suppression | audit survives deletion; suppression blocks re-ingest | `proxima_core.compliance_audit`, `proxima_core.compliance_suppression_keys` | storage integration tests; SQL policy ratchet for deletion fan-out |
| Edge target redaction | live source-owned edge is not deleted by abandoned target | projection metadata in `compliance_edge_target_redactions`; read projection redacts target | compliance erase PG tests; relation descriptor tests |
| Embedding independence | embeddings never author graph edges; entity identity is independent | append-only `embeddings` rows plus `embedding_heads`; no entity-table FK to embeddings | `crates/storage-pg/tests/integration/fact_embeddings_pg.rs`; docs [07 Vector Store](../07-storage.md#vector-store--independent) |
| Keyset performance | reads use bounded set-based owner authorization | single-kind `QueryCursor`; no mixed-query cursor; `(created_at, id)` keyset predicates | `crates/storage-pg/tests/integration/query_perf_pg.rs`; `crates/core/tests/query_verb.rs` |
| Owner read batching | no row-by-row owner resolution | storage reads receive owner arrays and join `unnest(owner_kind[], owner_id[])` | query PG/perf tests; `scripts/check-sql-policy.py` inventory |
| Protocol constants | duplicated tool/resource strings do not drift | `proxima_core::protocol` constants consumed by MCP/selfdoc/profile code | core/MCP tests; `scripts/check-pr9-ratchets.py` stale production-code tombstone/source-identity checks |
| Typed errors | public error codes serialize through one vocabulary | `ErrorCode::as_str`; non-exhaustive error enum | `crates/core/src/error.rs` tests; rustdoc `-D warnings` |
| Public API tiers | Host API root facade; Flavor SDK under `proxima::flavor` | `crates/proxima/src/host.rs`, `crates/proxima/src/flavor.rs` | `cargo test -p proxima --test public_api_tiers --locked`; [Public API](public-api.md) |
| Raw storage denial | flavors cannot depend on stable raw core DB access | no stable `PgPool`/aggregate `Storage`; flavor raw `proxima_core.*` SQL denied | `scripts/check-pr9-ratchets.py`; `crates/proxima/tests/public_api_tiers.rs` |
| SQL policy | dynamic SQL must prove validated identifiers, fixed fragments, or bound values | `scripts/check-sql-policy.py` exact inventory and unsafe fixture | `python3 scripts/check-sql-policy.py`; `python3 scripts/check-sql-policy.py --fixture scripts/fixtures/sql-policy/unsafe_dynamic_sql.rs` |
| CI ratchets | relapse checks block merges | CI docs job runs PR9 and SQL ratchets | `.github/workflows/ci.yml` |

PR9 local evidence:

| Artifact | Path |
|---|---|
| API diff summary | `.local/architecture-restoration/pr9-public-api-diff.md` |
| ignored API snapshots | `.local/architecture-restoration/api/` |
