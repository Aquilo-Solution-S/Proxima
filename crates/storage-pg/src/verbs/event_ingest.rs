//! `EventIngest` verb — atomic insert of `cited_object`, `source_batch`,
//! `event`, `memory`, `citation_mapping`, and `change_event` rows.
//!
//! Replay is detected by the `(event_id)` unique on `memories`; the
//! caller observes `idempotent_replay = true` and the original
//! `change_event_seq`.
//!
//! [`ingest_event_in_tx`] exposes the same body inside an existing
//! transaction so flavor crates can append a typed sidecar row
//! atomically with the Fact materialization (M3.B.5+). The pool-level
//! [`ingest_event_atomic`] is a thin wrapper that opens its own tx.

use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::{Principal, StorageError};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

/// Pool-scoped `EventIngest`. Opens its own transaction; commits on
/// success.
///
/// # Errors
///
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
pub async fn ingest_event_atomic(
    pool: &PgPool,
    draft: &EventDraft,
) -> Result<EventIngestOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    let outcome = ingest_event_in_tx(&mut tx, draft).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Run the `EventIngest` body inside an already-open transaction. The
/// caller owns `tx` and is responsible for committing or rolling back.
/// Flavors use this to bundle the typed sidecar insert with the core
/// Fact materialization in a single atomic write.
///
/// # Errors
///
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
#[allow(clippy::too_many_lines)]
pub async fn ingest_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &EventDraft,
) -> Result<EventIngestOutcome, StorageError> {
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();

    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    // Replay check.
    let existing: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT memory_id FROM proxima_core.memories WHERE event_id = $1")
            .bind(&event_id_bytes[..])
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;

    if let Some((memory_id,)) = existing {
        let seq_row: (uuid::Uuid,) = sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;

        return Ok(EventIngestOutcome {
            event_id,
            memory_id: proxima_core::MemoryId::new(memory_id),
            change_event_seq: seq_row.0,
            idempotent_replay: true,
        });
    }

    // Generate ids inside the tx; UUIDv7 carries time so seq
    // is monotonic-ish even across concurrent writers.
    let memory_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let cited_object_id_new = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    // 1. cited_object — idempotent on the UNIQUE.
    let cited_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects \
            (cited_object_id, schema_id, owner_principal_kind, \
             owner_principal_id, owner_org_id, content_hash) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, \
                      owner_org_id, schema_id, content_hash) \
         DO UPDATE SET schema_id = EXCLUDED.schema_id \
         RETURNING cited_object_id",
    )
    .bind(cited_object_id_new)
    .bind(draft.cited_object.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&draft.cited_object.content_hash[..])
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;

    // 2. source_batch upsert (idempotent on PK). Must come before
    //    event insert due to FK from events.source_batch_id.
    sqlx::query(
        "INSERT INTO proxima_core.source_batches \
            (id, source_id, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 3. event — collision = replay. We already short-circuited
    //    the replay path above, so a conflict here means a race.
    //    Treat as Internal (caller can retry).
    sqlx::query(
        "INSERT INTO proxima_core.events \
            (event_id, source_id, source_batch_id, \
             owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, observed_at, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&event_id_bytes[..])
    .bind(draft.source_id.as_str())
    .bind(draft.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(draft.observed_at)
    .bind(draft.occurred_at)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 4. memory (Fact) — citation_mapping_id FK is deferred.
    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, \
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 5. citation_mapping — memory_id FK is deferred.
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings \
            (citation_mapping_id, schema_id, memory_id, \
             cited_object_id, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(draft.citation_mapping.schema_id.as_str())
    .bind(memory_id)
    .bind(cited_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 6. change_event (EntityAppend / Fact).
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, \
             owner_org_id, kind, entity_kind, \
             entity_memory_id, entity_schema_id, \
             entity_schema_version) \
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(EventIngestOutcome {
        event_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}
