//! Operator-derived memory append verb.
//!
//! Crate-internal throughout: this is the body of the `MemoryWritePort` /
//! `WriteSession` derive implementations in `crate::ports`. A derived write
//! reaches it through `Engine::derive_memory` or
//! `UnitOfWork::derive_memory`, which is the one path that runs origin
//! validation, reference-kind validation, the sidecar + projection insert,
//! and the declared-index assertion as a unit.

use std::collections::{BTreeMap, BTreeSet};

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    EdgeEndpoint, EdgeKind, EntityKind, MemoryId, MemoryOperatorKind, OwnerId, SchemaId,
    SchemaVersion, SidecarPayload, StorageError,
};
use sqlx::{Acquire, Postgres, Transaction};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;

type InputProofRow = (uuid::Uuid, EntityKind, Option<uuid::Uuid>, bool, bool);

#[derive(Debug, Clone)]
pub(crate) struct DerivedDraft<'a> {
    pub(crate) store_number: u16,
    pub memory_id: uuid::Uuid,
    pub owner: OwnerId,
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
    /// Spaces selected by the schema and write route. Jobs are queued in
    /// this memory transaction; an empty slice requests no work.
    pub queued_spaces: &'a [proxima_core::EmbeddingSpace],
    /// Tool `idempotency_key` ledger entry, when the caller supplied one.
    pub request: Option<proxima_core::verbs::idempotency::RequestKey>,
}

impl<'a> DerivedDraft<'a> {
    pub(crate) fn from_request(
        req: &proxima_core::AuthorDerivedRequest<'a>,
        store_number: u16,
    ) -> Self {
        Self {
            store_number,
            memory_id: req.memory_id.into_inner(),
            owner: req.owner,
            kind: req.kind,
            schema_id: req.schema_id.clone(),
            schema_version: req.schema_version,
            text: req.text.clone(),
            operator_kind: req.operator_kind,
            supersedes: req.supersedes,
            lexical_language: req.lexical_language,
            queued_spaces: req.queued_spaces,
            request: req.request.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    /// The replay matched a forgotten memory: nothing was written and it
    /// stays forgotten.
    pub forgotten: bool,
}

/// Content resolution is shared with the Fact routes and lives with the
/// admission row it resolves against.
pub(crate) use super::memory_timeseries::ContentResolution;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DerivedAdmissionInput<'a> {
    pub(crate) origins: &'a [EdgeEndpoint],
    pub(crate) references: &'a [EdgeEndpoint],
    pub(crate) sidecar_tables: &'a [String],
    /// The flavor-declared lifecycle scopes this derived payload belongs to,
    /// already sorted and deduplicated. A derived Abstraction or Perspective
    /// is as much an admission into its scope as a Fact is, so it takes the
    /// same fence in the same place.
    pub(crate) scopes: &'a [crate::access::scope_surfaces::ScopeFenceTarget],
    pub(crate) content: ContentResolution<'a>,
}

async fn append_derived_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    writer: proxima_core::WriterAttribution,
    input: DerivedAdmissionInput<'_>,
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    let mut retried_collision = false;
    let outcome = loop {
        let mut attempt = tx.begin().await.map_err(map_err)?;
        match admit_derived(&mut attempt, draft, writer, input).await {
            Ok(outcome) => {
                attempt.commit().await.map_err(map_err)?;
                break outcome;
            }
            Err(StorageError::MemoryIdCollision) if !retried_collision => {
                attempt.rollback().await.map_err(map_err)?;
                retried_collision = true;
            }
            Err(StorageError::MemoryIdCollision) => return Err(crate::error::collision_exhausted()),
            Err(error) => return Err(error),
        }
    };
    if outcome.idempotent_replay {
        return Ok(outcome);
    }
    sidecar(tx, &outcome).await?;
    if !outcome.idempotent_replay {
        enqueue_derived_embedding_jobs(tx, draft, outcome.memory_id).await?;
    }
    Ok(outcome)
}

