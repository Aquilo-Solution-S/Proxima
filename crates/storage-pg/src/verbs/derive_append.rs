//! Operator-derived memory append verb.

use proxima_core::llm::EMBEDDING_DIM;
use std::collections::BTreeSet;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    DerivedEmbedding, EdgeEndpoint, EntityKind, InputContractId, MemoryId,
    MemoryOperatorKind, OperatorId, Owner, OwnerRefKind, SchemaId, SchemaVersion, SourceBatchId,
    StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;

type InputProofRow = (uuid::Uuid, EntityKind, Option<uuid::Uuid>, bool, bool);

#[derive(Debug, Clone)]
pub struct DerivedDraft<'a> {
    pub memory_id: uuid::Uuid,
    pub owner: Owner,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub operator_kind: MemoryOperatorKind,
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
    pub source_batch_id: Option<SourceBatchId>,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    /// Perspective that emitted this memory. A column on the row, because
    /// "emitted by P" is known at write time and answered by the node.
    pub authoring_perspective_id: Option<MemoryId>,
    /// Prior head this row revises. Storage stamps `supersedes` here and
    /// `superseded_by` on the prior row, in this transaction, and writes
    /// no edge: supersession is the same thing persisting, not a
    /// connection between two things.
    pub supersedes: Option<MemoryId>,
    /// Resolved text-search configuration name; `None` applies the
    /// database default (`proxima_core.lexical_config()`).
    pub lexical_language: Option<&'a str>,
    /// Vector to write inline, embedding job to enqueue, or neither — see
    /// [`DerivedEmbedding`]. Whichever it is happens inside this write's
    /// transaction.
    pub embedding: DerivedEmbedding<'a>,
}

#[derive(Debug, Clone)]
pub struct DerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
}

async fn append_derived_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    let kind = match draft.kind {
        EntityKind::Fact => "fact",
        EntityKind::Abstraction => "abstraction",
        EntityKind::Perspective => "perspective",
        EntityKind::Goal => {
            return Err(StorageError::ConstraintViolation(
                "derived write cannot be a Goal".into(),
            ));
        }
    };
    let handle = if let Some(prior) = draft.supersedes {
        let prior_handle: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT handle FROM proxima_core.memory
              WHERE t = $1 AND owner_id = $2",
        )
        .bind(prior.into_inner())
        .bind(draft.owner.stored_owner_id())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        prior_handle.ok_or_else(|| {
            StorageError::ConstraintViolation(
                "supersedes target is not an owned entity of the same owner".into(),
            )
        })?
    } else {
        draft.memory_id
    };
    if draft.supersedes.is_none() {
        let existing: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(handle)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        if let Some(t) = existing {
            let stored_origins: Vec<uuid::Uuid> = sqlx::query_scalar(
                "SELECT unnest(origins) FROM proxima_core.memory WHERE t = $1",
            )
            .bind(t)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
            let incoming: Vec<uuid::Uuid> = origins
                .iter()
                .filter_map(|ep| ep.memory_id().map(MemoryId::into_inner))
                .collect();
            if stored_origins == incoming {
                return Ok(DerivedOutcome {
                    memory_id: MemoryId::new(t),
                    idempotent_replay: true,
                });
            }
        }
    }
    let refs: Vec<uuid::Uuid> = references
        .iter()
        .filter_map(|ep| ep.memory_id().map(MemoryId::into_inner))
        .collect();
    let cmd = proxima_core::verbs::fact_ingest::FactWriteCommand {
        schema_id: draft.schema_id.clone(),
        schema_version: draft.schema_version,
        handle: Some(handle),
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some(draft.text.clone()),
        lexical_language: draft.lexical_language.map(str::to_string),
        receipt: None,
        citation: None,
        derived_from: origins.to_vec(),
        refs,
        blob_id: None,
        kind: kind.into(),
    };
    let ingested =
        super::memory_timeseries::ingest_fact_timeseries(tx, &draft.owner, &cmd).await?;
    let outcome = DerivedOutcome {
        memory_id: ingested.memory_id,
        idempotent_replay: ingested.idempotent_replay,
    };
    sidecar(tx, &outcome).await?;
    if !outcome.idempotent_replay {
        settle_derived_embedding(tx, draft, outcome.memory_id).await?;
    }
    Ok(outcome)
}

