# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

## Supported Tiers

Post-PR9 supported Rust tiers:

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, RuntimeConfig, Engine};` | boot composed binaries; call graph/admin verbs through server-resolved `AuthzContext` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | build-time schemas, relations, tools, sidecars |

Unsupported:

| Surface | Status |
|---|---|
| raw `sqlx::PgPool` | not stable Host API or Flavor SDK |
| aggregate `Storage` / `StorageHandle` | removed; Engine owns storage ports |
| flavor raw SQL against `proxima_core.*` | denied by `scripts/check-architecture-guardrails.py` |
| runtime plugin/tool/schema registration | denied; flavor composition is build-time |

Machine checks:

| Check | Command |
|---|---|
| import tiers | `cargo test -p proxima --test public_api_tiers --locked` |
| architecture ratchets | `python3 scripts/check-architecture-guardrails.py` |
| SQL policy ratchet | `python3 scripts/check-sql-policy.py` |

## Compliance Erase Host API

Public facade status:

| Type | Import | Status |
|---|---|---|
| `ComplianceEraseRequest` | `proxima::ComplianceEraseRequest` | Host API DTO |
| `ComplianceEraseTarget` | `proxima::ComplianceEraseTarget` | Host API DTO |
| `ComplianceEraseOutcome` | `proxima::ComplianceEraseOutcome` | Host API DTO |
| `ComplianceEraseRefusal` | `proxima::ComplianceEraseRefusal` | Host API DTO |
| `ComplianceEraseCounts` | `proxima::ComplianceEraseCounts` | Host API DTO |

Callers submit requests and inspect outcomes. Callers do not provide
`operation_id`, requester, auth path, request time, audit context, or
abandonment witnesses. Engine derives audit identity from `AuthzContext`,
verifies personal-owner drop proof before minting sealed erase authorization,
and storage rechecks group abandonment in-transaction before hard deletion.
All storage erase paths require sealed `EraseAuthorization` (see
[13 Compliance](../13-compliance.md) and
[14 Compliance Admin Surface](../14-protocol-surface.md#compliance-admin-surface)).

## Embeddings

Embedding contract:

| Item | Contract |
|---|---|
| host wiring | host injects `proxima::llm::EmbeddingClient`; no inference target registry |
| entity tables | no FK from entity rows to embeddings |
| write semantics | re-embedding appends a new `(entity_kind, entity_id, embedding_version, model_id)` row |
| latest pointer | `embedding_heads` metadata, rebuildable from `embeddings` |
| graph authority | similarity is query-time evidence only; embeddings never author edges |

See [07 Vector Store - Independent](../07-storage.md#vector-store--independent).

## Generated Rustdoc

Build locally:

```sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked --open
```

CI treats rustdoc warnings as failures.

## Local API Diff Evidence

Install `cargo-public-api` outside tracked source if missing:

```sh
cargo install cargo-public-api --locked --root /tmp/codex-cargo-public-api
```

Generate ignored snapshots:

```sh
mkdir -p .local/architecture-restoration/api
cargo +nightly public-api -p proxima --all-features > .local/architecture-restoration/api/pr9-proxima-public-api.txt
cargo +nightly public-api -p proxima-core --all-features > .local/architecture-restoration/api/pr9-proxima-core-public-api.txt
cargo +nightly public-api -p proxima-storage-pg --all-features > .local/architecture-restoration/api/pr9-proxima-storage-pg-public-api.txt
cargo +nightly public-api -p proxima-code --all-features > .local/architecture-restoration/api/pr9-proxima-code-public-api.txt
```

Summarize reviewer evidence in:

```text
.local/architecture-restoration/pr9-public-api-diff.md
```

Do not track generated API snapshots unless a release process requests a
baseline.