fn derived_kind(draft: &DerivedDraft<'_>) -> Result<&'static str, StorageError> {
    match draft.kind {
        EntityKind::Fact => Ok("fact"),
        EntityKind::Abstraction => Ok("abstraction"),
        EntityKind::Perspective => Ok("perspective"),
        EntityKind::Goal => Err(StorageError::ConstraintViolation(
            "derived write cannot be a Goal".into(),
        )),
    }
}

async fn admit_derived(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    writer: proxima_core::WriterAttribution,
    input: DerivedAdmissionInput<'_>,
) -> Result<DerivedOutcome, StorageError> {
    let kind = derived_kind(draft)?;
    let handle = if let Some(prior) = draft.supersedes {
        let prior_handle: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: live-only (a write never builds on a forgotten memory; owner RLS hides it too) */
              WHERE t = $1 AND owner_id = $2 AND forgotten_at IS NULL",
        )
        .bind(prior.into_inner())
        .bind(draft.owner.into_inner())
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
    if let Some(request) = draft.request.as_ref() {
        match super::request_keys::judge_request(tx, &draft.owner, request).await? {
            proxima_core::verbs::idempotency::KeyVerdict::Replay(replay) => {
                return Ok(DerivedOutcome {
                    memory_id: proxima_core::MemoryId::new(replay.memory_id),
                    idempotent_replay: true,
                    forgotten: replay.forgotten,
                });
            }
            proxima_core::verbs::idempotency::KeyVerdict::Fresh => {}
        }
    } else if let Some(replay) =
        try_derived_replay(tx, handle, draft, kind, input.origins, input.references).await?
    {
        return Ok(replay);
    }
    let mut cmd = derived_memory_command(draft, handle, kind);
    if let Some(request) = draft.request.as_ref() {
        cmd = cmd.request_key(request.clone());
    }
    let supersedes_targets = draft
        .supersedes
        .map(MemoryId::into_inner)
        .into_iter()
        .collect::<Vec<_>>();
    let prepared = super::memory_timeseries::prepare_memory_admission_with_extra_targets(
        tx,
        super::memory_timeseries::MemoryAdmissionDraft {
            store_number: draft.store_number,
            natural_key: None,
            owner: &draft.owner,
            writer,
            draft: &cmd,
            origins: input.origins,
            references: input.references,
            sidecar_tables: input.sidecar_tables,
            scopes: input.scopes,
        },
        &supersedes_targets,
    )
    .await?;
    super::memory_timeseries::lock_prepared_memory_admission(tx, &prepared).await?;
    // A concurrent writer can commit between the first replay probe and
    // preparation. Preparation then observes its head and legitimately
    // passes the locked head comparison. Recheck under the existing fences
    // and handle/lifecycle locks before materializing another version.
    // When a request key is present the ledger (via claim) is the authority.
    if draft.request.is_none()
        && let Some(replay) =
            try_derived_replay(tx, handle, draft, kind, input.origins, input.references).await?
    {
        return Ok(replay);
    }
    let prepared = super::memory_timeseries::claim_prepared_memory_admission(tx, prepared).await?;
    let content_id = resolve_derived_content_id(
        tx,
        draft,
        kind,
        input.content.content_id,
        input.content.payloads,
    )
    .await?;
    let ingested = super::memory_timeseries::materialize_prepared_memory_admission(
        tx, prepared, None, content_id,
    )
    .await?;
    let outcome = DerivedOutcome {
        memory_id: ingested.memory_id,
        idempotent_replay: ingested.idempotent_replay,
        forgotten: false,
    };
    Ok(outcome)
}

/// A replay of the write that made the series head answers with that head,
/// also when it is forgotten, and a replay of a forgotten older version
/// answers with that version: the probe always runs in the lifecycle scope,
/// so forgotten rows' pins are compared like live ones, and a match
/// answers `forgotten` without writing (#488 "a replay answers, it does not
/// revive"). Without the scope the forgotten row's pins read as empty, the
/// comparison failed, and the write appended a new live version.
async fn try_derived_replay(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    draft: &DerivedDraft<'_>,
    kind: &str,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<Option<DerivedOutcome>, StorageError> {
    if draft.supersedes.is_some() {
        return Ok(None);
    }
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let result = derived_replay_in_lifecycle(tx, handle, draft, kind, origins, references).await;
    lifecycle.finish(tx.as_mut(), result).await
}

async fn derived_replay_in_lifecycle(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    draft: &DerivedDraft<'_>,
    kind: &str,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<Option<DerivedOutcome>, StorageError> {
    {
        let existing = load_derived_replay_head(tx, handle, draft, kind).await?;
        if let Some((t, forgotten)) = existing {
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
                return Ok(Some(DerivedOutcome {
                    memory_id: MemoryId::new(t),
                    idempotent_replay: true,
                    forgotten,
                }));
            }
        }
    }
    // Forget moves the head back to the newest live version, so a replay of
    // a forgotten older-than-head write no longer meets it at the head
    // (#488 review r3). It still answers: any forgotten version with the
    // same pins is that write. A live non-head version that matches is not
    // a replay; re-deriving it appends a version that becomes head, as
    // before #488.
    let incoming_origins = pin_memory_ids(origins);
    let forgotten_version: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (a replay answers for a forgotten version) */
          WHERE handle = $1 AND owner_id = $2
            AND kind = $3::proxima_core.memory_kind AND schema_id = $4
            AND forgotten_at IS NOT NULL AND origins = $5::uuid[]
          ORDER BY t DESC
          LIMIT 1",
    )
    .bind(handle)
    .bind(draft.owner.into_inner())
    .bind(kind)
    .bind(draft.schema_id.as_str())
    .bind(&incoming_origins)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    if let Some(t) = forgotten_version {
        let stored_refs = load_pin_ids(tx, t, PinColumn::Refs).await?;
        let stored_goal_refs = load_pin_ids(tx, t, PinColumn::GoalRefs).await?;
        let (refs, goal_refs) = super::memory_timeseries::pin_reference_ids(references);
        if stored_refs != refs || stored_goal_refs != goal_refs {
            return Err(StorageError::Conflict(
                "derived replay changed declared refs".into(),
            ));
        }
        return Ok(Some(DerivedOutcome {
            memory_id: MemoryId::new(t),
            idempotent_replay: true,
            forgotten: true,
        }));
    }
    Ok(None)
}

async fn load_derived_replay_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    draft: &DerivedDraft<'_>,
    kind: &str,
) -> Result<Option<(uuid::Uuid, bool)>, StorageError> {
    sqlx::query_as(
        "SELECT h.t, m.forgotten_at IS NOT NULL
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m /* FORGET-POLICY: includes-forgotten (a replay answers for a forgotten head) */ ON m.t = h.t
          WHERE h.handle = $1 AND h.owner_id = $2
            AND h.kind = $3::proxima_core.memory_kind AND h.schema_id = $4",
    )
    .bind(handle)
    .bind(draft.owner.into_inner())
    .bind(kind)
    .bind(draft.schema_id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)
}