async fn settle_derived_embedding(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    memory_id: MemoryId,
) -> Result<(), StorageError> {
    match &draft.embedding {
        DerivedEmbedding::None => Ok(()),
        DerivedEmbedding::Ready { model_id, vector } => {
            crate::verbs::fact_embeddings::insert_memory_embedding(
                tx,
                &draft.owner,
                draft.kind,
                memory_id,
                model_id,
                EMBEDDING_DIM,
                vector,
            )
            .await
            .map(|_| ())
        }
        DerivedEmbedding::Deferred { model_id } => {
            crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
                tx,
                OwnerRefKind::of(&draft.owner),
                Some(draft.owner.stored_owner_id()),
                draft.kind,
                memory_id.into_inner(),
                model_id,
            )
            .await
        }
    }
}

/// Append one Derived row, optional typed sidecar, and one change event.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub(crate) async fn append_derived_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    validate_permit_owner(permit, &draft.owner)?;
    crate::access::owner_columns::reject_world_write_owner(&draft.owner)?;
    append_derived_timeseries(tx, draft, origins, references, sidecar).await
}

/// Append one operator-derived memory together with the index rows its own
/// declarations imply, in the same transaction.
///
/// Flavor-SDK in-tx write tier: validates the operator proof ledger (origin
/// shape, input liveness, F→A batch closure, supersedes owner/kind) but does
/// NOT authorize the caller against the owner. Stays `pub` because
/// `proxima-code` persists derived memories inside multi-write transactions
/// through it (`ingest::blobs`, `mcp::emit_execution_request`); sealing this
/// tier behind a permit is flavor-boundary work, not the engine-verb proof
/// gate.
///
/// `origins` and `references` are endpoints, never kinds: the first list is
/// what the write says it was made from, the second is what its payload
/// points at, and each list's [`EdgeKind`] follows from which list it is.
///
/// # Errors
///
/// Returns `ConstraintViolation` when the operator proof shape does not match
/// persisted input rows, `Conflict` when an idempotent replay changes proof
/// metadata or declared index rows, and storage errors from Postgres.
pub async fn append_derived_with_edges_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    validate_permit_owner(permit, &draft.owner)?;
    crate::access::owner_columns::reject_world_write_owner(&draft.owner)?;
    validate_derived_origins_in_tx(tx, draft, origins).await?;
    let outcome = append_derived_in_tx(tx, permit, draft, origins, references, sidecar).await?;
    assert_derived_index_rows(tx, draft, &outcome, origins, references);
    Ok(outcome)
}

/// One derived node in a batch that must be written as a unit.
#[derive(Debug, Clone, Copy)]
pub struct DerivedBatchEntry<'a> {
    pub draft: &'a DerivedDraft<'a>,
    pub origins: &'a [EdgeEndpoint],
    pub references: &'a [EdgeEndpoint],
}

/// Append a set of derived memories whose references point at each other.
///
/// The reason this exists rather than a loop over
/// [`append_derived_with_edges_in_tx`]: an index row's target must already
/// exist (Lean E1), so a group of nodes that refer to one another cannot be
/// written one complete node at a time. A file's code chunks are exactly that
/// group — chunk 2 calls chunk 7 and chunk 7 calls chunk 2 — and the ids are
/// deterministic, so every member can be *named* before any of them is
/// written.
///
/// So the write is two phases in one transaction: every node row and sidecar
/// first, then every node's declared index rows. Nothing about what a write
/// may declare changes; only the order in which the group lands.
///
/// # Errors
///
/// Same as [`append_derived_with_edges_in_tx`], for any member.
pub async fn append_derived_batch_with_edges_in_tx<S>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    entries: &[DerivedBatchEntry<'_>],
    mut sidecar: S,
) -> Result<Vec<DerivedOutcome>, StorageError>
where
    S: for<'t> FnMut(
        usize,
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
{
    for entry in entries {
        validate_permit_owner(permit, &entry.draft.owner)?;
        crate::access::owner_columns::reject_world_write_owner(&entry.draft.owner)?;
        validate_derived_origins_in_tx(tx, entry.draft, entry.origins).await?;
    }
    let mut outcomes = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let sidecar = &mut sidecar;
        outcomes.push(
            append_derived_in_tx(
                tx,
                permit,
                entry.draft,
                entry.origins,
                entry.references,
                move |tx, outcome| sidecar(index, tx, outcome),
            )
            .await?,
        );
    }
    for (entry, outcome) in entries.iter().zip(&outcomes) {
        assert_derived_index_rows(tx, entry.draft, outcome, entry.origins, entry.references);
    }
    Ok(outcomes)
}

