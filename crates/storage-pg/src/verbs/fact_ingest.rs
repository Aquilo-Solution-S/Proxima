//! `FactIngest` verb — atomic insert of a Fact `memory` + optional
//! `blob_id` citation. No `citation_mappings` / `cited_objects` tables.
//!
//! [`ingest_fact_in_tx`] exposes the same body inside an existing
//! transaction so flavor crates can append a typed sidecar row
//! atomically with the Fact materialization. The pool-level
//! [`ingest_fact_atomic`] is a thin wrapper that opens its own tx.

use std::future::Future;
use std::pin::Pin;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::{
    AuthorizedInlineCitedObject, CitationSpec, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite, EdgeEndpoint,
    FactPayload, MemoryId, Owner, SchemaId, SourceBatchId, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err, with_bounded_retry};
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
    /// What this Fact declares it was made from. One
    /// [`proxima_core::EdgeKind::Origin`] row per entry, in the Fact's own transaction.
    /// Endpoints only — the kind follows from the field, never from a
    /// caller.
    pub derived_from: &'a [EdgeEndpoint],
    /// Perspective that emitted this Fact. A column on the row, because
    /// "emitted by P" is known at write time and belongs to the node.
    pub authoring_perspective_id: Option<MemoryId>,
    /// Series handle. `None` mints a new series.
    pub handle: Option<uuid::Uuid>,
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
            derived_from: &[],
            authoring_perspective_id: None,
            handle: None,
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

    /// Declare what this Fact was made from. Each endpoint becomes one
    /// `origin` index row inside the Fact's write transaction, which is
    /// what makes the provenance idempotent without an id scheme.
    #[must_use]
    pub const fn derived_from(mut self, derived_from: &'a [EdgeEndpoint]) -> Self {
        self.derived_from = derived_from;
        self
    }

    /// Stamp the Perspective that emitted this Fact on the row.
    #[must_use]
    pub const fn authoring_perspective_id(mut self, memory_id: Option<MemoryId>) -> Self {
        self.authoring_perspective_id = memory_id;
        self
    }

    /// Reuse an existing series handle (new `t` on the same handle).
    #[must_use]
    pub const fn handle(mut self, handle: Option<uuid::Uuid>) -> Self {
        self.handle = handle;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct IngestCoreOptions<'a> {
    embedding_model_id: Option<&'a str>,
    citation_plan: CitationPlan<'a>,
}

#[derive(Debug, Clone, Copy)]
enum CitationPlan<'a> {
    DraftHint,
    Inline {
        cited_object: &'a AuthorizedInlineCitedObject,
    },
    ByRef {
        cited_object_id: uuid::Uuid,
        expected_object_schema: &'a SchemaId,
    },
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
        let outcome =
            ingest_fact_command_in_tx(&mut tx, permit, draft, embedding_model_id, None).await?;
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
    sidecar_tables: &[String],
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = ingest_fact_with_sidecar_in_tx(
        &mut tx,
        authorized,
        embedding_model_id,
        sidecar_tables,
        sidecar,
    )
    .await?;
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
    sidecar_tables: &[String],
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
        sidecar_tables,
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
    ingest_typed_payload(
        tx,
        permit.owner(),
        &draft,
        embedding_model_id,
        P::sidecar_table(),
        sidecar,
    )
    .await
}

/// Read a typed Fact payload's schema-declared reference fields as index
/// targets.
///
/// Schema-declared reference fields become index targets. Every address is a pin.
fn payload_reference_targets<P: FactPayload>(
    payload: &P,
) -> Result<Vec<EdgeEndpoint>, StorageError> {
    payload
        .references()
        .into_iter()
        .map(|reference| {
            reference
                .validate()
                .map(|()| reference.target)
                .map_err(StorageError::ConstraintViolation)
        })
        .collect()
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
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand::from_payload(
        "proxima/fact",
        SourceBatchId::new(uuid::Uuid::now_v7()),
        payload,
        now,
    );
    ingest_typed_payload(
        tx,
        permit.owner(),
        &draft,
        embedding_model_id,
        None,
        noop_fact_sidecar,
    )
    .await
}