fn derived_memory_command(
    draft: &DerivedDraft<'_>,
    handle: uuid::Uuid,
    kind: &str,
) -> proxima_core::verbs::fact_ingest::FactWriteCommand {
    proxima_core::verbs::fact_ingest::FactWriteCommand {
        schema_id: draft.schema_id.clone(),
        schema_version: draft.schema_version,
        handle: Some(handle),
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some(draft.text.clone()),
        lexical_language: draft.lexical_language.map(str::to_string),
        receipt: None,
        occurred_at: None,
        citation: None,
        // The authorized endpoint slices below are the sole pin source. Keep
        // compatibility fields empty so this backend cannot accidentally
        // persist an unverified draft projection.
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
        request: None,
    }
}

async fn enqueue_derived_embedding_jobs(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    memory_id: MemoryId,
) -> Result<(), StorageError> {
    crate::verbs::fact_embeddings::enqueue_embedding_jobs_in_tx(
        tx,
        draft.owner.into_inner(),
        memory_id.into_inner(),
        draft.queued_spaces,
    )
    .await
}

async fn resolve_derived_content_id(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    kind: &str,
    content_id: Option<uuid::Uuid>,
    content_payloads: Option<&[SidecarPayload]>,
) -> Result<Option<uuid::Uuid>, StorageError> {
    if let Some(id) = content_id {
        return Ok(Some(id));
    }
    if kind == "fact" {
        return Ok(None);
    }
    let owner_id =
        crate::access::owner_columns::ensure_owner_row(tx.as_mut(), &draft.owner).await?;
    if let Some(payloads) = content_payloads {
        return super::content::ensure_content_from_payloads(
            tx,
            owner_id,
            kind,
            draft.schema_id.as_str(),
            payloads,
        )
        .await;
    }
    Ok(Some(
        super::content::ensure_text_content(
            tx,
            owner_id,
            kind,
            draft.schema_id.as_str(),
            draft.text.as_bytes(),
        )
        .await?,
    ))
}

