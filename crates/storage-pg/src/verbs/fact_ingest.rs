//! `FactIngest` verb — atomic insert of a Fact `memory`, optional
//! receipt/source-batch rows, optional citation rows, and `change_event` rows.

#![allow(dead_code, unused_imports, unused_variables)]
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
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite, EdgeEndpoint,
    EdgeKind, EntityKind, FactPayload, FactReceiptId, MemoryId, Owner, SchemaId, SourceBatchId,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::owner_columns::owner_binds;
use crate::error::{internal, map_err, with_bounded_retry};
use crate::pg_ident::PgIdent;
use crate::sidecars::{PgMemorySidecar, PgSidecarRegistryFrozen};
use crate::verbs::edge_index::assert_index_rows_in_tx;

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
    /// [`EdgeKind::Origin`] row per entry, in the Fact's own transaction.
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
    /// What the Fact declares it was made from, and what its typed payload
    /// points at. One `origin` row per entry of the first, one `reference`
    /// row per entry of the second, written in this Fact's own transaction —
    /// which is what makes them idempotent without an id scheme.
    links: FactLinks<'a>,
    /// Perspective that emitted this Fact, stamped on the row.
    authoring_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, Copy, Default)]
struct FactLinks<'a> {
    origins: &'a [EdgeEndpoint],
    references: &'a [EdgeEndpoint],
}

impl<'a> FactLinks<'a> {
    const fn new(origins: &'a [EdgeEndpoint], references: &'a [EdgeEndpoint]) -> Self {
        Self {
            origins,
            references,
        }
    }

