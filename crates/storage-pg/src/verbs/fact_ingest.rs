//! `FactIngest` verb — atomic insert of a Fact `memory`, optional
//! receipt/source-batch rows, optional citation rows, and `change_event` rows.
//!
//! Receipt-backed replay is detected by the `(receipt_id)` unique on
//! `memories`; the caller observes `idempotent_replay = true` and the
//! original `change_event_seq`. Receiptless writes never replay.
//!
//! [`ingest_fact_in_tx`] exposes the same body inside an existing
//! transaction so flavor crates can append a typed sidecar row
//! atomically with the Fact materialization. The pool-level
//! [`ingest_fact_atomic`] is a thin wrapper that opens its own tx.

use std::future::Future;
use std::pin::Pin;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::{
    AuthorizedCitationAttachment, AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject,
    Citation, CitationSpec, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::{
    AuthorizedFactWithCitation, AuthorizedFactWrite, EntityKind, FactPayload, FactReceiptId,
    MemoryId, Owner, OwnerRefKind, SourceBatchId, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::owner_columns::owner_binds;
use crate::error::{internal, map_err, with_bounded_retry};
use crate::pg_ident::PgIdent;
use crate::sidecars::{PgMemorySidecar, PgSidecarRegistryFrozen};

pub type FactIngestSidecarFuture<'t> =
    Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;

/// Companion trait for Postgres-backed typed Fact sidecars. Flavor
/// crates implement this for each `FactPayload` whose sidecar table
/// is inserted by `proxima-storage-pg` helpers.
pub trait PgFactSidecar: FactPayload + Sized {
    /// Insert this payload's sidecar row keyed by `memory_id`.
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't;
}

/// Common context for typed Fact ingest helpers.
#[derive(Debug, Clone)]
pub struct FactIngestContext<'a> {
    pub permit: &'a OwnerWritePermit,
    pub source_id: &'a str,
    pub source_batch_id: SourceBatchId,
    pub observed_at: time::OffsetDateTime,
    pub embedding_model_id: Option<&'a str>,
}

impl<'a> FactIngestContext<'a> {
    /// Build a context with `observed_at = now` and no embedding model.
    #[must_use]
    pub fn new(
        permit: &'a OwnerWritePermit,
        source_id: &'a str,
        source_batch_id: SourceBatchId,
    ) -> Self {
        Self {
            permit,
            source_id,
            source_batch_id,
            observed_at: time::OffsetDateTime::now_utc(),
            embedding_model_id: None,
        }
    }

    /// Override the observation/occurrence timestamp used by the draft.
    #[must_use]
    pub const fn observed_at(mut self, observed_at: time::OffsetDateTime) -> Self {
        self.observed_at = observed_at;
        self
    }

    /// Configure the embedding model id to enqueue for the ingested Fact.
    #[must_use]
    pub const fn embedding_model_id(mut self, model_id: Option<&'a str>) -> Self {
        self.embedding_model_id = model_id;
        self
    }
}

#[derive(Debug, Clone)]
pub struct AttachCitationOutcome {
    pub memory_id: MemoryId,
    pub cited_object_id: uuid::Uuid,
    pub attached: bool,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Copy)]
struct FactEntityDeriveInputs<'a> {
    sidecar_table: Option<&'a str>,
    natural_key_columns: &'a [String],
}

#[derive(Debug, Clone, Copy)]
struct IngestCoreOptions<'a> {
    embedding_model_id: Option<&'a str>,
    derive_inputs: Option<FactEntityDeriveInputs<'a>>,
    citation_plan: CitationPlan<'a>,
}

#[derive(Debug, Clone, Copy)]
enum CitationPlan<'a> {
    DraftHint,
    Inline {
        cited_object: &'a AuthorizedInlineCitedObject,
        mapping: &'a AuthorizedInlineCitationMapping,
        sidecars: &'a PgSidecarRegistryFrozen,
    },
}

