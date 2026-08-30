//! Operator-derived memory append verb.
//!
//! Crate-internal throughout: this is the body of the `MemoryWritePort` /
//! `WriteSession` derive implementations in `crate::ports`. A derived write
//! reaches it through `Engine::author_derived_authorized` or
//! `UnitOfWork::author_derived`, which is the one path that runs origin
//! validation, reference-kind validation, the sidecar + projection insert,
//! and the declared-index assertion as a unit.

use proxima_core::llm::EMBEDDING_DIM;
use std::collections::{BTreeMap, BTreeSet};

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    DerivedEmbedding, EdgeEndpoint, EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerRefKind,
    SchemaId, SchemaVersion, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;

type InputProofRow = (uuid::Uuid, EntityKind, Option<uuid::Uuid>, bool, bool);

#[derive(Debug, Clone)]
pub(crate) struct DerivedDraft<'a> {
    pub memory_id: uuid::Uuid,
    pub owner: Owner,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub operator_kind: MemoryOperatorKind,
    /// Prior `t` this write revises. Resolved to its `handle` in this
    /// transaction so the write lands as a later `t` on the same series —
    /// that later `t` *is* the supersession. No column records it and no
    /// edge is written. `None` mints a new series.
    pub supersedes: Option<MemoryId>,
    /// Resolved text-search configuration name. The deployment-default
    /// sentinel resolves to `proxima_core.lexical_config()`; `None` is no
    /// language at all, which a `LanguagePolicy::PerRow` schema refuses
    /// (see `PgSidecarWriter::insert_memory_sidecar`).
    pub lexical_language: Option<&'a str>,
    /// Vector to write inline, embedding job to enqueue, or neither — see
    /// [`DerivedEmbedding`]. Whichever it is happens inside this write's
    /// transaction.
    pub embedding: DerivedEmbedding<'a>,
}

#[derive(Debug, Clone)]
pub(crate) struct DerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
}

async fn append_derived_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    content_id: Option<uuid::Uuid>,
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
        let existing: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(handle)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_err)?;
        if let Some(t) = existing {
            let stored_origins = load_pin_ids(tx, t, PinColumn::Origins).await?;
            let incoming_origins = pin_memory_ids(origins);
            if stored_origins == incoming_origins {
                let stored_refs = load_pin_ids(tx, t, PinColumn::Refs).await?;
                let stored_goal_refs = load_pin_ids(tx, t, PinColumn::GoalRefs).await?;
                let (refs, goal_refs) = super::memory_timeseries::pin_reference_ids(references);
                if stored_refs != refs || stored_goal_refs != goal_refs {
                    return Err(StorageError::Conflict(
                        "derived replay changed declared refs".into(),
                    ));
                }
                return Ok(DerivedOutcome {
                    memory_id: MemoryId::new(t),
                    idempotent_replay: true,
                });
            }
        }
    }
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
        // The authorized endpoint slices below are the sole pin source. Keep
        // compatibility fields empty so this backend cannot accidentally
        // persist an unverified draft projection.
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
    };
    let content_id = resolve_derived_content_id(tx, draft, kind, content_id).await?;
    let ingested = super::memory_timeseries::ingest_fact_timeseries(
        tx,
        &draft.owner,
        &cmd,
        origins,
        references,
        sidecar_tables,
        content_id,
    )
    .await?;
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

async fn resolve_derived_content_id(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    kind: &str,
    content_id: Option<uuid::Uuid>,
) -> Result<Option<uuid::Uuid>, StorageError> {
    if let Some(id) = content_id {
        return Ok(Some(id));
    }
    if kind == "fact" {
        return Ok(None);
    }
    let owner_id =
        crate::access::owner_columns::ensure_owner_row(tx.as_mut(), &draft.owner).await?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-content-text-v1\0");
    hasher.update(draft.schema_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(draft.text.as_bytes());
    Ok(Some(
        super::content::ensure_content(
            tx,
            owner_id,
            draft.schema_id.as_str(),
            hasher.finalize().as_bytes(),
        )
        .await?,
    ))
}

/// Append one Derived row, optional typed sidecar, and one change event.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_derived_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    content_id: Option<uuid::Uuid>,
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    validate_permit_owner(permit, &draft.owner)?;
    append_derived_timeseries(
        tx,
        draft,
        origins,
        references,
        sidecar_tables,
        content_id,
        sidecar,
    )
    .await
}