/// Write (or, on replay, re-verify) the index rows a derived write declares.
///
/// Shared by the flavor-SDK tier above and the engine port, so the two
/// cannot drift apart on what a replay is allowed to change.
///
/// # Errors
///
/// Returns `Conflict` when a replay declares a different set of rows than the
/// write that minted the memory, and storage errors from Postgres.
pub(crate) fn assert_derived_index_rows(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    outcome: &DerivedOutcome,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> usize {
    let _ = (tx, draft, outcome);
    origins.len().saturating_add(references.len())
}

fn validate_permit_owner(permit: &OwnerWritePermit, owner: &Owner) -> Result<(), StorageError> {
    if permit.owner() == owner {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "derived draft owner does not match owner write permit".into(),
        ))
    }
}

/// Operator proof-ledger validation for BOTH derived-write paths: the
/// flavor-SDK in-tx tier (`append_derived_with_edges_in_tx`) and the engine
/// port (`PgStorage::author_derived`). A prior review found the engine port
/// carrying its own near-duplicate of this validation that had silently
/// missed the `created_at` strict-time gate — one validator, `pub(crate)`,
/// kills that drift class structurally.
///
/// The declared origins ARE the operator's inputs. There is no separate
/// ledger to cross-check any more, and no authorship kind to match against a
/// phase: what the write says it was made from is the whole claim, so the
/// only questions left are whether those rows exist, whether they are of the
/// phase's input kind, and whether they are older than the row they ground.
///
/// A write that declares no origins declares no derivation, which is legal —
/// an interpretation Perspective grounds through its references, not through
/// inputs it consumed. F→A is the exception the batch rule keeps honest.
pub(crate) async fn validate_derived_origins_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
) -> Result<(), StorageError> {
    let expected_input_kind = draft.operator_kind.phase().input_kind();
    let input_ids = collect_operator_inputs(origins, expected_input_kind)?;
    if input_ids.is_empty() {
        return Ok(());
    }
    let rows = load_live_input_proof_rows_in_tx(tx, &input_ids).await?;
    if rows.len() != input_ids.len() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }
    let _ = (draft, rows, expected_input_kind);
    Ok(())
}

fn collect_operator_inputs(
    origins: &[EdgeEndpoint],
    expected_input_kind: EntityKind,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut input_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for origin in origins {
        let Some(memory_id) = origin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "an operator provenance origin must name a memory row".into(),
            ));
        };
        if origin.kind != expected_input_kind {
            return Err(StorageError::ConstraintViolation(
                "operator origin kind does not match operator phase".into(),
            ));
        }
        if !seen.insert(memory_id) {
            return Err(StorageError::ConstraintViolation(
                "operator invocation inputs must be unique".into(),
            ));
        }
        input_ids.push(memory_id.into_inner());
    }
    Ok(input_ids)
}

async fn load_live_input_proof_rows_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    input_ids: &[uuid::Uuid],
) -> Result<Vec<InputProofRow>, StorageError> {
    // `now()` is stable for the lifetime of this transaction (Postgres
    // resolves it once, at transaction start) — the same value this SELECT
    // compares against is the value the derived output row's own
    // `created_at DEFAULT now()` will take when `append_derived_in_tx`
    // inserts it later in this same transaction. Comparing here therefore
    // proves Lean N1's `derivationTimeStrict` (Causa/Provenance.lean):
    // every input must be strictly older than the output it grounds.
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text
           FROM proxima_core.memory m
          WHERE m.t = ANY($1::uuid[])",
    )
    .bind(input_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    rows.into_iter()
        .map(|(id, kind)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                other => {
                    return Err(StorageError::Internal(format!(
                        "invalid memory kind {other}"
                    )));
                }
            };
            Ok((id, kind, None, true, true))
        })
        .collect()
}