async fn ingest_typed_payload<F>(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
    sidecar_table: Option<&str>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::DraftHint,
    };
    let tables = sidecar_table
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    ingest_core(tx, owner, draft, options, &tables, sidecar).await
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
    let mut draft = FactWriteCommand::from_payload(
        ctx.source_id,
        ctx.source_batch_id,
        payload,
        ctx.observed_at,
    )
    .with_citation(citation)
    .with_derived_from(ctx.derived_from.to_vec());
    draft.handle = ctx.handle;
    if draft.handle.is_none()
        && let Some(table) = P::sidecar_table()
    {
        let columns = P::natural_key_columns();
        if !columns.is_empty() {
            let atoms = super::query::sidecar_atoms_from_payload(payload, columns)?;
            let binds = atoms
                .iter()
                .map(|(column, value)| (column.as_str(), value.clone()))
                .collect::<Vec<_>>();
            draft.handle = super::query::owned_head_handle(
                &mut **tx,
                *ctx.permit.owner(),
                &P::schema_id(),
                table,
                &binds,
            )
            .await?;
        }
    }
    let sidecar_payload = payload.clone();
    let references = payload_reference_targets(payload)?;
    ingest_fact_with_derived_sidecar_in_tx(
        tx,
        ctx.permit,
        &draft,
        ctx.embedding_model_id,
        P::sidecar_table(),
        P::natural_key_columns(),
        &references,
        ctx.authoring_perspective_id,
        move |tx, outcome| {
            Box::pin(async move {
                if outcome.idempotent_replay {
                    return Ok(());
                }
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
    _authoring_perspective_id: Option<MemoryId>,
) -> Result<FactIngestOutcome, StorageError> {
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(tx, permit.owner(), draft, options, &[], |_tx, _outcome| {
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
#[allow(clippy::too_many_arguments)] // one parameter per typed-ingest input
pub(crate) async fn ingest_fact_with_derived_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
    sidecar_table: Option<&str>,
    _natural_key_columns: &[&str],
    _references: &[EdgeEndpoint],
    _authoring_perspective_id: Option<MemoryId>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::DraftHint,
    };
    let tables = sidecar_table
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    ingest_core(tx, permit.owner(), draft, options, &tables, sidecar).await
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
    sidecar_tables: &[String],
    fact_sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    reject_unstamped_memory_tables(sidecars, sidecar_tables)?;
    let draft = authorized.draft();
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::Inline {
            cited_object: authorized.cited_object(),
        },
    };
    ingest_core(
        tx,
        authorized.owner_write_permit().owner(),
        draft,
        options,
        sidecar_tables,
        fact_sidecar,
    )
    .await
}

/// Run gated Fact ingest citing an EXISTING cited object by id inside an
/// already-open transaction.
///
/// The by-ref twin of [`ingest_fact_with_citation_in_tx`]: verifies the
/// referenced object (existence, owner, schema) in the same transaction,
/// then reuses the mapping-insert plumbing against the existing row.
/// Receipt/idempotency semantics are identical to the inline path — a
/// receipt replay returns before any citation row is written.
///
/// # Errors
///
/// Returns `ConstraintViolation` when the referenced object is missing
/// for the Fact's owner or its schema differs from the mapping's target;
/// otherwise storage errors from core row materialization, Fact sidecar
/// insertion, or citation-mapping sidecar insertion. The caller owns
/// transaction rollback/commit.
pub async fn ingest_fact_with_citation_ref_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedFactWithCitationRef,
    embedding_model_id: Option<&str>,
    sidecar_tables: &[String],
    fact_sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    reject_unstamped_memory_tables(sidecars, sidecar_tables)?;
    let draft = authorized.draft();
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::ByRef {
            cited_object_id: authorized.cited_object_id(),
            expected_object_schema: authorized.expected_object_schema(),
        },
    };
    ingest_core(
        tx,
        authorized.owner_write_permit().owner(),
        draft,
        options,
        sidecar_tables,
        fact_sidecar,
    )
    .await
}

fn reject_unstamped_memory_tables(
    sidecars: &PgSidecarRegistryFrozen,
    tables: &[String],
) -> Result<(), StorageError> {
    for table in tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
    }
    Ok(())
}