#[derive(Debug, Clone, Copy)]
enum PendingCitation<'a> {
    DraftHint {
        citation: &'a Citation,
        citation_mapping_id: uuid::Uuid,
        cited_object_id: uuid::Uuid,
    },
    Inline {
        mapping: &'a AuthorizedInlineCitationMapping,
        sidecars: &'a PgSidecarRegistryFrozen,
        citation_mapping_id: uuid::Uuid,
        cited_object_id: uuid::Uuid,
    },
}

impl PendingCitation<'_> {
    const fn citation_mapping_id(self) -> uuid::Uuid {
        match self {
            Self::DraftHint {
                citation_mapping_id,
                ..
            }
            | Self::Inline {
                citation_mapping_id,
                ..
            } => citation_mapping_id,
        }
    }
}

/// Pool-scoped `FactIngest`. Opens its own transaction; commits on
/// success.
///
/// # Errors
///
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
pub async fn ingest_fact_atomic(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    // Retry the whole transaction on transient deadlock/serialization.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let outcome = ingest_fact_command_in_tx(&mut tx, permit, draft, embedding_model_id).await?;
        tx.commit().await.map_err(map_err)?;
        Ok(outcome)
    })
    .await
}

/// Pool-scoped gated `FactIngest` with a caller-owned typed sidecar
/// insert. Opens its own transaction; commits only after the Fact and
/// sidecar both succeed.
///
/// # Errors
///
/// Returns storage errors from Fact materialization, sidecar insertion,
/// or transaction commit. A sidecar error rolls back the transaction.
pub async fn fact_ingest_with_sidecar_atomic<F>(
    pool: &PgPool,
    authorized: &AuthorizedFactWrite,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome =
        ingest_fact_with_sidecar_in_tx(&mut tx, authorized, embedding_model_id, sidecar).await?;
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
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedFactWithCitation,
    embedding_model_id: Option<&str>,
    fact_sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = ingest_fact_with_citation_in_tx(
        &mut tx,
        sidecars,
        authorized,
        embedding_model_id,
        fact_sidecar,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Build an uncited Fact draft from a typed payload, authorize it, and
/// materialize it plus the caller-owned sidecar inside an existing tx.
///
/// # Errors
///
/// Returns storage errors from authorization/schema validation, Fact
/// materialization, or sidecar insertion.
pub async fn ingest_fact_in_tx<P, F>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    ingest_fact_for_owner_in_tx(tx, permit, payload, embedding_model_id, sidecar).await
}

/// Transaction-scoped uncited Fact ingest helper for an explicit target owner.
///
/// # Errors
///
/// Returns storage errors from Fact authorization, materialization, sidecar
/// insertion, or embedding enqueue.
pub async fn ingest_fact_for_owner_in_tx<P, F>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand::from_payload(
        "proxima/fact",
        SourceBatchId::new(uuid::Uuid::now_v7()),
        payload,
        now,
    );
    let natural_key_columns = P::natural_key_columns()
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let derive_inputs = FactEntityDeriveInputs {
        sidecar_table: P::sidecar_table(),
        natural_key_columns: &natural_key_columns,
    };
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: Some(derive_inputs),
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(tx, permit.owner(), &draft, options, sidecar).await
}

/// Transaction-scoped uncited Fact ingest helper with no caller-owned sidecar.
///
/// # Errors
///
/// Returns storage errors from Fact authorization, materialization, or
/// embedding enqueue. The caller owns transaction rollback/commit.
pub async fn ingest_fact_for_owner_plain_in_tx<P>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
{
    ingest_fact_for_owner_in_tx(tx, permit, payload, embedding_model_id, noop_fact_sidecar).await
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
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    ingest_fact_for_owner(pool, permit, payload, embedding_model_id, sidecar).await
}

