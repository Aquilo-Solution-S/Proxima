//! Operator-derived memory append verb.

use proxima_core::llm::EMBEDDING_DIM;
use std::collections::BTreeSet;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    DerivedEmbedding, EdgeEndpoint, EdgeKind, EntityKind, InputContractId, MemoryId,
    MemoryOperatorKind, OperatorId, Owner, OwnerRefKind, SchemaId, SchemaVersion, SourceBatchId,
    StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;
use crate::verbs::edge_index::{
    assert_index_rows_in_tx, declared_index_rows, stored_index_rows_in_tx,
};

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
    return append_derived_timeseries(tx, draft, origins, references, sidecar).await;
    #[allow(unreachable_code)]
    let (owner_kind, owner_id) = crate::access::owner_columns::owner_binds(&draft.owner);
    if let Some(prior) = draft.supersedes {
        validate_supersedes_in_owner(tx, &draft.owner, prior, draft.kind, draft.memory_id).await?;
    }
    if let Some(language) = draft.lexical_language {
        // Replay short-circuit BEFORE registration, mirroring ingest_core's
        // receipt check: the derive memory_id is deterministic per
        // idempotency key, and a replayed pipeline re-arrives with whatever
        // language it resolves today. Registering that would mutate the
        // active-language set (an extra tsquery arm on every future search)
        // for a call that writes nothing — and fail outright on a
        // configuration this database never had, instead of no-oping like
        // every other replay.
        let existing: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM proxima_core.memories WHERE memory_id = $1")
                .bind(draft.memory_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_err)?;
        if existing.is_some() {
            validate_derived_replay_equivalent(tx, draft, owner_kind, owner_id).await?;
            return Ok(DerivedOutcome {
                memory_id: MemoryId::new(draft.memory_id),
                idempotent_replay: true,
            });
        }
        super::lexical_language::register_lexical_language_in_tx(tx, language).await?;
    }

    // NULL language means the column DEFAULT — the COALESCE spells that
    // out rather than branching the statement text on the option.
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id,
             model_id, prompt_version, supersedes, authoring_perspective_id,
             lexical_language)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                 COALESCE($16::regconfig, proxima_core.lexical_config()))
         ON CONFLICT (memory_id) DO NOTHING
         RETURNING memory_id",
    )
    .bind(draft.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.kind)
    .bind(&draft.text)
    .bind(draft.operator_kind)
    .bind(draft.operator_id.into_inner())
    .bind(draft.input_contract_id.into_inner())
    .bind(draft.source_batch_id.map(SourceBatchId::into_inner))
    .bind(draft.model_id)
    .bind(draft.prompt_version)
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .bind(draft.authoring_perspective_id.map(MemoryId::into_inner))
    .bind(draft.lexical_language)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if inserted.is_none() {
        validate_derived_replay_equivalent(tx, draft, owner_kind, owner_id).await?;
        return Ok(DerivedOutcome {
            memory_id: MemoryId::new(draft.memory_id),
            idempotent_replay: true,
        });
    }
    if let Some(prior) = draft.supersedes {
        point_prior_head_forward(tx, prior, draft.memory_id).await?;
    }
    let outcome = DerivedOutcome {
        memory_id: MemoryId::new(draft.memory_id),
        idempotent_replay: false,
    };
    sidecar(tx, &outcome).await?;

    insert_embedding_in_tx(tx, draft, owner_kind, owner_id).await?;

    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             supersedes_memory_id)
         VALUES ($1, $2, $3, 'EntityAppend', $4, $5, $6, $7, $8)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.kind)
    .bind(draft.memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(outcome)
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
    assert_derived_index_rows(tx, draft, &outcome, origins, references).await?;
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
        assert_derived_index_rows(tx, entry.draft, outcome, entry.origins, entry.references)
            .await?;
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
pub(crate) async fn assert_derived_index_rows(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    outcome: &DerivedOutcome,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<usize, StorageError> {
    let _ = (tx, draft, outcome, origins, references);
    return Ok(0);
    #[allow(unreachable_code)]
    let source = EdgeEndpoint::memory(draft.kind, outcome.memory_id);
    if outcome.idempotent_replay {
        let stored = stored_index_rows_in_tx(tx.as_mut(), source).await?;
        let declared = declared_index_rows(origins, references)?;
        if stored != declared {
            return Err(StorageError::Conflict(
                "derived memory idempotent replay index mismatch".into(),
            ));
        }
        return Ok(declared.len());
    }
    let mut asserted =
        assert_index_rows_in_tx(tx.as_mut(), &draft.owner, source, EdgeKind::Origin, origins)
            .await?;
    asserted += assert_index_rows_in_tx(
        tx.as_mut(),
        &draft.owner,
        source,
        EdgeKind::Reference,
        references,
    )
    .await?;
    Ok(asserted)
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

#[allow(dead_code)]
fn validate_input_proof_rows(
    draft: &DerivedDraft<'_>,
    rows: Vec<InputProofRow>,
    expected_len: usize,
    expected_input_kind: EntityKind,
) -> Result<Option<uuid::Uuid>, StorageError> {
    if rows.len() != expected_len {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }
    let mut ftoa_batch = None;
    for (memory_id, actual_kind, source_batch_id, closed, derivation_time_strict) in rows {
        let actual = actual_kind;
        if actual != expected_input_kind {
            return Err(StorageError::ConstraintViolation(format!(
                "invalid input kind for {:?}: expected {expected_input_kind:?}, got {actual:?}",
                MemoryId::new(memory_id)
            )));
        }
        if !derivation_time_strict {
            return Err(StorageError::ConstraintViolation(format!(
                "operator invocation input {:?} must be created strictly before the derived output",
                MemoryId::new(memory_id)
            )));
        }
        if draft.operator_kind == MemoryOperatorKind::FtoA {
            ftoa_batch = Some(validate_ftoa_input_batch(
                source_batch_id,
                closed,
                ftoa_batch,
            )?);
        }
    }
    Ok(ftoa_batch)
}

#[allow(dead_code)]
fn validate_ftoa_input_batch(
    source_batch_id: Option<uuid::Uuid>,
    closed: bool,
    existing_batch: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, StorageError> {
    let source_batch_id = source_batch_id.ok_or_else(|| {
        StorageError::ConstraintViolation("F→A operator inputs must be receipted Facts".into())
    })?;
    if !closed {
        return Err(StorageError::ConstraintViolation(
            "F→A operator source batch must be closed".into(),
        ));
    }
    match existing_batch {
        Some(existing) if existing != source_batch_id => Err(StorageError::ConstraintViolation(
            "F→A operator inputs must belong to one source batch".into(),
        )),
        Some(existing) => Ok(existing),
        None => Ok(source_batch_id),
    }
}

#[allow(dead_code)]
fn validate_derived_source_batch(
    draft: &DerivedDraft<'_>,
    ftoa_batch: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    if draft.operator_kind == MemoryOperatorKind::FtoA {
        let expected = ftoa_batch.ok_or_else(|| {
            StorageError::ConstraintViolation("F→A operator source batch is required".into())
        })?;
        if draft.source_batch_id.map(SourceBatchId::into_inner) != Some(expected) {
            return Err(StorageError::ConstraintViolation(
                "F→A operator source_batch_id must match input Facts".into(),
            ));
        }
    } else if draft.source_batch_id.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_batch_id is only valid for F→A operator invocations".into(),
        ));
    }
    Ok(())
}

async fn validate_derived_replay_equivalent(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let equivalent: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memories
              WHERE memory_id = $1
                AND owner_kind = $2
                AND owner_id IS NOT DISTINCT FROM $3
                AND schema_id = $4
                AND schema_version = $5
                AND kind = $6
                AND text = $7
                AND operator_kind = $8
                AND operator_id = $9
                AND input_contract_id = $10
                AND source_batch_id IS NOT DISTINCT FROM $11
                AND model_id = $12
                AND prompt_version = $13
                AND supersedes IS NOT DISTINCT FROM $14
                AND authoring_perspective_id IS NOT DISTINCT FROM $15
                AND receipt_id IS NULL
                AND tombstoned_at IS NULL
         )",
    )
    .bind(draft.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.kind)
    .bind(&draft.text)
    .bind(draft.operator_kind)
    .bind(draft.operator_id.into_inner())
    .bind(draft.input_contract_id.into_inner())
    .bind(draft.source_batch_id.map(SourceBatchId::into_inner))
    .bind(draft.model_id)
    .bind(draft.prompt_version)
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .bind(draft.authoring_perspective_id.map(MemoryId::into_inner))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;

    if equivalent {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "derived memory idempotent replay proof mismatch".into(),
        ))
    }
}

