//! `EventIngest` verb — atomic insert of optional citation rows,
//! `source_batch`, `event`, `memory`, and `change_event` rows.
//!
//! Replay is detected by the `(event_id)` unique on `memories`; the
//! caller observes `idempotent_replay = true` and the original
//! `change_event_seq`.
//!
//! [`ingest_event_in_tx`] exposes the same body inside an existing
//! transaction so flavor crates can append a typed sidecar row
//! atomically with the Fact materialization (M3.B.5+). The pool-level
//! [`ingest_event_atomic`] is a thin wrapper that opens its own tx.

use std::future::Future;
use std::pin::Pin;

use proxima_core::verbs::event_ingest::{
    AuthorizedCitationAttachment, AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject,
    EventDraft, EventIngestOutcome,
};
use proxima_core::{
    AuthorizedEventIngest, AuthorizedFactWithCitation, AuthzContext, Engine, EntityKind,
    FactPayload, MemoryId, Owner, OwnerPrincipalKind, Role, SchemaVersion, SourceBatchId, SourceId,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err};
use crate::pg_ident::PgIdent;

pub type EventIngestSidecarFuture<'t> =
    Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;

#[derive(Debug, Clone)]
pub struct AttachCitationOutcome {
    pub memory_id: MemoryId,
    pub cited_object_id: uuid::Uuid,
    pub attached: bool,
    pub idempotent: bool,
}

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
    embedding_model_id: Option<&str>,
) -> Result<EventIngestOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = ingest_event_in_tx(&mut tx, draft, embedding_model_id).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Pool-scoped gated `EventIngest` with a caller-owned typed sidecar
/// insert. Opens its own transaction; commits only after the Fact and
/// sidecar both succeed.
///
/// # Errors
///
/// Returns storage errors from Fact materialization, sidecar insertion,
/// or transaction commit. A sidecar error rolls back the transaction.
pub async fn event_ingest_with_sidecar_atomic<F>(
    pool: &PgPool,
    authorized: &AuthorizedEventIngest,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome =
        ingest_event_with_sidecar_in_tx(&mut tx, authorized, embedding_model_id, sidecar).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Pool-scoped gated Fact ingest with typed inline citation sidecars.
/// Opens its own transaction; commits only after all core rows and
/// caller/registry sidecars succeed.
///
/// # Errors
///
/// Returns storage errors from Fact materialization, cited-object
/// sidecar insertion, Fact sidecar insertion, citation-mapping
/// sidecar insertion, or transaction commit.
pub async fn ingest_fact_with_citation_atomic<F>(
    pool: &PgPool,
    authorized: &AuthorizedFactWithCitation,
    embedding_model_id: Option<&str>,
    fact_sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome =
        ingest_fact_with_citation_in_tx(&mut tx, authorized, embedding_model_id, fact_sidecar)
            .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Build an uncited Fact draft from a typed payload, authorize it, and
/// materialize it plus the caller-owned sidecar inside an existing tx.
///
/// # Errors
///
/// Returns storage errors from JSON serialization, authorization/schema
/// validation, Fact materialization, or sidecar insertion.
pub async fn ingest_fact_in_tx<P, F>(
    tx: &mut Transaction<'_, Postgres>,
    engine: &Engine,
    authz: &AuthzContext,
    role: Role,
    payload: &P,
    sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let payload_value = serde_json::to_value(payload).map_err(internal)?;
    let payload_bytes = proxima_core::canonical_json_bytes(&payload_value);
    let now = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new("proxima/fact"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        principal: authz.identity.principal.clone(),
        org_id: None,
        author_personality_instance_id: None,
        schema_id: P::schema_id(),
        schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
        payload: payload_bytes,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: None,
    };
    let authorized = engine
        .authorize_event_ingest(authz, role, draft)
        .map_err(internal)?;
    let embedding_client = engine.embed_client();
    let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
    ingest_event_with_sidecar_in_tx(tx, &authorized, embedding_model_id, sidecar).await
}

/// Pool-scoped uncited Fact ingest helper. Opens its own transaction;
/// commits only after the Fact and sidecar both succeed.
///
/// # Errors
///
/// Returns storage errors from transaction setup, Fact ingest, sidecar
/// insertion, or transaction commit.
pub async fn ingest_fact<P, F>(
    pool: &PgPool,
    engine: &Engine,
    authz: &AuthzContext,
    role: Role,
    payload: &P,
    sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = ingest_fact_in_tx(&mut tx, engine, authz, role, payload, sidecar).await?;
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
    embedding_model_id: Option<&str>,
) -> Result<EventIngestOutcome, StorageError> {
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let owner = draft.owner();

    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();

    // Replay check.
    let existing = sqlx::query_scalar::<_, uuid::Uuid>(
        r"SELECT memory_id FROM proxima_core.memories
           WHERE event_id = $1
             AND tombstoned_at IS NULL",
    )
    .bind(&event_id_bytes[..])
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if let Some(memory_id) = existing {
        let seq = sqlx::query_scalar!(
            r#"SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1"#,
            memory_id,
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;

        return Ok(EventIngestOutcome {
            event_id,
            memory_id: proxima_core::MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    // Generate ids inside the tx; UUIDv7 carries time so seq
    // is monotonic-ish even across concurrent writers.
    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    // 1. cited_object — optional; idempotent on the UNIQUE when present.
    let citation_refs = if let Some(citation) = &draft.citation {
        let citation_mapping_id = uuid::Uuid::now_v7();
        let cited_object_id_new = uuid::Uuid::now_v7();
        let cited_id: uuid::Uuid = sqlx::query_scalar!(
            r#"INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, content_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      owner_org_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id"#,
            cited_object_id_new,
            citation.object.schema_id.as_str(),
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
            owner_org_id,
            &citation.object.content_hash[..],
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
        Some((citation, citation_mapping_id, cited_id))
    } else {
        None
    };
    let citation_mapping_id = citation_refs
        .as_ref()
        .map(|(_, citation_mapping_id, _)| *citation_mapping_id);

    // 2. source_batch upsert (idempotent on PK). Must come before
    //    event insert due to FK from events.source_batch_id.
    sqlx::query!(
        r#"INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind,
             owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING"#,
        draft.source_batch_id.into_inner(),
        draft.source_id.as_str(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 3. event — collision = replay. We already short-circuited
    //    the replay path above, so a conflict here means a race.
    //    Treat as Internal (caller can retry).
    sqlx::query!(
        r#"INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        &event_id_bytes[..],
        draft.source_id.as_str(),
        draft.source_batch_id.into_inner(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        draft.schema_id.as_str(),
        draft.schema_version.into_inner().cast_signed(),
        draft.observed_at,
        draft.occurred_at,
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    let author_personality_instance_id = draft.author_personality_instance_id.map_or_else(
        uuid::Uuid::nil,
        proxima_core::PersonalityInstanceId::into_inner,
    );

    // 4. memory (Fact) — citation_mapping_id FK is deferred when present.
    //    Nil marks non-personality authoring.
    sqlx::query(
        r"INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             text, personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 $9, $10)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .bind(draft.rendered_text.as_deref())
    .bind(author_personality_instance_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    enqueue_embedding_job_in_tx(
        tx,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        EntityKind::Fact,
        memory_id,
        embedding_model_id,
    )
    .await?;

    // 5. citation_mapping — optional; memory_id FK is deferred when present.
    if let Some((citation, citation_mapping_id, cited_id)) = citation_refs {
        sqlx::query!(
            r#"INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_principal_kind,
             owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            citation_mapping_id,
            citation.mapping.schema_id.as_str(),
            memory_id,
            cited_id,
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
            owner_org_id,
        )
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }

    // 6. change_event (EntityAppend / Fact).
    sqlx::query!(
        r#"INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id,
             owner_org_id, kind, entity_kind,
             entity_memory_id, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, $7)"#,
        change_seq,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        memory_id,
        draft.schema_id.as_str(),
        draft.schema_version.into_inner().cast_signed(),
    )
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

/// Run gated Fact ingest plus typed inline citation sidecars inside an
/// already-open transaction.
///
/// # Errors
///
/// Returns storage errors from core row materialization, caller-owned
/// Fact sidecar insertion, cited-object sidecar insertion, or
/// citation-mapping sidecar insertion. The caller owns transaction
/// rollback/commit.
#[allow(clippy::too_many_lines)]
pub async fn ingest_fact_with_citation_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedFactWithCitation,
    embedding_model_id: Option<&str>,
    fact_sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let draft = authorized.draft();
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let owner = draft.owner();
    let cited_object = authorized.cited_object();
    let mapping = authorized.mapping();

    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();

    let existing = sqlx::query_scalar::<_, uuid::Uuid>(
        r"SELECT memory_id FROM proxima_core.memories
           WHERE event_id = $1
             AND tombstoned_at IS NULL",
    )
    .bind(&event_id_bytes[..])
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    if let Some(memory_id) = existing {
        let seq = sqlx::query_scalar::<_, uuid::Uuid>(
            r"SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

        return Ok(EventIngestOutcome {
            event_id,
            memory_id: proxima_core::MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let citation_mapping_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    let author_personality_instance_id = authorized.author_personality_instance_id().map_or_else(
        uuid::Uuid::nil,
        proxima_core::PersonalityInstanceId::into_inner,
    );

    let cited_object_id = upsert_cited_object_in_tx(tx, &owner, cited_object).await?;

    sqlx::query(
        r"INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind,
             owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
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
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             text, personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .bind(draft.rendered_text.as_deref())
    .bind(author_personality_instance_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let outcome = EventIngestOutcome {
        event_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    };
    fact_sidecar(tx, &outcome).await?;
    derive_fact_entity_after_sidecar(
        tx,
        &owner,
        draft,
        memory_id,
        authorized.fact_sidecar_table(),
        authorized.fact_natural_key_columns(),
    )
    .await?;
    enqueue_embedding_job_in_tx(
        tx,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        EntityKind::Fact,
        memory_id,
        embedding_model_id,
    )
    .await?;

    insert_citation_mapping_in_tx(
        tx,
        &owner,
        mapping,
        citation_mapping_id,
        memory_id,
        cited_object_id,
    )
    .await?;

    sqlx::query(
        r"INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id,
             owner_org_id, kind, entity_kind,
             entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_instance_id)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, $7, $8)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(author_personality_instance_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(outcome)
}

/// Attach a typed inline citation to an existing uncited Fact inside an
/// already-open transaction.
///
/// # Errors
///
/// Returns `NotFound` when the memory is absent for the owner, tombstoned,
/// already derived, or owner-mismatched. Returns storage errors from
/// cited-object sidecar insertion, citation-mapping sidecar insertion, or
/// core row updates. The caller owns transaction rollback/commit.
pub async fn attach_citation_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedCitationAttachment,
) -> Result<AttachCitationOutcome, StorageError> {
    let memory_id = authorized.memory_id();
    let memory_uuid = memory_id.into_inner();
    let owner = authorized.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();

    let existing_mapping_id = sqlx::query_scalar::<_, Option<uuid::Uuid>>(
        r"SELECT citation_mapping_id
            FROM proxima_core.memories
           WHERE memory_id = $1
             AND owner_principal_kind = $2
             AND owner_principal_id = $3
             AND owner_org_id = $4
             AND kind IS NULL
             AND tombstoned_at IS NULL",
    )
    .bind(memory_uuid)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;

    if let Some(citation_mapping_id) = existing_mapping_id {
        let cited_object_id =
            cited_object_id_for_mapping_in_tx(tx, owner, memory_uuid, citation_mapping_id).await?;
        return Ok(AttachCitationOutcome {
            memory_id,
            cited_object_id,
            attached: false,
            idempotent: true,
        });
    }

    let cited_object_id = upsert_cited_object_in_tx(tx, owner, authorized.cited_object()).await?;
    let citation_mapping_id = uuid::Uuid::now_v7();
    insert_citation_mapping_in_tx(
        tx,
        owner,
        authorized.mapping(),
        citation_mapping_id,
        memory_uuid,
        cited_object_id,
    )
    .await?;

    let updated = sqlx::query(
        r"UPDATE proxima_core.memories
             SET citation_mapping_id = $2
           WHERE memory_id = $1
             AND owner_principal_kind = $3
             AND owner_principal_id = $4
             AND owner_org_id = $5
             AND citation_mapping_id IS NULL",
    )
    .bind(memory_uuid)
    .bind(citation_mapping_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    if updated.rows_affected() != 1 {
        return Err(StorageError::Conflict(
            "citation was attached concurrently".to_string(),
        ));
    }

    Ok(AttachCitationOutcome {
        memory_id,
        cited_object_id,
        attached: true,
        idempotent: false,
    })
}

async fn cited_object_id_for_mapping_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    memory_id: uuid::Uuid,
    citation_mapping_id: uuid::Uuid,
) -> Result<uuid::Uuid, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    sqlx::query_scalar::<_, uuid::Uuid>(
        r"SELECT cm.cited_object_id
            FROM proxima_core.citation_mappings cm
           WHERE cm.citation_mapping_id = $1
             AND cm.memory_id = $2
             AND cm.owner_principal_kind = $3
             AND cm.owner_principal_id = $4
             AND cm.owner_org_id = $5",
    )
    .bind(citation_mapping_id)
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or_else(|| {
        StorageError::Internal(format!(
            "Fact memory {memory_id} references missing citation mapping {citation_mapping_id}",
        ))
    })
}

async fn upsert_cited_object_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    cited_object: &AuthorizedInlineCitedObject,
) -> Result<uuid::Uuid, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let cited_object_id_new = uuid::Uuid::now_v7();
    let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
        r"INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, content_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      owner_org_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id_new)
    .bind(cited_object.schema_id().as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&cited_object.content_hash()[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    (cited_object.sidecar_inserter_fn())(tx, cited_object_id, cited_object.payload_bytes()).await?;
    Ok(cited_object_id)
}

async fn insert_citation_mapping_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    mapping: &AuthorizedInlineCitationMapping,
    citation_mapping_id: uuid::Uuid,
    memory_id: uuid::Uuid,
    cited_object_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    sqlx::query(
        r"INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_principal_kind,
             owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(mapping.schema_id().as_str())
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    // A pure-link mapping has no sidecar — the link is fully captured by
    // the citation_mappings row above, so there's nothing more to write.
    if let Some(insert_sidecar) = mapping.sidecar_inserter_fn() {
        insert_sidecar(tx, citation_mapping_id, mapping.payload_bytes()).await?;
    }
    Ok(())
}

/// Run gated `EventIngest` plus a typed sidecar insert inside an
/// already-open transaction. The witness proves the draft passed core
/// authorization, owner stamping, and schema validation before storage
/// saw it.
///
/// # Errors
///
/// Returns storage errors from Fact materialization or sidecar
/// insertion. The caller owns transaction rollback/commit.
#[allow(clippy::too_many_lines)]
pub async fn ingest_event_with_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedEventIngest,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<EventIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t EventIngestOutcome,
    ) -> EventIngestSidecarFuture<'t>,
{
    let draft = authorized.draft();
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let owner = draft.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();

    let existing = sqlx::query_scalar::<_, uuid::Uuid>(
        r"SELECT memory_id FROM proxima_core.memories
           WHERE event_id = $1
             AND tombstoned_at IS NULL",
    )
    .bind(&event_id_bytes[..])
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    if let Some(memory_id) = existing {
        let seq = sqlx::query_scalar::<_, uuid::Uuid>(
            r"SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

        return Ok(EventIngestOutcome {
            event_id,
            memory_id: proxima_core::MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    let citation_refs = if let Some(citation) = &draft.citation {
        let citation_mapping_id = uuid::Uuid::now_v7();
        let cited_object_id_new = uuid::Uuid::now_v7();
        let cited_id = sqlx::query_scalar::<_, uuid::Uuid>(
            r"INSERT INTO proxima_core.cited_objects
                (cited_object_id, schema_id, owner_principal_kind,
                 owner_principal_id, owner_org_id, content_hash)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (owner_principal_kind, owner_principal_id,
                          owner_org_id, schema_id, content_hash)
             DO UPDATE SET schema_id = EXCLUDED.schema_id
             RETURNING cited_object_id",
        )
        .bind(cited_object_id_new)
        .bind(citation.object.schema_id.as_str())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&citation.object.content_hash[..])
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
        Some((citation, citation_mapping_id, cited_id))
    } else {
        None
    };
    let citation_mapping_id = citation_refs
        .as_ref()
        .map(|(_, citation_mapping_id, _)| *citation_mapping_id);

    sqlx::query(
        r"INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind,
             owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
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
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let author_personality_instance_id = draft.author_personality_instance_id.map_or_else(
        uuid::Uuid::nil,
        proxima_core::PersonalityInstanceId::into_inner,
    );

    sqlx::query(
        r"INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             text, personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 $9, $10)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .bind(draft.rendered_text.as_deref())
    .bind(author_personality_instance_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let outcome = EventIngestOutcome {
        event_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    };

    sidecar(tx, &outcome).await?;
    derive_fact_entity_after_sidecar(
        tx,
        &owner,
        draft,
        memory_id,
        authorized.fact_sidecar_table(),
        authorized.fact_natural_key_columns(),
    )
    .await?;
    enqueue_embedding_job_in_tx(
        tx,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        EntityKind::Fact,
        memory_id,
        embedding_model_id,
    )
    .await?;

    if let Some((citation, citation_mapping_id, cited_id)) = citation_refs {
        sqlx::query(
            r"INSERT INTO proxima_core.citation_mappings
                (citation_mapping_id, schema_id, memory_id,
                 cited_object_id, owner_principal_kind,
                 owner_principal_id, owner_org_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(citation_mapping_id)
        .bind(citation.mapping.schema_id.as_str())
        .bind(memory_id)
        .bind(cited_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    }

    sqlx::query(
        r"INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id,
             owner_org_id, kind, entity_kind,
             entity_memory_id, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(outcome)
}

async fn derive_fact_entity_after_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &EventDraft,
    memory_id: uuid::Uuid,
    sidecar_table: Option<&str>,
    natural_key_columns: &[String],
) -> Result<(), StorageError> {
    if natural_key_columns.is_empty() {
        return Ok(());
    }

    let sidecar_table = sidecar_table.ok_or_else(|| {
        StorageError::Internal(format!(
            "stateful Fact schema {} v{} has no sidecar table",
            draft.schema_id.as_str(),
            draft.schema_version.into_inner(),
        ))
    })?;
    let sidecar_table = PgIdent::table(sidecar_table)?;
    let natural_key_exprs = natural_key_columns
        .iter()
        .map(|column| PgIdent::column(column).map(|ident| format!("s.{}", ident.as_str())))
        .collect::<Result<Vec<_>, _>>()?;
    let natural_key_sql = format!(
        "SELECT jsonb_build_array({}) AS natural_key, m.created_at
           FROM {} s
           JOIN proxima_core.memories m ON m.memory_id = s.memory_id
          WHERE s.memory_id = $1",
        natural_key_exprs.join(", "),
        sidecar_table.as_str(),
    );
    let Some((natural_key, created_at)) =
        sqlx::query_as::<_, (serde_json::Value, time::OffsetDateTime)>(&natural_key_sql)
            .bind(memory_id)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?
    else {
        return Err(StorageError::Internal(format!(
            "missing sidecar row in {} for Fact memory {memory_id}",
            sidecar_table.as_str(),
        )));
    };

    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let fact_entity_id = sqlx::query_scalar::<_, uuid::Uuid>(
        r"INSERT INTO proxima_core.fact_entities
            (fact_entity_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, natural_key, current_memory_id, current_created_at)
         VALUES
            ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      schema_id, schema_version, natural_key)
         DO UPDATE
             SET current_memory_id = EXCLUDED.current_memory_id,
                 current_created_at = EXCLUDED.current_created_at
             WHERE (EXCLUDED.current_created_at, EXCLUDED.current_memory_id)
                 > (fact_entities.current_created_at, fact_entities.current_memory_id)
         RETURNING fact_entity_id",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(&natural_key)
    .bind(memory_id)
    .bind(created_at)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    let fact_entity_id = if let Some(fact_entity_id) = fact_entity_id {
        fact_entity_id
    } else {
        fact_entity_id_for_in_tx(
            tx,
            owner,
            &draft.schema_id,
            draft.schema_version,
            &natural_key,
        )
        .await?
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "fact_entities conflict race left no row for schema {} v{} natural key {natural_key}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
            ))
        })?
    };

    let updated = sqlx::query(
        "UPDATE proxima_core.memories
            SET fact_entity_id = $1
          WHERE memory_id = $2",
    )
    .bind(fact_entity_id)
    .bind(memory_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    if updated.rows_affected() != 1 {
        return Err(StorageError::Internal(format!(
            "failed to attach fact_entity_id to memory {memory_id}",
        )));
    }
    Ok(())
}

async fn fact_entity_id_for_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    schema_id: &proxima_core::SchemaId,
    schema_version: SchemaVersion,
    natural_key: &serde_json::Value,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT fact_entity_id
           FROM proxima_core.fact_entities
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND schema_id = $4
            AND schema_version = $5
            AND natural_key = $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(schema_id.as_str())
    .bind(schema_version.into_inner().cast_signed())
    .bind(natural_key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

async fn enqueue_embedding_job_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: Option<&str>,
) -> Result<(), StorageError> {
    let Some(model_id) = model_id else {
        return Ok(());
    };

    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_principal_kind, owner_principal_id, owner_org_id,
             entity_kind, entity_id, model_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