/// Pool-scoped uncited Fact ingest helper with no caller-owned sidecar.
///
/// # Errors
///
/// Returns storage errors from transaction setup, Fact authorization,
/// materialization, embedding enqueue, or transaction commit.
pub async fn ingest_fact_for_owner_plain<P>(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
{
    // Retry the whole transaction on transient deadlock/serialization.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let outcome =
            ingest_fact_for_owner_plain_in_tx(&mut tx, permit, payload, embedding_model_id).await?;
        tx.commit().await.map_err(map_err)?;
        Ok(outcome)
    })
    .await
}

/// Pool-scoped uncited Fact ingest helper for an explicit target owner.
///
/// # Errors
///
/// Returns storage errors from transaction setup, Fact authorization, sidecar
/// insertion, or transaction commit.
pub async fn ingest_fact_for_owner<P, F>(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    payload: &P,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload,
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome =
        ingest_fact_for_owner_in_tx(&mut tx, permit, payload, embedding_model_id, sidecar).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

fn noop_fact_sidecar<'t>(
    _tx: &'t mut Transaction<'_, Postgres>,
    _outcome: &'t FactIngestOutcome,
) -> FactIngestSidecarFuture<'t> {
    Box::pin(async { Ok(()) })
}

/// Build a typed Fact draft with an opaque citation and insert the
/// Fact plus its Postgres sidecar inside an existing transaction.
///
/// # Errors
///
/// Returns storage errors from Fact materialization, sidecar insertion,
/// Fact-entity derivation, or embedding enqueue.
pub async fn ingest_fact_with_sidecar<P>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &FactIngestContext<'_>,
    payload: &P,
    citation: CitationSpec,
) -> Result<FactIngestOutcome, StorageError>
where
    P: FactPayload + PgMemorySidecar + Clone,
{
    let draft = FactWriteCommand::from_payload(
        ctx.source_id,
        ctx.source_batch_id,
        payload,
        ctx.observed_at,
    )
    .with_citation(citation);
    let sidecar_payload = payload.clone();
    ingest_fact_with_derived_sidecar_in_tx(
        tx,
        ctx.permit,
        &draft,
        ctx.embedding_model_id,
        P::sidecar_table(),
        P::natural_key_columns(),
        move |tx, outcome| {
            Box::pin(async move {
                sidecar_payload
                    .insert_memory_sidecar(tx, outcome.memory_id)
                    .await
            })
        },
    )
    .await
}

/// Run the `FactIngest` body inside an already-open transaction. The
/// caller owns `tx` and is responsible for committing or rolling back.
///
/// Crate-private: raw-owner write with no proof/authz param; in-crate
/// callers only (`ingest_fact_atomic`, goal-write side effects). External
/// flavors bundle sidecars via the `FactIngestContext`-based helpers.
///
/// # Errors
///
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
pub(crate) async fn ingest_fact_command_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: None,
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(tx, permit.owner(), draft, options, |_tx, _outcome| {
        Box::pin(async { Ok(()) })
    })
    .await
}

