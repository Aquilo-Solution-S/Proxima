//! `FactIngest` verb — atomic insert of a Fact `memory` + optional
//! `blob_id` citation.
//!
//! Every write helper here is `pub(crate)`: they are the body of the
//! `FactIngestPort` / `WriteSession` implementations in `crate::ports`, not
//! an API. A caller reaches a Fact write through `Engine`/`UnitOfWork`,
//! which is the one path that maintains the search-projection row, the
//! sketch, and the embedding enqueue together with the Fact. A second
//! entry point that skipped any of those would make them optional.
//!
//! [`PgFactSidecar`] stays public because flavors IMPLEMENT it. They cannot
//! invoke it — see [`crate::sidecars::SidecarInsertPermit`].

use std::future::Future;
use std::pin::Pin;

use proxima_core::verbs::fact_ingest::{
    AuthorizedInlineCitedObject, AuthorizedNodeLinks, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite, FactPayload,
    MemoryId, Owner, SchemaId, SidecarPayload, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::scope_surfaces::ScopeFenceTarget;
use crate::error::{internal, map_err, with_bounded_retry};
use crate::sidecars::PgSidecarRegistryFrozen;

pub type FactIngestSidecarFuture<'t> =
    Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;

/// Companion trait for Postgres-backed typed Fact sidecars. Flavor
/// crates implement this for each `FactPayload` whose sidecar table
/// is inserted by `proxima-storage-pg`.
///
/// Implemented by flavors, invoked by the frozen registry only: the
/// [`crate::sidecars::SidecarInsertPermit`] argument cannot be minted
/// outside this crate.
pub trait PgFactSidecar: FactPayload + Sized {
    /// Insert this payload's sidecar row keyed by `memory_id`.
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        permit: crate::sidecars::SidecarInsertPermit,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't;
}

/// What [`ingest_core`] needs beyond the draft: which embedding model to
/// enqueue, how the draft's citation resolves to a `blob_id`, and which
/// flavor-declared lifecycle scopes this write's payloads belong to.
#[derive(Debug, Clone, Copy)]
struct IngestCoreOptions<'a> {
    embedding_model_id: Option<&'a str>,
    citation_plan: CitationPlan<'a>,
    /// The declared scopes to fence, already sorted and deduplicated. Empty
    /// for a payload-less write; a write whose payload declares a scope and
    /// whose caller passes `&[]` here would admit a row a concurrent scope
    /// erase cannot see, which is the defect this parameter exists to make
    /// unspellable at the call site.
    scopes: &'a [ScopeFenceTarget],
}

/// Authorization-bearing inputs shared by every Fact persistence route.
#[derive(Debug, Clone, Copy)]
struct AuthorizedFactInput<'a> {
    owner: &'a Owner,
    draft: &'a FactWriteCommand,
    links: &'a AuthorizedNodeLinks,
}

impl<'a> AuthorizedFactInput<'a> {
    const fn new(
        owner: &'a Owner,
        draft: &'a FactWriteCommand,
        links: &'a AuthorizedNodeLinks,
    ) -> Self {
        Self {
            owner,
            draft,
            links,
        }
    }
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
pub(crate) async fn ingest_authorized_fact_atomic(
    pool: &PgPool,
    authorized: &AuthorizedFactWrite,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    // Retry the whole transaction on transient deadlock/serialization.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let outcome = ingest_fact_command_in_tx(&mut tx, authorized, embedding_model_id).await?;
        tx.commit().await.map_err(map_err)?;
        Ok(outcome)
    })
    .await
}