/// Append one Derived row, optional typed sidecar, and one change event,
/// resolving typed Content from the admission's payloads after the complete
/// lifecycle target set is held.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub(crate) async fn append_derived_with_content_payloads_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    input: DerivedAdmissionInput<'_>,
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    validate_permit_owner(permit, &draft.owner)?;
    let writer = crate::owner_scope::require_owner_scope(permit.owner_scope())?.writer();
    append_derived_timeseries(tx, draft, writer, input, sidecar).await
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
        // A forgotten memory's row is hidden from owner reads; its pins are
        // re-read in the lifecycle scope, as the replay probe read them.
        let (stored_origins, stored_refs, stored_goal_refs) = if outcome.forgotten {
            let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
            let stored = load_stored_pins(tx, t).await;
            lifecycle.finish(tx.as_mut(), stored).await?
        } else {
            load_stored_pins(tx, t).await?
        };
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

type StoredPins = (Vec<uuid::Uuid>, Vec<uuid::Uuid>, Vec<uuid::Uuid>);

async fn load_stored_pins(
    tx: &mut Transaction<'_, Postgres>,
    t: uuid::Uuid,
) -> Result<StoredPins, StorageError> {
    Ok((
        load_pin_ids(tx, t, PinColumn::Origins).await?,
        load_pin_ids(tx, t, PinColumn::Refs).await?,
        load_pin_ids(tx, t, PinColumn::GoalRefs).await?,
    ))
}

/// Record the other-store edges of a fresh derived row (`origin` for its
/// remote sources, `reference` for its remote subjects, #480), or check a
/// replay declares the same rows, kinds included, as its first write. No
/// constraint looks at the remote side.
///
/// # Errors
///
/// `Conflict` when a replay's stored external edges differ from the
/// declaration. Storage errors from Postgres.
pub(crate) async fn record_external_edges(
    tx: &mut Transaction<'_, Postgres>,
    outcome: &DerivedOutcome,
    origins: &[uuid::Uuid],
    references: &[uuid::Uuid],
) -> Result<usize, StorageError> {
    let t = outcome.memory_id.into_inner();
    let declared: BTreeSet<(uuid::Uuid, String)> = origins
        .iter()
        .map(|id| (*id, EdgeKind::Origin.as_str().to_owned()))
        .chain(
            references
                .iter()
                .map(|id| (*id, EdgeKind::Reference.as_str().to_owned())),
        )
        .collect();
    if outcome.idempotent_replay {
        // A forgotten memory's edges are hidden from owner reads; the replay
        // compares them in the lifecycle scope, as it compared the pins.
        let lifecycle = if outcome.forgotten {
            Some(crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?)
        } else {
            None
        };
        let stored: Result<Vec<(uuid::Uuid, String)>, StorageError> = sqlx::query_as(
            "SELECT memory, edge_kind FROM proxima_core.memory_external_origin WHERE t = $1",
        )
        .bind(t)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err);
        let stored = match lifecycle {
            Some(lifecycle) => lifecycle.finish(tx.as_mut(), stored).await?,
            None => stored?,
        };
        let stored: BTreeSet<(uuid::Uuid, String)> = stored.into_iter().collect();
        if stored != declared {
            return Err(StorageError::Conflict(
                "derived replay changed declared external edges".into(),
            ));
        }
        return Ok(declared.len());
    }
    if declared.is_empty() {
        return Ok(0);
    }
    let (memories, kinds): (Vec<uuid::Uuid>, Vec<String>) = declared.iter().cloned().unzip();
    sqlx::query(
        "INSERT INTO proxima_core.memory_external_origin (t, memory, edge_kind)
         SELECT $1, edge.memory, edge.kind
           FROM unnest($2::uuid[], $3::text[]) AS edge(memory, kind)
         ON CONFLICT DO NOTHING",
    )
    .bind(t)
    .bind(&memories)
    .bind(&kinds)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(declared.len())
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
        PinColumn::Origins => {
            "SELECT unnest(origins) FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (a replay compares a forgotten head's pins in the lifecycle scope) */ WHERE t = $1"
        }
        PinColumn::Refs => {
            "SELECT unnest(refs) FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (a replay compares a forgotten head's pins in the lifecycle scope) */ WHERE t = $1"
        }
        PinColumn::GoalRefs => {
            "SELECT unnest(goal_refs) FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (a replay compares a forgotten head's pins in the lifecycle scope) */ WHERE t = $1"
        }
    };
    // SQL-POLICY: fixed-fragment
    sqlx::query_scalar(sql)
        .bind(t)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)
}

fn validate_permit_owner(permit: &OwnerWritePermit, owner: &OwnerId) -> Result<(), StorageError> {
    if permit.owner() == owner {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "derived draft owner does not match owner write permit".into(),
        ))
    }
}