/// Run raw-draft `FactIngest` plus a typed sidecar insert inside an
/// already-open transaction, deriving a Fact-entity head from the
/// sidecar row after the sidecar insert succeeds.
///
/// Crate-private: raw-owner write with no proof/authz param; the only
/// caller is `ingest_fact_with_sidecar`, which carries the owner inside
/// its `FactIngestContext`.
///
/// # Errors
///
/// Returns storage errors from Fact materialization, sidecar
/// insertion, Fact-entity derivation, or embedding enqueue. The caller
/// owns transaction rollback/commit.
pub(crate) async fn ingest_fact_with_derived_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
    sidecar_table: Option<&str>,
    natural_key_columns: &[&str],
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let natural_key_columns = natural_key_columns
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let derive_inputs = FactEntityDeriveInputs {
        sidecar_table,
        natural_key_columns: &natural_key_columns,
    };
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: Some(derive_inputs),
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(tx, permit.owner(), draft, options, sidecar).await
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
pub async fn ingest_fact_with_citation_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedFactWithCitation,
    embedding_model_id: Option<&str>,
    fact_sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let draft = authorized.draft();
    let derive_inputs = FactEntityDeriveInputs {
        sidecar_table: authorized.fact_sidecar_table(),
        natural_key_columns: authorized.fact_natural_key_columns(),
    };
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: Some(derive_inputs),
        citation_plan: CitationPlan::Inline {
            cited_object: authorized.cited_object(),
            mapping: authorized.mapping(),
            sidecars,
        },
    };
    ingest_core(
        tx,
        authorized.owner_write_permit().owner(),
        draft,
        options,
        fact_sidecar,
    )
    .await
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
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedCitationAttachment,
) -> Result<AttachCitationOutcome, StorageError> {
    let memory_id = authorized.memory_id();
    let memory_uuid = memory_id.into_inner();
    let owner = authorized.owner_write_permit().owner();
    let (owner_kind, owner_id) = owner_binds(owner);

    let existing_mapping_id = sqlx::query_scalar::<_, Option<uuid::Uuid>>(
        "SELECT citation_mapping_id
            FROM proxima_core.memories m
           WHERE m.memory_id = $1
             AND m.owner_kind = $2
             AND m.owner_id IS NOT DISTINCT FROM $3
             AND m.kind IS NULL
             AND m.tombstoned_at IS NULL",
    )
    .bind(memory_uuid)
    .bind(owner_kind)
    .bind(owner_id)
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

    let cited_object_id =
        upsert_cited_object_in_tx(tx, sidecars, owner, authorized.cited_object()).await?;
    let citation_mapping_id = uuid::Uuid::now_v7();
    insert_citation_mapping_in_tx(
        tx,
        owner,
        authorized.mapping(),
        sidecars,
        citation_mapping_id,
        memory_uuid,
        cited_object_id,
    )
    .await?;

    let updated = sqlx::query(
        "UPDATE proxima_core.memories m
             SET citation_mapping_id = $2
           WHERE m.memory_id = $1
             AND m.owner_kind = $3
             AND m.owner_id IS NOT DISTINCT FROM $4
             AND m.citation_mapping_id IS NULL",
    )
    .bind(memory_uuid)
    .bind(citation_mapping_id)
    .bind(owner_kind)
    .bind(owner_id)
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
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT cm.cited_object_id
            FROM proxima_core.citation_mappings cm
           WHERE cm.citation_mapping_id = $1
             AND cm.memory_id = $2
             AND cm.owner_kind = $3
             AND cm.owner_id IS NOT DISTINCT FROM $4",
    )
    .bind(citation_mapping_id)
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
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
    sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    cited_object: &AuthorizedInlineCitedObject,
) -> Result<uuid::Uuid, StorageError> {
    let (owner_kind, owner_id) = owner_binds(owner);
    let cited_object_id_new = uuid::Uuid::now_v7();
    let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_kind,
             owner_id, content_hash)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_kind, owner_id,
                      schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id_new)
    .bind(cited_object.schema_id().as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&cited_object.content_hash()[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    sidecars
        .insert_cited_object_sidecar(tx.as_mut(), cited_object_id, cited_object.sidecar_payload())
        .await?;
    Ok(cited_object_id)
}

async fn insert_citation_mapping_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    mapping: &AuthorizedInlineCitationMapping,
    sidecars: &PgSidecarRegistryFrozen,
    citation_mapping_id: uuid::Uuid,
    memory_id: uuid::Uuid,
    cited_object_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_kind,
             owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(citation_mapping_id)
    .bind(mapping.schema_id().as_str())
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    // A pure-link mapping has no sidecar — the link is fully captured by
    // the citation_mappings row above, so there's nothing more to write.
    if let Some(sidecar_payload) = mapping.sidecar_payload() {
        sidecars
            .insert_citation_mapping_sidecar(tx.as_mut(), citation_mapping_id, sidecar_payload)
            .await?;
    }
    Ok(())
}