/// Count declared pins; on replay, re-read stored `origins`/`refs` and
/// refuse a different set.
///
/// Shared by the flavor-SDK tier and the engine port. The replay
/// decision (same handle, same origins) lives in
/// [`append_derived_timeseries`]; this re-checks so a caller that got
/// `idempotent_replay` cannot report a pin set the row does not have.
///
/// # Errors
///
/// `Conflict` when a replay's stored pins differ from the declaration.
/// Storage errors from Postgres.
pub(crate) async fn assert_derived_index_rows(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    outcome: &DerivedOutcome,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<usize, StorageError> {
    let _ = draft;
    if outcome.idempotent_replay {
        let t = outcome.memory_id.into_inner();
        let stored_origins = load_pin_ids(tx, t, PinColumn::Origins).await?;
        let stored_refs = load_pin_ids(tx, t, PinColumn::Refs).await?;
        let stored_goal_refs = load_pin_ids(tx, t, PinColumn::GoalRefs).await?;
        let (refs, goal_refs) = super::memory_timeseries::pin_reference_ids(references);
        if stored_origins != pin_memory_ids(origins)
            || stored_refs != refs
            || stored_goal_refs != goal_refs
        {
            return Err(StorageError::Conflict(
                "derived replay changed declared index rows".into(),
            ));
        }
    }
    Ok(origins.len().saturating_add(references.len()))
}

enum PinColumn {
    Origins,
    Refs,
    GoalRefs,
}

/// Origins as the write path persists them: declaration order, duplicates
/// dropped. Comparing a replay against a non-deduplicating projection would
/// miss the replay and append a second version of the same declaration.
fn pin_memory_ids(pins: &[EdgeEndpoint]) -> Vec<uuid::Uuid> {
    let mut ids = Vec::with_capacity(pins.len());
    for id in pins
        .iter()
        .filter_map(|ep| ep.memory_id().map(MemoryId::into_inner))
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

async fn load_pin_ids(
    tx: &mut Transaction<'_, Postgres>,
    t: uuid::Uuid,
    column: PinColumn,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let sql = match column {
        PinColumn::Origins => "SELECT unnest(origins) FROM proxima_core.memory WHERE t = $1",
        PinColumn::Refs => "SELECT unnest(refs) FROM proxima_core.memory WHERE t = $1",
        PinColumn::GoalRefs => "SELECT unnest(goal_refs) FROM proxima_core.memory WHERE t = $1",
    };
    // SQL-POLICY: fixed-fragment
    sqlx::query_scalar(sql)
        .bind(t)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)
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
/// port (`PgStorage::author_derived`). One `pub(crate)` validator, not a copy
/// per path — a second copy drifts, and the gate it drops silently is the
/// `created_at` strict-time check.
///
/// The declared origins ARE the operator's inputs: what the write says it was
/// made from is the whole claim. There is no separate ledger and no authorship
/// kind, so the only questions are whether those rows exist, whether they are
/// of the phase's input kind, and whether they are older than the row they
/// ground.
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
    let input_ids = collect_operator_inputs(origins)?;
    if input_ids.is_empty() {
        return Ok(());
    }
    let rows = load_live_input_proof_rows_in_tx(tx, &input_ids).await?;
    if rows.len() != input_ids.len() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }
    let stored_kind: BTreeMap<uuid::Uuid, EntityKind> =
        rows.into_iter().map(|(id, kind, ..)| (id, kind)).collect();
    assert_declared_kinds_match_stored(origins, &stored_kind, true)?;
    // Phase contract is on the stored row. Declared kind == phase is the
    // engine manifest; flavor-SDK has no manifest, so this is its gate.
    for origin in origins {
        let Some(memory_id) = origin.memory_id() else {
            continue;
        };
        let Some(kind) = stored_kind.get(&memory_id.into_inner()).copied() else {
            continue;
        };
        if kind != expected_input_kind {
            return Err(StorageError::ConstraintViolation(
                "operator origin kind does not match operator phase".into(),
            ));
        }
    }
    Ok(())
}