/// Shared origin validation for the storage and write-session authoring paths.
///
/// Each origin must name a unique memory row present in `proxima_core.memory`.
/// Its declared kind must match the stored kind and the operator phase's input
/// kind.
///
/// Empty origins declare no derivation and pass this validator. An
/// interpretation Perspective can ground through its references.
pub(crate) async fn validate_derived_origins_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<(), StorageError> {
    let expected_input_kind = draft.operator_kind.phase().input_kind();
    let input_ids = collect_operator_inputs(origins)?;
    if input_ids.is_empty() {
        return Ok(());
    }
    let mut rows = load_live_input_proof_rows_in_tx(tx, &input_ids).await?;
    // A replay answers even when an input was forgotten after the first
    // write (#488 review r3): the write is not repeated, so nothing new is
    // built on the forgotten input. Its inputs are proved over every row,
    // and the stored-kind checks below still apply. Any other write keeps
    // the live-only proof.
    if rows.len() != input_ids.len()
        && replays_an_existing_version(tx, draft, origins, references).await?
    {
        rows = load_replay_input_proof_rows_in_tx(tx, &input_ids).await?;
    }
    if rows.len() != input_ids.len() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }
    let stored_kind: BTreeMap<uuid::Uuid, EntityKind> =
        rows.into_iter().map(|(id, kind, ..)| (id, kind)).collect();
    assert_declared_kinds_match_stored(origins, &stored_kind)?;
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

/// Payload reference proof. Like the origin proof it is live-only: every
/// memory reference must name a live row this transaction can read, and its
/// declared kind must match the stored kind. A nonexistent, forgotten or
/// foreign (hidden by owner RLS) reference is refused.
///
/// The one exception is a replay of an existing version (#488 review r4):
/// the replay writes nothing, so it answers even when a reference was
/// forgotten since, as for origins. Its references are then proved over
/// every row in the lifecycle scope, and the kind check still applies.
pub(crate) async fn validate_derived_reference_kinds_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<(), StorageError> {
    let ids: Vec<uuid::Uuid> = references
        .iter()
        .filter_map(|endpoint| endpoint.memory_id().map(MemoryId::into_inner))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let mut stored = load_stored_memory_kinds_in_tx(tx, &ids).await?;
    if stored.len() != ids.len()
        && replays_an_existing_version(tx, draft, origins, references).await?
    {
        stored = load_replay_input_proof_rows_in_tx(tx, &ids)
            .await?
            .into_iter()
            .map(|(id, kind, ..)| (id, kind))
            .collect();
    }
    assert_declared_kinds_match_stored(references, &stored)
}

fn assert_declared_kinds_match_stored(
    pins: &[EdgeEndpoint],
    stored: &BTreeMap<uuid::Uuid, EntityKind>,
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
            None => {
                return Err(StorageError::ConstraintViolation(
                    "operator invocation inputs must exist and be live".into(),
                ));
            }
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

async fn replays_an_existing_version(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<bool, StorageError> {
    let kind = derived_kind(draft)?;
    Ok(
        try_derived_replay(tx, draft.memory_id, draft, kind, origins, references)
            .await?
            .is_some(),
    )
}

async fn load_replay_input_proof_rows_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    input_ids: &[uuid::Uuid],
) -> Result<Vec<InputProofRow>, StorageError> {
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let rows: Result<Vec<(uuid::Uuid, String)>, StorageError> = sqlx::query_as(
        "SELECT m.t, m.kind::text
           FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (a replay's inputs may have been forgotten since) */
          WHERE m.t = ANY($1::uuid[])",
    )
    .bind(input_ids)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err);
    let rows = lifecycle.finish(tx.as_mut(), rows).await?;
    rows.into_iter()
        .map(|(id, kind)| Ok((id, parse_stored_kind(&kind)?, None, true, true)))
        .collect()
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
           FROM proxima_core.memory m /* FORGET-POLICY: live-only (a write never builds on a forgotten memory; owner RLS hides it too) */
          WHERE m.t = ANY($1::uuid[]) AND m.forgotten_at IS NULL",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    rows.into_iter()
        .map(|(id, kind)| Ok((id, parse_stored_kind(&kind)?)))
        .collect()
}

fn parse_stored_kind(kind: &str) -> Result<EntityKind, StorageError> {
    match kind {
        "fact" => Ok(EntityKind::Fact),
        "abstraction" => Ok(EntityKind::Abstraction),
        "perspective" => Ok(EntityKind::Perspective),
        other => Err(StorageError::Internal(format!(
            "invalid memory kind {other}"
        ))),
    }
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