/// Run gated `FactIngest` plus a typed sidecar insert inside an
/// already-open transaction. The witness proves the draft passed core
/// authorization, owner stamping, and schema validation before storage
/// saw it.
///
/// # Errors
///
/// Returns storage errors from Fact materialization or sidecar
/// insertion. The caller owns transaction rollback/commit.
pub async fn ingest_fact_with_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedFactWrite,
    embedding_model_id: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let draft = authorized.draft();
    let derive_inputs = FactEntityDeriveInputs {
        sidecar_table: authorized.fact_sidecar_table(),
        natural_key_columns: authorized.fact_natural_key_columns(),
    };
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: Some(derive_inputs),
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(
        tx,
        authorized.owner_write_permit().owner(),
        draft,
        options,
        sidecar,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn ingest_core<F>(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    options: IngestCoreOptions<'_>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    crate::access::owner_columns::reject_world_write_owner(owner)?;
    let receipt_id = draft.receipt_id_for_owner(*owner);
    let receipt_id_bytes = receipt_id.map(FactReceiptId::into_inner);
    let (owner_kind, owner_id) = owner_binds(owner);

    if let (Some(receipt), Some(receipt_id_bytes)) = (&draft.receipt, receipt_id_bytes.as_ref()) {
        super::compliance_erase::check_suppression_for_fact_tx(
            tx,
            *owner,
            &receipt.source_id,
            receipt.source_batch_id.into_inner(),
            receipt_id_bytes,
        )
        .await?;
    }

    if let Some(receipt_id_bytes) = receipt_id_bytes
        && let Some(memory_id) = existing_fact_memory_by_receipt(tx, &receipt_id_bytes[..]).await?
    {
        return fact_replay_outcome(tx, receipt_id, memory_id).await;
    }

    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    let citation_refs = match options.citation_plan {
        CitationPlan::DraftHint => {
            if let Some(citation) = &draft.citation {
                let citation_mapping_id = uuid::Uuid::now_v7();
                let cited_object_id_new = uuid::Uuid::now_v7();
                let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
                    "INSERT INTO proxima_core.cited_objects
                        (cited_object_id, schema_id, owner_kind,
                         owner_id, content_hash)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (owner_kind, owner_id,
                                  schema_id, content_hash)
                     DO UPDATE SET schema_id = EXCLUDED.schema_id
                     RETURNING cited_object_id",
                )
                .bind(cited_object_id_new)
                .bind(citation.object.schema_id.as_str())
                .bind(owner_kind)
                .bind(owner_id)
                .bind(&citation.object.content_hash[..])
                .fetch_one(tx.as_mut())
                .await
                .map_err(map_err)?;
                Some(PendingCitation::DraftHint {
                    citation,
                    citation_mapping_id,
                    cited_object_id,
                })
            } else {
                None
            }
        }
        CitationPlan::Inline {
            cited_object,
            mapping,
            sidecars,
        } => {
            let citation_mapping_id = uuid::Uuid::now_v7();
            let cited_object_id =
                upsert_cited_object_in_tx(tx, sidecars, owner, cited_object).await?;
            Some(PendingCitation::Inline {
                mapping,
                sidecars,
                citation_mapping_id,
                cited_object_id,
            })
        }
    };
    let citation_mapping_id = citation_refs
        .as_ref()
        .map(|pending| (*pending).citation_mapping_id());

    if let (Some(receipt), Some(receipt_id_bytes)) = (&draft.receipt, receipt_id_bytes) {
        sqlx::query(
            "INSERT INTO proxima_core.source_batches
                (id, source_id, owner_kind,
                 owner_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(receipt.source_batch_id.into_inner())
        .bind(receipt.source_id.as_str())
        .bind(owner_kind)
        .bind(owner_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

        let batch_closed: bool = sqlx::query_scalar(
            "SELECT closed_at IS NOT NULL
               FROM proxima_core.source_batches
              WHERE id = $1
              FOR UPDATE",
        )
        .bind(receipt.source_batch_id.into_inner())
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
        if batch_closed {
            return Err(StorageError::ConstraintViolation(
                "cannot ingest Fact into closed source batch".into(),
            ));
        }

        // Two concurrent same-receipt ingests both pass the existing
        // check above; the loser collides on `fact_receipts_pkey` here (the
        // receipt row is written before the memories row, so this is the
        // race's first collision). Guard the insert with a SAVEPOINT so the
        // mid-tx unique violation does not poison the whole transaction —
        // then roll back and replay the winner's committed Fact instead of
        // surfacing a spurious ConstraintViolation.
        sqlx::query("SAVEPOINT proxima_fact_receipt")
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
        let receipt_insert = sqlx::query(
            "INSERT INTO proxima_core.fact_receipts
                (receipt_id, source, source_batch_id,
                 owner_kind, owner_id,
                 schema_id, schema_version, observed_at, occurred_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&receipt_id_bytes[..])
        .bind(receipt.source_id.as_str())
        .bind(receipt.source_batch_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(draft.schema_id.as_str())
        .bind(draft.schema_version.into_inner().cast_signed())
        .bind(receipt.observed_at)
        .bind(receipt.occurred_at)
        .execute(tx.as_mut())
        .await;
        match receipt_insert {
            Ok(_) => {
                sqlx::query("RELEASE SAVEPOINT proxima_fact_receipt")
                    .execute(tx.as_mut())
                    .await
                    .map_err(map_err)?;
            }
            Err(err) if is_receipt_race(&err) => {
                sqlx::query("ROLLBACK TO SAVEPOINT proxima_fact_receipt")
                    .execute(tx.as_mut())
                    .await
                    .map_err(map_err)?;
                if let Some(memory_id) =
                    existing_fact_memory_by_receipt(tx, &receipt_id_bytes[..]).await?
                {
                    return fact_replay_outcome(tx, receipt_id, memory_id).await;
                }
                // Receipt occupied but no live memory row (e.g. tombstoned):
                // a genuine conflict, not a concurrent-ingest replay.
                return Err(map_err(err));
            }
            Err(err) => return Err(map_err(err)),
        }
    }

    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version,
             receipt_id, citation_mapping_id, text)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(receipt_id_bytes.as_ref().map(|bytes| &bytes[..]))
    .bind(citation_mapping_id)
    .bind(draft.rendered_text.as_deref())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let outcome = FactIngestOutcome {
        receipt_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    };

    sidecar(tx, &outcome).await?;
    if let Some(inputs) = options.derive_inputs {
        derive_fact_entity_after_sidecar(
            tx,
            owner,
            draft,
            memory_id,
            inputs.sidecar_table,
            inputs.natural_key_columns,
        )
        .await?;
    }
    enqueue_embedding_job_in_tx(
        tx,
        owner_kind,
        owner_id,
        EntityKind::Fact,
        memory_id,
        options.embedding_model_id,
    )
    .await?;

    if let Some(pending) = citation_refs {
        match pending {
            PendingCitation::DraftHint {
                citation,
                citation_mapping_id,
                cited_object_id,
            } => {
                sqlx::query(
                    "INSERT INTO proxima_core.citation_mappings
                        (citation_mapping_id, schema_id, memory_id,
                         cited_object_id, owner_kind,
                         owner_id)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(citation_mapping_id)
                .bind(citation.mapping.schema_id.as_str())
                .bind(memory_id)
                .bind(cited_object_id)
                .bind(owner_kind)
                .bind(owner_id)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            }
            PendingCitation::Inline {
                mapping,
                sidecars,
                citation_mapping_id,
                cited_object_id,
            } => {
                insert_citation_mapping_in_tx(
                    tx,
                    owner,
                    mapping,
                    sidecars,
                    citation_mapping_id,
                    memory_id,
                    cited_object_id,
                )
                .await?;
            }
        }
    }

    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind,
             entity_memory_id, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, 'EntityAppend', 'Fact', $4, $5, $6)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(outcome)
}

/// True when a `fact_receipts`/`memories` insert failed because another
/// transaction already claimed the same receipt id (the idempotent race).
/// Matched on the constraint name so it never mistakes an unrelated unique
/// violation for a replay signal.
fn is_receipt_race(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db)
            if db.is_unique_violation()
                && matches!(
                    db.constraint(),
                    Some("fact_receipts_pkey" | "memories_one_fact_per_receipt")
                )
    )
}

/// The live (non-tombstoned) Fact memory id for a receipt, if any.
async fn existing_fact_memory_by_receipt(
    tx: &mut Transaction<'_, Postgres>,
    receipt_id_bytes: &[u8],
) -> Result<Option<uuid::Uuid>, StorageError> {
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT memory_id FROM proxima_core.memories
           WHERE receipt_id = $1
             AND tombstoned_at IS NULL",
    )
    .bind(receipt_id_bytes)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

/// Build the `idempotent_replay = true` outcome for an already-materialized
/// Fact, resolving its original `change_event` seq.
async fn fact_replay_outcome(
    tx: &mut Transaction<'_, Postgres>,
    receipt_id: Option<FactReceiptId>,
    memory_id: uuid::Uuid,
) -> Result<FactIngestOutcome, StorageError> {
    let seq = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT seq FROM proxima_core.change_event
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
    )
    .bind(memory_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(FactIngestOutcome {
        receipt_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        change_event_seq: seq,
        idempotent_replay: true,
    })
}

async fn derive_fact_entity_after_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
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
    let (natural_key, created_at) =
        fact_natural_key_after_sidecar(tx, memory_id, sidecar_table, natural_key_columns).await?;

    let (owner_kind, owner_id) = owner_binds(owner);
    let fact_entity_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO proxima_core.fact_entities
            (fact_entity_id, owner_kind, owner_id,
             schema_id, schema_version, natural_key, current_memory_id, current_created_at)
         VALUES
            ($1, $2, $3, $4, $5, $6::text[], $7, $8)
         ON CONFLICT (owner_kind, owner_id,
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
    .bind(owner_id)
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
        crate::verbs::query::fact_entity_id_for(
            tx.as_mut(),
            owner,
            &draft.schema_id,
            draft.schema_version,
            &natural_key,
        )
        .await?
        .map(proxima_core::FactEntityId::into_inner)
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "fact_entities conflict race left no row for schema {} v{} natural key {:?}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
                natural_key,
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

async fn fact_natural_key_after_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: uuid::Uuid,
    sidecar_table: &str,
    natural_key_columns: &[String],
) -> Result<(Vec<String>, time::OffsetDateTime), StorageError> {
    let sidecar_table = PgIdent::table(sidecar_table)?;
    let natural_key_exprs = natural_key_columns
        .iter()
        .map(|column| PgIdent::column(column).map(|ident| format!("s.{}::text", ident.as_str())))
        .collect::<Result<Vec<_>, _>>()?;
    let natural_key_sql = format!(
        "SELECT ARRAY[{}]::text[] AS natural_key, m.created_at
           FROM {} s
           JOIN proxima_core.memories m ON m.memory_id = s.memory_id
          WHERE s.memory_id = $1",
        natural_key_exprs.join(", "),
        sidecar_table.as_str(),
    );
    // SQL-POLICY: PgIdent — sidecar_table and every natural-key column are
    // PgIdent-validated above; the only bound input is the $1 uuid.
    sqlx::query_as::<_, (Vec<String>, time::OffsetDateTime)>(sqlx::AssertSqlSafe(natural_key_sql))
        .bind(memory_id)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "missing sidecar row in {} for Fact memory {memory_id}",
                sidecar_table.as_str(),
            ))
        })
}

async fn enqueue_embedding_job_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: Option<&str>,
) -> Result<(), StorageError> {
    let Some(model_id) = model_id else {
        return Ok(());
    };

    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_kind, owner_id,
             entity_kind, entity_id, model_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_kind, owner_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