/// Verify a by-ref citation target: the cited object must exist for the
/// Fact's owner and carry exactly the schema the mapping targets. Both
/// failures are caller-fixable `ConstraintViolation`s whose messages the
/// MCP surface passes through verbatim.
async fn persist_citation_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    plan: CitationPlan<'_>,
) -> Result<Option<uuid::Uuid>, StorageError> {
    match plan {
        CitationPlan::DraftHint => persist_draft_citation(tx, owner, draft).await,
        CitationPlan::Inline { cited_object, .. } => upsert_blob(
            tx,
            owner,
            cited_object.schema_id().as_str(),
            cited_object.content_hash(),
        )
        .await
        .map(Some),
        CitationPlan::ByRef {
            cited_object_id,
            expected_object_schema,
            ..
        } => {
            verify_cited_object_ref_in_tx(tx, owner, cited_object_id, expected_object_schema)
                .await?;
            Ok(Some(cited_object_id))
        }
    }
}

async fn upsert_blob(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    schema_id: &str,
    content_hash: &[u8; 32],
) -> Result<uuid::Uuid, StorageError> {
    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;
    sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING blob_id",
    )
    .bind(owner_id)
    .bind(schema_id)
    .bind(&content_hash[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}

async fn persist_draft_citation(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let Some(citation) = &draft.citation else {
        return Ok(None);
    };
    upsert_blob(
        tx,
        owner,
        citation.object.schema_id.as_str(),
        &citation.object.content_hash,
    )
    .await
    .map(Some)
}

async fn citation_object_for_t(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let blob_id: Option<Option<uuid::Uuid>> =
        sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
            .bind(memory_id.into_inner())
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    Ok(blob_id.flatten())
}

async fn verify_cited_object_ref_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    cited_object_id: uuid::Uuid,
    expected_object_schema: &SchemaId,
) -> Result<(), StorageError> {
    let owner_id = owner.stored_owner_id();
    let schema_id: Option<String> = sqlx::query_scalar(
        "SELECT schema_id
           FROM proxima_core.blob
          WHERE blob_id = $1
            AND owner_id = $2",
    )
    .bind(cited_object_id)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    // Absent and other-owner are deliberately the same message: whether
    // the row exists under a foreign owner must not be observable.
    let Some(schema_id) = schema_id else {
        return Err(StorageError::ConstraintViolation(format!(
            "cited object {cited_object_id} not found for this owner"
        )));
    };
    if schema_id != expected_object_schema.as_str() {
        return Err(StorageError::ConstraintViolation(format!(
            "citation mapping targets cited-object schema {}, but cited object \
             {cited_object_id} has schema {schema_id}",
            expected_object_schema.as_str(),
        )));
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
    sidecar_tables: &[String],
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let draft = authorized.draft();
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::DraftHint,
    };
    ingest_core(
        tx,
        authorized.owner_write_permit().owner(),
        draft,
        options,
        sidecar_tables,
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
    sidecar_tables: &[String],
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    crate::access::owner_columns::reject_world_write_owner(owner)?;
    let mut write = draft.clone();
    if write.blob_id.is_none() {
        write.blob_id =
            persist_citation_timeseries(tx, owner, draft, options.citation_plan).await?;
    }
    let mut outcome =
        super::memory_timeseries::ingest_fact_timeseries(tx, owner, &write, sidecar_tables).await?;
    // Replay reuses the original `(handle, t)`. The sidecar row is already
    // there; inserting again trips `<table>_pkey` on `t`.
    if !outcome.idempotent_replay {
        sidecar(tx, &outcome).await?;
    }
    if outcome.idempotent_replay {
        outcome.cited_object_id = citation_object_for_t(tx, outcome.memory_id).await?;
    } else {
        outcome.cited_object_id = write.blob_id;
        if let Some(model_id) = options.embedding_model_id {
            crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
                tx,
                crate::access::owner_columns::owner_binds(owner).0,
                Some(owner.stored_owner_id()),
                proxima_core::EntityKind::Fact,
                outcome.memory_id.into_inner(),
                model_id,
            )
            .await?;
        }
    }
    Ok(outcome)
}