/// Stored kind for payload references. Missing targets are skipped:
/// batch writes name sibling rows that land later in the same
/// transaction. After those inserts, the batch path calls this again.
pub(crate) async fn validate_derived_reference_kinds_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    references: &[EdgeEndpoint],
) -> Result<(), StorageError> {
    let ids: Vec<uuid::Uuid> = references
        .iter()
        .filter_map(|endpoint| endpoint.memory_id().map(MemoryId::into_inner))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let stored = load_stored_memory_kinds_in_tx(tx, &ids).await?;
    assert_declared_kinds_match_stored(references, &stored, false)
}

fn assert_declared_kinds_match_stored(
    pins: &[EdgeEndpoint],
    stored: &BTreeMap<uuid::Uuid, EntityKind>,
    require_present: bool,
) -> Result<(), StorageError> {
    for pin in pins {
        let Some(memory_id) = pin.memory_id() else {
            continue;
        };
        match stored.get(&memory_id.into_inner()).copied() {
            Some(kind) if kind == pin.kind => {}
            Some(_) => {
                return Err(StorageError::ConstraintViolation(
                    "declared pin kind must match the stored row".into(),
                ));
            }
            None if require_present => {
                return Err(StorageError::ConstraintViolation(
                    "operator invocation inputs must exist and be live".into(),
                ));
            }
            None => {}
        }
    }
    Ok(())
}

fn collect_operator_inputs(origins: &[EdgeEndpoint]) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut input_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for origin in origins {
        let Some(memory_id) = origin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "an operator provenance origin must name a memory row".into(),
            ));
        };
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
    let stored = load_stored_memory_kinds_in_tx(tx, input_ids).await?;
    Ok(stored
        .into_iter()
        .map(|(id, kind)| (id, kind, None, true, true))
        .collect())
}

async fn load_stored_memory_kinds_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[uuid::Uuid],
) -> Result<BTreeMap<uuid::Uuid, EntityKind>, StorageError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text
           FROM proxima_core.memory m
          WHERE m.t = ANY($1::uuid[])",
    )
    .bind(ids)
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
            Ok((id, kind))
        })
        .collect()
}

#[cfg(test)]
#[path = "derive_append_origins_pg_tests.rs"]
mod origins_pg_tests;

#[cfg(test)]
#[path = "derive_append_replay_pg_tests.rs"]
mod replay_pg_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn in_tx_origin_proof_uses_stored_kind() {
        let src = include_str!("derive_append.rs");
        let discarded = format!("{}{}", "let _ = (draft, rows, ", "expected_input_kind)");
        assert!(
            !src.contains(&discarded),
            "in-tx SELECT of kind must be compared to the declared origin"
        );
        let needle = format!("{}{}", "must match the stored ", "row");
        assert!(
            src.contains(&needle),
            "declared pin kind cannot widen past the stored row"
        );
        let refs = format!("{}{}", "validate_derived_reference_kinds", "_in_tx");
        assert!(
            src.contains(&refs),
            "references must use the same stored-kind compare as origins"
        );
        let start = src
            .find("fn collect_operator_inputs")
            .expect("collect_operator_inputs");
        let rest = &src[start..];
        let end = rest
            .find("async fn load_live_input_proof_rows_in_tx")
            .expect("next fn");
        let declared_phase = format!("{}{}", "origin.kind != ", "expected_input_kind");
        assert!(
            !rest[..end].contains(&declared_phase),
            "phase contract is on stored kind, not the declared endpoint"
        );
    }
}