/// Run the `FactIngest` body inside an already-open transaction. The
/// caller owns `tx` and is responsible for committing or rolling back.
///
/// Crate-private transaction body for the authorized Fact write. External
/// flavors bundle sidecars via the `FactIngestContext`-based helpers.
///
/// # Errors
///
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
pub(crate) async fn ingest_fact_command_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedFactWrite,
    embedding_model_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    let options = IngestCoreOptions {
        embedding_model_id,
        citation_plan: CitationPlan::DraftHint,
        // The untyped Fact write: this route carries no `SidecarPayload` at
        // all (`FactIngestPort::ingest_authorized_fact_atomic` refuses one
        // that declares references), so there is no payload to read a scope
        // off and no sidecar row for a scope erase to sweep.
        scopes: &[],
    };
    ingest_core(
        tx,
        AuthorizedFactInput::new(
            authorized.owner_write_permit().owner(),
            authorized.draft(),
            authorized.links(),
        ),
        options,
        &[],
        None,
        None,
        |_tx, _outcome| Box::pin(async { Ok(()) }),
    )
    .await
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
pub(crate) async fn ingest_fact_with_citation_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedFactWithCitation,
    embedding_model_id: Option<&str>,
    sidecar_tables: &[String],
    scopes: &[ScopeFenceTarget],
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
        scopes,
    };
    ingest_core(
        tx,
        AuthorizedFactInput::new(
            authorized.owner_write_permit().owner(),
            draft,
            authorized.links(),
        ),
        options,
        sidecar_tables,
        None,
        None,
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
pub(crate) async fn ingest_fact_with_citation_ref_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    authorized: &AuthorizedFactWithCitationRef,
    embedding_model_id: Option<&str>,
    sidecar_tables: &[String],
    scopes: &[ScopeFenceTarget],
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
        scopes,
    };
    ingest_core(
        tx,
        AuthorizedFactInput::new(
            authorized.owner_write_permit().owner(),
            draft,
            authorized.links(),
        ),
        options,
        sidecar_tables,
        None,
        None,
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

/// Resolve the draft's citation to a `blob_id` per [`CitationPlan`]: a draft
/// hint and an inline object both upsert a blob; a by-ref plan verifies the
/// existing object and returns its id.
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
    owner_id: uuid::Uuid,
    memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let blob_id: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT blob_id FROM proxima_core.memory WHERE t = $1 AND owner_id = $2
             UNION ALL
             SELECT blob_id FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2
             LIMIT 1",
    )
    .bind(memory_id.into_inner())
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(blob_id.flatten())
}

/// Verify a by-ref citation target: the cited object must exist for the
/// Fact's owner and carry exactly the schema the mapping targets. Both
/// failures are caller-fixable `ConstraintViolation`s whose messages the
/// MCP surface passes through verbatim.
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
pub(crate) async fn ingest_fact_with_sidecar_in_tx<F>(
    tx: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedFactWrite,
    embedding_model_id: Option<&str>,
    sidecar_tables: &[String],
    content_id: Option<uuid::Uuid>,
    content_payloads: &[SidecarPayload],
    scopes: &[ScopeFenceTarget],
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
        scopes,
    };
    ingest_core(
        tx,
        AuthorizedFactInput::new(
            authorized.owner_write_permit().owner(),
            draft,
            authorized.links(),
        ),
        options,
        sidecar_tables,
        content_id,
        Some(content_payloads),
        sidecar,
    )
    .await
}

async fn ingest_core<F>(
    tx: &mut Transaction<'_, Postgres>,
    authorized: AuthorizedFactInput<'_>,
    options: IngestCoreOptions<'_>,
    sidecar_tables: &[String],
    content_id: Option<uuid::Uuid>,
    content_payloads: Option<&[SidecarPayload]>,
    sidecar: F,
) -> Result<FactIngestOutcome, StorageError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t FactIngestOutcome,
    ) -> FactIngestSidecarFuture<'t>,
{
    let AuthorizedFactInput {
        owner,
        draft,
        links,
    } = authorized;
    let prepared = super::memory_timeseries::prepare_memory_admission(
        tx,
        owner,
        draft,
        links.origins(),
        links.references(),
        sidecar_tables,
        options.scopes,
    )
    .await?;
    super::memory_timeseries::lock_prepared_memory_admission(tx, &prepared).await?;
    let prepared = super::memory_timeseries::claim_prepared_memory_admission(tx, prepared).await?;
    if let super::memory_timeseries::PreparedMemoryAdmission::Replay(mut outcome) = prepared {
        outcome.cited_object_id =
            citation_object_for_t(tx, owner.stored_owner_id(), outcome.memory_id).await?;
        return Ok(outcome);
    }

    let mut write = draft.clone();
    if write.blob_id.is_none() {
        write.blob_id =
            persist_citation_timeseries(tx, owner, draft, options.citation_plan).await?;
    }
    let content_id = match (content_id, content_payloads) {
        (Some(content_id), _) => Some(content_id),
        (None, Some(payloads)) => {
            crate::verbs::content::ensure_content_from_payloads(
                tx,
                owner.stored_owner_id(),
                draft.schema_id.as_str(),
                payloads,
            )
            .await?
        }
        (None, None) => None,
    };
    let mut outcome = super::memory_timeseries::materialize_prepared_memory_admission(
        tx,
        prepared,
        write.blob_id,
        content_id,
    )
    .await?;
    // Replay reuses the original `(handle, t)`. The sidecar row is already
    // there; inserting again trips `<table>_pkey` on `t`.
    if !outcome.idempotent_replay {
        sidecar(tx, &outcome).await?;
    }
    if outcome.idempotent_replay {
        outcome.cited_object_id =
            citation_object_for_t(tx, owner.stored_owner_id(), outcome.memory_id).await?;
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