    const fn is_empty(&self) -> bool {
        self.origins.is_empty() && self.references.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
enum CitationPlan<'a> {
    DraftHint,
    Inline {
        cited_object: &'a AuthorizedInlineCitedObject,
        mapping: &'a AuthorizedInlineCitationMapping,
        sidecars: &'a PgSidecarRegistryFrozen,
    },
    /// Cite an EXISTING cited object by id: no cited-object insert, no
    /// object sidecar — only the mapping row (+ mapping sidecar) against
    /// the stored object, after verifying it exists for the Fact's owner
    /// and carries the schema the mapping targets.
    ByRef {
        cited_object_id: uuid::Uuid,
        expected_object_schema: &'a SchemaId,
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
    const fn cited_object_id(self) -> uuid::Uuid {
        match self {
            Self::DraftHint {
                cited_object_id, ..
            }
            | Self::Inline {
                cited_object_id, ..
            } => cited_object_id,
        }
    }

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
    // A payload's schema-declared reference fields are read here, not passed
    // in: the payload IS the declaration, so a flavor ingest helper cannot
    // forget to hand them over and cannot invent one either.
    let references = payload_reference_targets(payload)?;
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: Some(derive_inputs),
        citation_plan: CitationPlan::DraftHint,
        links: FactLinks::new(&draft.derived_from, &references),
        authoring_perspective_id: None,
    };
    ingest_core(tx, permit.owner(), &draft, options, sidecar).await
}

/// Read a typed Fact payload's schema-declared reference fields as index
/// targets.
///
/// A declaration whose binding disagrees with the address form it produced is
/// refused rather than coerced: `FollowHead` and `Pin` are different
/// statements about what the reference means when the target is re-observed.
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
    let mut draft = FactWriteCommand::from_payload(
        ctx.source_id,
        ctx.source_batch_id,
        payload,
        ctx.observed_at,
    )
    .with_citation(citation)
    .with_derived_from(ctx.derived_from.to_vec());
    draft.handle = ctx.handle;
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
    authoring_perspective_id: Option<MemoryId>,
) -> Result<FactIngestOutcome, StorageError> {
    let options = IngestCoreOptions {
        embedding_model_id,
        derive_inputs: None,
        citation_plan: CitationPlan::DraftHint,
        links: FactLinks::new(&draft.derived_from, &[]),
        authoring_perspective_id,
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
#[allow(clippy::too_many_arguments)] // one parameter per typed-ingest input
pub(crate) async fn ingest_fact_with_derived_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    embedding_model_id: Option<&str>,
    sidecar_table: Option<&str>,
    natural_key_columns: &[&str],
    references: &[EdgeEndpoint],
    authoring_perspective_id: Option<MemoryId>,
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
        links: FactLinks::new(&draft.derived_from, references),
        authoring_perspective_id,
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
        links: FactLinks::new(
            authorized.links().origins(),
            authorized.links().references(),
        ),
        authoring_perspective_id: None,
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
        citation_plan: CitationPlan::ByRef {
            cited_object_id: authorized.cited_object_id(),
            expected_object_schema: authorized.expected_object_schema(),
            mapping: authorized.mapping(),
            sidecars,
        },
        links: FactLinks::new(
            authorized.links().origins(),
            authorized.links().references(),
        ),
        authoring_perspective_id: None,
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

/// Verify a by-ref citation target: the cited object must exist for the
/// Fact's owner and carry exactly the schema the mapping targets. Both
/// failures are caller-fixable `ConstraintViolation`s whose messages the
/// MCP surface passes through verbatim.
async fn persist_citation_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    plan: CitationPlan<'_>,
    memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    match plan {
        CitationPlan::DraftHint => persist_draft_citation(tx, owner, draft, memory_id).await,
        CitationPlan::Inline {
            cited_object,
            mapping,
            sidecars,
        } => {
            let cited_object_id =
                upsert_cited_object_in_tx(tx, sidecars, owner, cited_object).await?;
            insert_citation_mapping_in_tx(
                tx,
                owner,
                mapping,
                sidecars,
                uuid::Uuid::now_v7(),
                memory_id.into_inner(),
                cited_object_id,
            )
            .await?;
            Ok(Some(cited_object_id))
        }
        CitationPlan::ByRef {
            cited_object_id,
            expected_object_schema,
            mapping,
            sidecars,
        } => {
            verify_cited_object_ref_in_tx(tx, owner, cited_object_id, expected_object_schema)
                .await?;
            insert_citation_mapping_in_tx(
                tx,
                owner,
                mapping,
                sidecars,
                uuid::Uuid::now_v7(),
                memory_id.into_inner(),
                cited_object_id,
            )
            .await?;
            Ok(Some(cited_object_id))
        }
    }
}

async fn persist_draft_citation(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let Some(citation) = &draft.citation else {
        return Ok(None);
    };
    let owner_id = owner.stored_owner_id();
    let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO proxima_core.cited_objects
            (schema_id, owner_id, content_hash)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(citation.object.schema_id.as_str())
    .bind(owner_id)
    .bind(&citation.object.content_hash[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (schema_id, memory_id, cited_object_id)
         VALUES ($1, $2, $3)",
    )
    .bind(citation.mapping.schema_id.as_str())
    .bind(memory_id.into_inner())
    .bind(cited_object_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(Some(cited_object_id))
}

async fn citation_object_for_t(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT cited_object_id
           FROM proxima_core.citation_mappings
          WHERE memory_id = $1
          ORDER BY citation_mapping_id
          LIMIT 1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
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
           FROM proxima_core.cited_objects
          WHERE cited_object_id = $1
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
    let owner_id = owner.stored_owner_id();

    let stored_kind_text: String = sqlx::query_scalar(
        "SELECT m.kind::text
           FROM proxima_core.memory m
          WHERE m.t = $1
            AND m.owner_id = $2",
    )
    .bind(memory_uuid)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;
    let stored_kind = match stored_kind_text.as_str() {
        "fact" => EntityKind::Fact,
        "abstraction" => EntityKind::Abstraction,
        "perspective" => EntityKind::Perspective,
        other => {
            return Err(StorageError::Internal(format!(
                "invalid memory kind {other}"
            )));
        }
    };
    if stored_kind != authorized.memory_kind() {
        return Err(StorageError::ConstraintViolation(format!(
            "citation target {memory_uuid} is a {}, not a {}",
            stored_kind.as_str(),
            authorized.memory_kind().as_str(),
        )));
    }
    if !proxima_core::citations::kind_may_cite_directly(stored_kind) {
        return Err(StorageError::ConstraintViolation(format!(
            "a {} does not cite directly",
            stored_kind.as_str(),
        )));
    }

    if let Some(cited_object_id) = citation_object_for_t(tx, memory_id).await? {
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

    Ok(AttachCitationOutcome {
        memory_id,
        cited_object_id,
        attached: true,
        idempotent: false,
    })
}

async fn cited_object_id_for_mapping_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    _owner: &Owner,
    memory_id: uuid::Uuid,
    citation_mapping_id: uuid::Uuid,
) -> Result<uuid::Uuid, StorageError> {
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT cm.cited_object_id
            FROM proxima_core.citation_mappings cm
           WHERE cm.citation_mapping_id = $1
             AND cm.memory_id = $2",
    )
    .bind(citation_mapping_id)
    .bind(memory_id)
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
    let owner_id = owner.stored_owner_id();
    let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO proxima_core.cited_objects
            (schema_id, owner_id, content_hash)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object.schema_id().as_str())
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
    let _ = owner;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(citation_mapping_id)
    .bind(mapping.schema_id().as_str())
    .bind(memory_id)
    .bind(cited_object_id)
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
        links: FactLinks::new(
            authorized.links().origins(),
            authorized.links().references(),
        ),
        authoring_perspective_id: None,
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
    let mut outcome = super::memory_timeseries::ingest_fact_timeseries(tx, owner, draft).await?;
    sidecar(tx, &outcome).await?;
    if outcome.idempotent_replay {
        outcome.cited_object_id = citation_object_for_t(tx, outcome.memory_id).await?;
    } else {
        outcome.cited_object_id = persist_citation_timeseries(
            tx,
            owner,
            draft,
            options.citation_plan,
            outcome.memory_id,
        )
        .await?;
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
    return Ok(outcome);
    #[allow(unreachable_code)]
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
        let outcome = fact_replay_outcome(tx, receipt_id, memory_id).await?;
        assert_fact_index_rows(tx, owner, memory_id, options.links).await?;
        return Ok(outcome);
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
        CitationPlan::ByRef {
            cited_object_id,
            expected_object_schema,
            mapping,
            sidecars,
        } => {
            verify_cited_object_ref_in_tx(tx, owner, cited_object_id, expected_object_schema)
                .await?;
            Some(PendingCitation::Inline {
                mapping,
                sidecars,
                citation_mapping_id: uuid::Uuid::now_v7(),
                cited_object_id,
            })
        }
    };
    let citation_mapping_id = citation_refs
        .as_ref()
        .map(|pending| (*pending).citation_mapping_id());
    let cited_object_id = citation_refs
        .as_ref()
        .map(|pending| (*pending).cited_object_id());

    if let (Some(receipt), Some(receipt_id_bytes)) = (&draft.receipt, receipt_id_bytes) {
        sqlx::query(
            // Target-less ON CONFLICT tolerates conflicts on ANY unique
            // index. Keyed batches (deterministic ids from
            // `source_batch_key`) race concurrent ingests into identical
            // rows; with `(id)` as the sole arbiter, the loser collided on
            // `source_batches_unique_per_source` and surfaced a spurious
            // unique violation instead of no-oping.
            "INSERT INTO proxima_core.source_batches
                (id, source_id, owner_kind,
                 owner_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
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
                    let outcome = fact_replay_outcome(tx, receipt_id, memory_id).await?;
                    assert_fact_index_rows(tx, owner, memory_id, options.links).await?;
                    return Ok(outcome);
                }
                // Receipt occupied but no live memory row (e.g. tombstoned):
                // a genuine conflict, not a concurrent-ingest replay.
                return Err(map_err(err));
            }
            Err(err) => return Err(map_err(err)),
        }
    }

    if let Some(language) = draft.lexical_language.as_deref() {
        super::lexical_language::register_lexical_language_in_tx(tx, language).await?;
    }

    // NULL language means the column DEFAULT — the COALESCE spells that
    // out rather than branching the statement text on the option.
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version,
             kind, receipt_id, citation_mapping_id, text, authoring_perspective_id,
             lexical_language)
         VALUES ($1, $2, $3, $4, $5, 'Fact', $6, $7, $8, $9,
                 COALESCE($10::regconfig, proxima_core.lexical_config()))",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(receipt_id_bytes.as_ref().map(|bytes| &bytes[..]))
    .bind(citation_mapping_id)
    .bind(draft.rendered_text.as_deref())
    .bind(options.authoring_perspective_id.map(MemoryId::into_inner))
    .bind(draft.lexical_language.as_deref())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    assert_fact_index_rows(tx, owner, memory_id, options.links).await?;

    let outcome = FactIngestOutcome {
        receipt_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        handle: memory_id,
        change_event_seq: change_seq,
        idempotent_replay: false,
        cited_object_id,
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
    if let Some(embedding_model_id) = options.embedding_model_id {
        crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
            tx,
            owner_kind,
            owner_id,
            EntityKind::Fact,
            memory_id,
            embedding_model_id,
        )
        .await?;
    }

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