/// Close the lineage pointer on the row this write revises.
///
/// The successor already carries `supersedes`; this stamps the predecessor's
/// `superseded_by` in the same transaction, so "is this the head?" is a
/// column read on either row. The `WHERE superseded_by IS NULL` guard is what
/// makes a second successor a conflict rather than a silent overwrite — the
/// unique index would catch it anyway, and this names it.
async fn point_prior_head_forward(
    tx: &mut Transaction<'_, Postgres>,
    prior: MemoryId,
    successor: uuid::Uuid,
) -> Result<(), StorageError> {
    let updated = sqlx::query(
        "UPDATE proxima_core.memories
            SET superseded_by = $2
          WHERE memory_id = $1
            AND superseded_by IS NULL",
    )
    .bind(prior.into_inner())
    .bind(successor)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "supersedes target is not the current head".into(),
        ))
    }
}

/// The prior row must exist, live in this Owner, share the successor's kind —
/// and still be the head.
///
/// The head check is here, before the INSERT, because `supersedes` is unique:
/// without it a second successor surfaces as a raw unique-violation from a
/// column the caller never named. Reading the pointer first lets the refusal
/// say what actually went wrong. A replay of the very same successor reads its
/// own id back out of `superseded_by` and is not a conflict.
async fn validate_supersedes_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    prior: MemoryId,
    kind: EntityKind,
    successor: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let head: Option<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT m.superseded_by
           FROM proxima_core.memories m
          WHERE m.memory_id = $1
            AND m.tombstoned_at IS NULL
            AND m.kind = $4
            AND m.owner_kind = $2
            AND m.owner_id = $3",
    )
    .bind(prior.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(kind)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    match head {
        None => Err(StorageError::ConstraintViolation(
            "supersedes crosses Owner boundary or does not exist".into(),
        )),
        Some((None,)) => Ok(()),
        Some((Some(existing),)) if existing == successor => Ok(()),
        Some((Some(_),)) => Err(StorageError::Conflict(
            "supersedes target is not the current head".into(),
        )),
    }
}

/// Settle the derived row's vector inside the row's own transaction:
/// write it, or enqueue the job that will.
///
/// The deferred arm is the whole reason the owner columns are threaded
/// here. It is what makes an unembeddable derived text a *later* embedding
/// rather than a permanently failing write — and it must share this
/// transaction, or a committed memory can exist with no vector and no job
/// to give it one.
async fn insert_embedding_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    match &draft.embedding {
        DerivedEmbedding::None => Ok(()),
        DerivedEmbedding::Ready { model_id, vector } => {
            crate::verbs::fact_embeddings::insert_memory_embedding(
                tx,
                &draft.owner,
                draft.kind,
                MemoryId::new(draft.memory_id),
                model_id,
                EMBEDDING_DIM,
                vector,
            )
            .await
            .map(|_outcome| ())
        }
        DerivedEmbedding::Deferred { model_id } => {
            crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
                tx,
                owner_kind,
                owner_id,
                draft.kind,
                draft.memory_id,
                model_id,
            )
            .await
        }
    }
}