/// Assert the index rows a Fact write declared, inside the Fact's own
/// transaction.
///
/// Also called on the receipt-replay path. That is not a double write: the
/// primary key is the row, so re-asserting the same declaration inserts
/// nothing and announces nothing, while a declaration the first ingest did
/// not carry still lands. Idempotency here is structural, not conditional.
async fn assert_fact_index_rows(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    memory_id: uuid::Uuid,
    links: FactLinks<'_>,
) -> Result<(), StorageError> {
    if links.is_empty() {
        return Ok(());
    }
    let source = EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(memory_id));
    assert_index_rows_in_tx(tx.as_mut(), owner, source, EdgeKind::Origin, links.origins).await?;
    assert_index_rows_in_tx(
        tx.as_mut(),
        owner,
        source,
        EdgeKind::Reference,
        links.references,
    )
    .await?;
    Ok(())
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
    // The citation the ORIGINAL write made. A replay writes no citation
    // rows (this function returns before the citation plan runs), so
    // without this read a replayed caller could not learn which object
    // its Fact reaches — which for a content-addressed upload is the one
    // answer it came for.
    let cited_object_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT cm.cited_object_id
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm USING (citation_mapping_id)
          WHERE m.memory_id = $1",
    )
    .bind(memory_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(FactIngestOutcome {
        receipt_id,
        memory_id: proxima_core::MemoryId::new(memory_id),
        handle: memory_id,
        change_event_seq: seq,
        idempotent_replay: true,
        cited_object_id,
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
