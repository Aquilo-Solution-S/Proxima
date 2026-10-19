//! Memory timeseries: the admission row every write lands, and the two
//! reads that go straight at it.
//!
//! Crate-internal write and reads: `ingest_fact_timeseries` is the row every
//! governed write path materializes through, so a public door onto it is a
//! second write path — one that skips the sidecar, the projection row, the
//! sketch and the embedding enqueue its callers add. `read_memory_by_t` /
//! `read_memory_head` take the same transaction and exist to assert on that
//! row from inside the crate; the authorized read surface is the query ports.

#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use proxima_core::edge::EdgeEndpoint;
use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{MemoryId, OwnerId, SidecarPayload, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::access::scope_surfaces::ScopeFenceTarget;
use crate::error::{internal, map_err};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemoryRow {
    pub handle: Uuid,
    pub t: Uuid,
    pub kind: String,
    pub owner_id: Uuid,
    pub by: Uuid,
    pub via: Option<Uuid>,
    pub source_id: Option<String>,
    pub ingest_key: Option<String>,
    pub origins: Vec<Uuid>,
    pub refs: Vec<Uuid>,
    pub goal_refs: Vec<Uuid>,
    pub occurred_at: Option<time::OffsetDateTime>,
}

/// The admission a write asks the timeseries to prepare: its owner and
/// authenticated writer, what it says and pins, its declared sidecars, and
/// its flavor-owned lifecycle scopes. These inputs travel together from the
/// authorizing verb to [`prepare_memory_admission_at`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryAdmissionDraft<'a> {
    pub(crate) store_number: u16,
    pub(crate) natural_key: Option<&'a MemoryNaturalKey>,
    pub(crate) owner: &'a OwnerId,
    pub(crate) writer: proxima_core::WriterAttribution,
    pub(crate) draft: &'a FactWriteCommand,
    pub(crate) origins: &'a [EdgeEndpoint],
    pub(crate) references: &'a [EdgeEndpoint],
    /// The declared set forget will dump/delete — tables actually inserted
    /// for this `t`, never the global registry.
    pub(crate) sidecar_tables: &'a [String],
    /// The flavor-owned lifecycle scopes this admission's payloads declare,
    /// already sorted and deduplicated
    /// ([`crate::access::scope_surfaces::ScopeSurfaces::targets_for_payloads`]).
    /// Empty for a payload-less write; a write whose payload declares a
    /// scope and whose caller leaves this empty would admit a row a
    /// concurrent scope erase cannot see.
    pub(crate) scopes: &'a [ScopeFenceTarget],
}

/// Backend-only natural identity, derived from an authorization-bound typed payload.
#[derive(Debug, Clone)]
pub(crate) struct MemoryNaturalKey {
    pub(crate) sidecar_table: String,
    pub(crate) memory_key_column: String,
    pub(crate) columns: Vec<(String, SidecarAtom)>,
    /// The writer asked for a replay whose Fact is no longer its series head
    /// to be admitted again ([`latest_observation`]).
    pub(crate) reobserve_displaced: bool,
}

/// How an admission's owner-scoped `Content` row is resolved: an id the
/// caller already holds, or the typed payloads to derive one from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContentResolution<'a> {
    pub(crate) content_id: Option<Uuid>,
    pub(crate) payloads: Option<&'a [SidecarPayload]>,
}

#[derive(Debug)]
pub(crate) enum PreparedMemoryAdmission {
    Replay(FactIngestOutcome),
    New(Box<PreparedMemoryAdmissionNew>),
}

#[derive(Debug)]
pub(crate) struct PreparedMemoryAdmissionNew {
    owner: OwnerId,
    owner_id: Uuid,
    writer: proxima_core::WriterAttribution,
    draft: FactWriteCommand,
    origins: Vec<Uuid>,
    refs: Vec<Uuid>,
    goal_refs: Vec<Uuid>,
    sidecar_tables: Vec<String>,
    handle: Uuid,
    fresh_handle: bool,
    t: Uuid,
    /// The `memory.occurred_at` this admission stores, resolved once here
    /// and handed unchanged to the row insert and the outbox seal: a Fact's
    /// writer-given time, else its write time read out of `t`. `None` for
    /// an Abstraction or Perspective.
    occurred_at: Option<time::OffsetDateTime>,
    expected_head_t: Option<Uuid>,
    targets: Vec<Uuid>,
}

impl PreparedMemoryAdmission {
    /// The occurrence time a new admission will store; `None` for a replay
    /// (which writes no row) and for an Abstraction or Perspective.
    pub(crate) fn occurred_at(&self) -> Option<time::OffsetDateTime> {
        match self {
            Self::New(prepared) => prepared.occurred_at,
            Self::Replay(_) => None,
        }
    }
}

/// One txn: owners upsert + ingest_keys + memory_head + memory.
///
/// `admission.sidecar_tables` is the declared set forget will dump/delete —
/// tables actually inserted for this `t`, never the global registry.
/// `content_id` is the owner-scoped payload (required for A/P).
pub(crate) async fn ingest_fact_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    let blob_id = admission.draft.blob_id;
    let prepared = prepare_memory_admission(tx, admission).await?;
    lock_prepared_memory_admission(tx, &prepared).await?;
    let prepared = claim_prepared_memory_admission(tx, prepared).await?;
    materialize_prepared_memory_admission(tx, prepared, blob_id, content_id).await
}

/// Prepare ordinary Memory admission without touching content, blob, head, or
/// Memory rows. Replay lookup runs first; a new admission arbitrates owner
/// identity, reserves its real identity, and snapshots its lifecycle footprint
/// before locking that footprint.
pub(crate) async fn prepare_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
) -> Result<PreparedMemoryAdmission, StorageError> {
    prepare_memory_admission_at(
        tx,
        admission,
        PreparationOptions {
            identity: None,
            extra_targets: &[],
        },
    )
    .await
}

/// Prepare a derived admission with its explicit supersedes target included
/// in the lifecycle set without persisting that target as a pin.
pub(crate) async fn prepare_memory_admission_with_extra_targets(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    extra_targets: &[Uuid],
) -> Result<PreparedMemoryAdmission, StorageError> {
    prepare_memory_admission_at(
        tx,
        admission,
        PreparationOptions {
            identity: None,
            extra_targets,
        },
    )
    .await
}

/// Insert an unpinned lifecycle Fact at an identity reserved before a Goal's
/// union lock. The caller already owns the reserved handle and lifecycle
/// union; this helper still uses the same prepare/claim/materialize phases as
/// ordinary admission.
///
/// `sidecar_tables` is the stamp the caller then backs with its sidecar rows
/// in the same transaction: empty for the payload-less write-act Fact, the
/// goal-change table for a goal's change Fact.
///
/// Returns the outcome with the occurrence time the admission stored, which
/// a goal's change Fact publishes; `None` only for a replay.
pub(crate) async fn ingest_unpinned_fact_at(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    writer: proxima_core::WriterAttribution,
    draft: &FactWriteCommand,
    identity: (Uuid, Uuid),
    sidecar_tables: &[String],
) -> Result<(FactIngestOutcome, Option<time::OffsetDateTime>), StorageError> {
    let prepared = prepare_memory_admission_at(
        tx,
        MemoryAdmissionDraft {
            store_number: proxima_core::memory_store_number(identity.1),
            natural_key: None,
            owner,
            writer,
            draft,
            origins: &[],
            references: &[],
            sidecar_tables,
            // A Goal's lifecycle Facts (write-act, goal change) carry no
            // declared scope.
            scopes: &[],
        },
        PreparationOptions {
            identity: Some(identity),
            extra_targets: &[],
        },
    )
    .await?;
    let prepared = match claim_prepared_memory_admission(tx, prepared).await? {
        PreparedMemoryAdmission::New(prepared) => prepared,
        PreparedMemoryAdmission::Replay(outcome) => return Ok((outcome, None)),
    };
    let occurred_at = prepared.occurred_at;
    // Goal writes acquire this reserved handle together with their complete
    // lifecycle union before persistence. Re-entering the Memory lifecycle
    // lock here would acquire a new handle after the Goal `t` locks and invert
    // the cross-entity order.
    let outcome =
        materialize_prepared_memory_admission_after_locks(tx, prepared, draft.blob_id, None)
            .await?;
    Ok((outcome, occurred_at))
}

/// Take every declared scope's fence shared, then run each scope's
/// generated liveness probe under it.
///
/// The probe is the flavor's declaration made executable — `SELECT
/// EXISTS(...)` over the registry table, id column and owner columns the
/// [`proxima_core::ScopeDecl`] names, and nothing storage spelled itself.
/// It runs IN THIS TRANSACTION, under the fence, which is what turns a
/// vanished registry row from a race into a settled refusal.
///
/// # Errors
///
/// [`StorageError::ScopeMissing`] when a scope this write names is no
/// longer registered for `owner`.
async fn fence_declared_scopes(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    scopes: &[ScopeFenceTarget],
) -> Result<(), StorageError> {
    let owner_id = owner.into_inner();
    for scope in scopes {
        crate::access::owner_columns::lock_scope_fence_shared_tx(tx, scope.kind, owner, scope.id)
            .await?;
        // SQL-POLICY: generated
        let registered: bool =
            sqlx::query_scalar(sqlx::AssertSqlSafe(std::sync::Arc::clone(&scope.probe_sql)))
                .bind(owner_id)
                .bind(scope.id)
                .fetch_one(tx.as_mut())
                .await
                .map_err(map_err)?;
        if !registered {
            return Err(StorageError::ScopeMissing {
                kind: scope.kind.as_str().to_owned(),
                id: scope.id,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct PreparationOptions<'a> {
    identity: Option<(Uuid, Uuid)>,
    extra_targets: &'a [Uuid],
}

enum AdmissionReplay {
    Replay(FactIngestOutcome),
    Admit,
    Reobserve { next_key: String },
}

async fn decide_admission_replay(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    owner: &OwnerId,
    owner_id: Uuid,
    draft: &FactWriteCommand,
    refs: &[Uuid],
    goal_refs: &[Uuid],
) -> Result<AdmissionReplay, StorageError> {
    if let Some(request) = draft.request.as_ref() {
        return Ok(
            match super::request_keys::judge_request(tx, owner, request).await? {
                proxima_core::verbs::idempotency::KeyVerdict::Replay(replay) => {
                    AdmissionReplay::Replay(FactIngestOutcome {
                        receipt_id: None,
                        memory_id: MemoryId::new(replay.memory_id),
                        handle: replay.handle,
                        idempotent_replay: true,
                        cited_object_id: None,
                        forgotten: replay.forgotten,
                    })
                }
                proxima_core::verbs::idempotency::KeyVerdict::Fresh => AdmissionReplay::Admit,
            },
        );
    }
    Ok(
        match replay_decision(tx, admission, owner_id, refs, goal_refs).await? {
            ReplayDecision::Replay(replay) => AdmissionReplay::Replay(replay),
            ReplayDecision::Admit => AdmissionReplay::Admit,
            ReplayDecision::Reobserve { next_key } => AdmissionReplay::Reobserve { next_key },
        },
    )
}

async fn prepare_memory_admission_at(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    options: PreparationOptions<'_>,
) -> Result<PreparedMemoryAdmission, StorageError> {
    let MemoryAdmissionDraft { owner, draft, .. } = admission;
    let owner_id = owner.into_inner();

    let kind = validated_admission_kind(draft)?;
    let mut persisted_origins = pin_memory_ids(admission.origins)?;
    let (mut refs, goal_refs) = pin_reference_ids(admission.references);
    // CHECK memory_fact_origins_chk: Facts cannot carry origins. Pins
    // a Fact declares (activation, evidence, prior request) live in refs.
    if kind == "fact" {
        for id in persisted_origins.drain(..) {
            if !refs.contains(&id) {
                refs.push(id);
            }
        }
    }

    let reobservation_key =
        match decide_admission_replay(tx, admission, owner, owner_id, draft, &refs, &goal_refs)
            .await?
        {
            AdmissionReplay::Replay(replay) => return Ok(PreparedMemoryAdmission::Replay(replay)),
            AdmissionReplay::Admit => None,
            AdmissionReplay::Reobserve { next_key } => Some(next_key),
        };

    // A new admission takes the owner fence BEFORE arbitrating the owner row.
    // This is the absent-owner half of the transfer order: a transfer may
    // already hold the destination fence while it creates that row, so an
    // append must wait on the fence rather than insert the row and then wait
    // behind its own uncommitted owner row.
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, owner).await?;
    if let Some(source_id) = draft.source_id.as_deref() {
        crate::access::owner_columns::lock_source_fence_shared_tx(tx, owner, source_id).await?;
    }
    // Then the flavor-declared lifecycle scopes, shared, BEFORE the handle
    // and `t` locks below. That order — owner fence, source fence, scope
    // fence, handle, lifecycle `t`, rows — is what makes a racing scope
    // erase queue behind this write instead of closing a lock cycle with it.
    // Under the fence, "is this scope still registered" has one answer for
    // the life of the transaction: an erase that committed first is visible,
    // and one that has not is waiting.
    fence_declared_scopes(tx, owner, admission.scopes).await?;
    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;
    // These fences are held through commit, so a whole-owner/source erase
    // either waits for this write or observes it in the exact-scope
    // revalidation before deleting anything.

    let (handle, fresh_handle) = admission_handle(tx, admission, options.identity).await?;
    // Read the head BEFORE minting `t`. Minted first, a write could take its
    // `t`, lose the series to a later writer that commits, and then read
    // that writer's head as its own expected head: the head check under the
    // lock would pass, `GREATEST` would keep the other writer's later `t`,
    // and this write would still take the higher publication position.
    // Read first, any writer that commits after this read moves the head
    // away from `expected_head_t`, and `validate_prepared_memory_head`
    // sends this write back to retry with a fresh `t`.
    let expected_head_t: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
            .bind(handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let t: Uuid = if let Some((_, t)) = options.identity {
        t
    } else if let Some(head_t) = expected_head_t {
        proxima_core::new_memory_uuid_after(admission.store_number, head_t).ok_or_else(|| {
            StorageError::ConstraintViolation("memory UUID timestamp range exhausted".into())
        })?
    } else {
        proxima_core::new_memory_uuid(admission.store_number)
    };
    #[cfg(test)]
    crate::pause_point::reach(crate::pause_point::AFTER_SERIES_MINT, handle).await;
    let occurred_at = (kind == "fact").then(|| {
        draft
            .occurred_at
            .unwrap_or_else(|| MemoryId::new(t).created_at())
    });

    let mut targets = Vec::with_capacity(
        1 + usize::from(expected_head_t.is_some())
            + persisted_origins.len()
            + refs.len()
            + goal_refs.len()
            + options.extra_targets.len(),
    );
    targets.push(t);
    if let Some(head_t) = expected_head_t {
        targets.push(head_t);
    }
    targets.extend(persisted_origins.iter().copied());
    targets.extend(refs.iter().copied());
    targets.extend(goal_refs.iter().copied());
    targets.extend(options.extra_targets.iter().copied());
    let mut draft = draft.clone();
    if reobservation_key.is_some() {
        draft.ingest_key = reobservation_key;
    }
    Ok(PreparedMemoryAdmission::New(Box::new(
        PreparedMemoryAdmissionNew {
            owner: *owner,
            owner_id,
            writer: admission.writer,
            draft,
            origins: persisted_origins,
            refs,
            goal_refs,
            sidecar_tables: admission.sidecar_tables.to_vec(),
            handle,
            fresh_handle,
            t,
            occurred_at,
            expected_head_t,
            targets,
        },
    )))
}

/// The kind a draft is stored as. Only a Fact carries a source, an ingest key
/// and an occurrence time; an Abstraction or Perspective that names one is
/// refused here rather than by the table's CHECKs
/// (`memory_fact_occurred_at_chk` for the time).
fn validated_admission_kind(draft: &FactWriteCommand) -> Result<&str, StorageError> {
    if draft.source_id.is_some() != draft.ingest_key.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_id and ingest_key must both be set or both be absent".into(),
        ));
    }
    let kind = if draft.kind.is_empty() {
        "fact"
    } else {
        draft.kind.as_str()
    };
    if !matches!(kind, "fact" | "abstraction" | "perspective") {
        return Err(StorageError::ConstraintViolation(
            "kind must be fact, abstraction, or perspective".into(),
        ));
    }
    if kind != "fact" && (draft.source_id.is_some() || draft.ingest_key.is_some()) {
        return Err(StorageError::ConstraintViolation(
            "A/P cannot carry source_id/ingest_key".into(),
        ));
    }
    if kind != "fact" && draft.occurred_at.is_some() {
        return Err(StorageError::ConstraintViolation(
            "A/P cannot carry occurred_at".into(),
        ));
    }
    Ok(kind)
}

async fn admission_handle(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    reserved: Option<(Uuid, Uuid)>,
) -> Result<(Uuid, bool), StorageError> {
    if let Some(handle) = reserved
        .map(|(handle, _)| handle)
        .or(admission.draft.handle)
    {
        return Ok((handle, reserved.is_some()));
    }
    let Some(key) = admission.natural_key else {
        return Ok((proxima_core::new_memory_uuid(admission.store_number), true));
    };
    Ok(
        match resolve_natural_series_handle(tx, admission.owner, &admission.draft.schema_id, key)
            .await?
        {
            Some(handle) => (handle, false),
            None => (proxima_core::new_memory_uuid(admission.store_number), true),
        },
    )
}

/// Runs after owner/source/scope fences and before handle/lifecycle locks.
async fn resolve_natural_series_handle(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    schema_id: &proxima_core::SchemaId,
    key: &MemoryNaturalKey,
) -> Result<Option<Uuid>, StorageError> {
    let lock = natural_key_lock(owner, schema_id, key);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    let columns = key
        .columns
        .iter()
        .map(|(column, atom)| (column.as_str(), atom.clone()))
        .collect::<Vec<_>>();
    // The series of a natural key outlives forgetting its head (#488 review
    // r3): the lookup runs in the lifecycle scope, so a changed write after
    // forget appends to the forgotten series, as a changed derive does,
    // instead of minting a second series that a restore would leave live
    // beside the first. The forgotten version stays history.
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let handle = super::query::owned_head_handle(
        tx.as_mut(),
        *owner,
        schema_id,
        &key.sidecar_table,
        &key.memory_key_column,
        &columns,
    )
    .await;
    lifecycle.finish(tx.as_mut(), handle).await
}

/// Stable, domain-separated lock key only; never a stored memory or series ID.
fn natural_key_lock(
    owner: &OwnerId,
    schema_id: &proxima_core::SchemaId,
    key: &MemoryNaturalKey,
) -> i64 {
    fn part(hash: &mut blake3::Hasher, bytes: &[u8]) {
        hash.update(&(bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    let mut hash = blake3::Hasher::new();
    part(&mut hash, b"proxima-natural-series-lock-v1");
    part(&mut hash, owner.into_inner().as_bytes());
    part(&mut hash, schema_id.as_str().as_bytes());
    part(&mut hash, key.sidecar_table.as_bytes());
    part(&mut hash, key.memory_key_column.as_bytes());
    for (column, atom) in &key.columns {
        part(&mut hash, column.as_bytes());
        match atom {
            SidecarAtom::Uuid(value) => {
                part(&mut hash, b"uuid");
                part(&mut hash, value.as_bytes());
            }
            SidecarAtom::Text(value) => {
                part(&mut hash, b"text");
                part(&mut hash, value.as_bytes());
            }
            SidecarAtom::Bool(value) => {
                part(&mut hash, b"bool");
                part(&mut hash, &[u8::from(*value)]);
            }
            SidecarAtom::I32(value) => {
                part(&mut hash, b"integer");
                part(&mut hash, &i64::from(*value).to_be_bytes());
            }
            SidecarAtom::I64(value) => {
                part(&mut hash, b"integer");
                part(&mut hash, &value.to_be_bytes());
            }
        }
    }
    let digest = hash.finalize();
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    i64::from_be_bytes(bytes)
}

async fn load_ingest_replay(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    source_id: &str,
    key: &str,
    refs: &[Uuid],
    goal_refs: &[Uuid],
) -> Result<Option<FactIngestOutcome>, StorageError> {
    let Some(replay_t): Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.ingest_keys
          WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3",
    )
    .bind(owner_id)
    .bind(source_id)
    .bind(key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    else {
        return Ok(None);
    };
    // Forget keeps the `ingest_keys` row, so a source re-delivering a
    // forgotten admission still replays: the row is read in the lifecycle
    // scope, which sees it (#488), and the replay says it is forgotten. A
    // row without its declared refs is refused rather than read as empty,
    // which would silently bless a changed declaration.
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let replay_row: Result<Option<ReplayRow>, StorageError> = sqlx::query_as(
        "SELECT handle, refs, goal_refs, forgotten_at IS NOT NULL
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (a re-delivered admission replays as forgotten) */ WHERE t = $1 AND owner_id = $2",
    )
    .bind(replay_t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err);
    let replay_row = lifecycle
        .finish(tx.as_mut(), replay_row)
        .await?
        .ok_or_else(|| internal(format!("ingest key claims t {replay_t} with no memory row")))?;
    let (Some(stored_refs), Some(stored_goal_refs)) = (replay_row.1, replay_row.2) else {
        return Err(StorageError::Conflict(
            "fact replay references are unavailable for this admission".into(),
        ));
    };
    if stored_refs != refs || stored_goal_refs != goal_refs {
        return Err(StorageError::Conflict(
            "fact replay changed declared refs".into(),
        ));
    }
    Ok(Some(FactIngestOutcome {
        receipt_id: None,
        memory_id: MemoryId::new(replay_t),
        idempotent_replay: true,
        cited_object_id: None,
        handle: replay_row.0,
        forgotten: replay_row.3,
    }))
}

/// `(handle, refs, goal_refs, forgotten)` of a replayed admission.
type ReplayRow = (Uuid, Option<Vec<Uuid>>, Option<Vec<Uuid>>, bool);

/// What a sourced admission's replay key resolves to, before any fence.
#[derive(Debug)]
enum ReplayDecision {
    /// Unsourced, or a key nothing has claimed: admit.
    Admit,
    /// The key's admission — or, for a re-observing write, its newest
    /// re-observation — still stands: replay it.
    Replay(FactIngestOutcome),
    /// A re-observing write whose newest admission a later one displaced:
    /// admit it again under `next_key`.
    Reobserve { next_key: String },
}

/// The replay key a sourced admission arrived under, with the pins a replay
/// must match.
#[derive(Debug, Clone, Copy)]
struct IngestReceipt<'a> {
    owner_id: Uuid,
    source_id: &'a str,
    key: &'a str,
    refs: &'a [Uuid],
    goal_refs: &'a [Uuid],
}

async fn replay_decision(
    tx: &mut Transaction<'_, Postgres>,
    admission: MemoryAdmissionDraft<'_>,
    owner_id: Uuid,
    refs: &[Uuid],
    goal_refs: &[Uuid],
) -> Result<ReplayDecision, StorageError> {
    let (Some(source_id), Some(key)) = (
        admission.draft.source_id.as_deref(),
        admission.draft.ingest_key.as_deref(),
    ) else {
        return Ok(ReplayDecision::Admit);
    };
    let Some(replay) = load_ingest_replay(tx, owner_id, source_id, key, refs, goal_refs).await?
    else {
        return Ok(ReplayDecision::Admit);
    };
    if !admission
        .natural_key
        .is_some_and(|natural| natural.reobserve_displaced)
    {
        return Ok(ReplayDecision::Replay(replay));
    }
    let receipt = IngestReceipt {
        owner_id,
        source_id,
        key,
        refs,
        goal_refs,
    };
    latest_observation(tx, receipt, replay).await
}

/// Follow a replayed key's re-observations to the newest admission of the
/// payload, and decide between replaying it and admitting it again.
///
/// Re-observation `n` of `key` claims [`reobservation_key`]`(key, n)`, in
/// order, so the chain has no gaps: every key stays bound to the one `t` it
/// first claimed (docs/concepts.md), and a retried re-observation is
/// found here and replays. The newest admission is displaced when its handle
/// has moved on to another row — the source went back to a state an earlier
/// observation already recorded. A forgotten admission is never admitted
/// again: forget keeps its key replaying.
async fn latest_observation(
    tx: &mut Transaction<'_, Postgres>,
    receipt: IngestReceipt<'_>,
    replay: FactIngestOutcome,
) -> Result<ReplayDecision, StorageError> {
    let mut latest = replay;
    let mut n: u64 = 1;
    loop {
        let next_key = reobservation_key(receipt.key, n);
        if let Some(outcome) = load_ingest_replay(
            tx,
            receipt.owner_id,
            receipt.source_id,
            &next_key,
            receipt.refs,
            receipt.goal_refs,
        )
        .await?
        {
            latest = outcome;
            n += 1;
            continue;
        }
        let displaced: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM proxima_core.memory_head h
                   JOIN proxima_core.memory m /* FORGET-POLICY: live-only (a forgotten memory displaces no head) */
                     ON m.t = $2 AND m.owner_id = $3 AND m.forgotten_at IS NULL
                  WHERE h.handle = $1 AND h.t <> $2
             )",
        )
        .bind(latest.handle)
        .bind(latest.memory_id.into_inner())
        .bind(receipt.owner_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
        return Ok(if displaced {
            ReplayDecision::Reobserve { next_key }
        } else {
            ReplayDecision::Replay(latest)
        });
    }
}

/// The replay key of re-observation `n` of `key`: domain-separated from the
/// source's own keys, and as opaque as the key it extends (docs/concepts.md
/// §Idempotency-key constraint).
fn reobservation_key(key: &str, n: u64) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"proxima-reobservation-v1");
    hash.update(&(key.len() as u64).to_be_bytes());
    hash.update(key.as_bytes());
    hash.update(&n.to_be_bytes());
    hash.finalize().to_hex().to_string()
}

/// Claim the request ledger (when present) and the sourced replay key after
/// the lifecycle set is held. A concurrent loser discovers the replay here,
/// still before citation/content persistence.
///
/// When a tool `idempotency_key` request is present it alone decides: the
/// receipt-key short-circuit is skipped so a revision's natural note key
/// cannot answer as a replay of an older version under a different request.
pub(crate) async fn claim_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedMemoryAdmission,
) -> Result<PreparedMemoryAdmission, StorageError> {
    let PreparedMemoryAdmission::New(prepared) = prepared else {
        return Ok(prepared);
    };
    if let Some(request) = prepared.draft.request.as_ref() {
        match super::request_keys::claim_request(tx, &prepared.owner, request, prepared.t).await? {
            proxima_core::verbs::idempotency::KeyVerdict::Fresh => {}
            proxima_core::verbs::idempotency::KeyVerdict::Replay(replay) => {
                return Ok(PreparedMemoryAdmission::Replay(FactIngestOutcome {
                    receipt_id: None,
                    memory_id: MemoryId::new(replay.memory_id),
                    handle: replay.handle,
                    idempotent_replay: true,
                    cited_object_id: None,
                    forgotten: replay.forgotten,
                }));
            }
        }
        // Request key claimed: do not also short-circuit on the natural
        // receipt key (note_id+title+body+tags). That key is still inserted
        // below only when no request is present.
        return Ok(PreparedMemoryAdmission::New(prepared));
    }
    if let (Some(source_id), Some(key)) = (
        prepared.draft.source_id.as_deref(),
        prepared.draft.ingest_key.as_deref(),
    ) {
        let claimed = sqlx::query(
            "INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (owner_id, source_id, ingest_key) DO NOTHING",
        )
        .bind(prepared.owner_id)
        .bind(source_id)
        .bind(key)
        .bind(prepared.t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        if claimed.rows_affected() == 0 {
            let replay = load_ingest_replay(
                tx,
                prepared.owner_id,
                source_id,
                key,
                &prepared.refs,
                &prepared.goal_refs,
            )
            .await?
            .ok_or_else(|| internal("ingest key conflict had no replay row".to_owned()))?;
            return Ok(PreparedMemoryAdmission::Replay(replay));
        }
    }
    Ok(PreparedMemoryAdmission::New(prepared))
}

pub(crate) async fn lock_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmission,
) -> Result<(), StorageError> {
    if let PreparedMemoryAdmission::New(prepared) = prepared {
        lock_and_validate_prepared_memory(tx, prepared).await?;
    }
    Ok(())
}

async fn lock_and_validate_prepared_memory(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    lock_prepared_memory_targets(tx, prepared).await?;
    validate_prepared_memory_head(tx, prepared).await
}

/// Scope fences first, then the handle lock, then the sorted lifecycle set.
/// Split from [`validate_prepared_memory_head`] so a caller that already owns
/// these locks can still run the validation: the two are separate obligations,
/// and bundling them let the reserved-identity path drop the head check while
/// it was only trying to avoid re-acquiring the locks.
async fn lock_prepared_memory_targets(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, &prepared.owner).await?;
    if let Some(source_id) = prepared.draft.source_id.as_deref() {
        crate::access::owner_columns::lock_source_fence_shared_tx(tx, &prepared.owner, source_id)
            .await?;
    }
    crate::verbs::forget::lock_memory_handles_tx(tx, &[prepared.handle]).await?;
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &prepared.targets).await
}

/// The head this admission prepared against must still be the head under the
/// lock. Run by every materialization path, including callers that acquired
/// the locks themselves.
async fn validate_prepared_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    let current_head: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1 FOR UPDATE")
            .bind(prepared.handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    if current_head != prepared.expected_head_t {
        return Err(StorageError::Retryable(
            "memory series head changed while preparing admission".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn materialize_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedMemoryAdmission,
    blob_id: Option<Uuid>,
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    let prepared = match prepared {
        PreparedMemoryAdmission::New(prepared) => prepared,
        PreparedMemoryAdmission::Replay(outcome) => return Ok(outcome),
    };
    // Keep this materialization primitive safe for every direct crate-internal
    // caller, not only the ordinary prepare/lock/claim wrapper above. The
    // advisory lock is re-entrant in this transaction and precedes all
    // Content/head/Memory persistence below.
    lock_prepared_memory_targets(tx, &prepared).await?;
    materialize_prepared_memory_admission_after_locks(tx, prepared, blob_id, content_id).await
}

/// Materialize an admission after the caller has acquired its complete
/// handle/lifecycle union. The reserved Goal lifecycle Fact uses this path so
/// it cannot acquire a Memory handle after the Goal transaction already holds
/// lifecycle `t` locks. The head validation still runs here — only the lock
/// acquisition is the caller's.
async fn materialize_prepared_memory_admission_after_locks(
    tx: &mut Transaction<'_, Postgres>,
    mut prepared: Box<PreparedMemoryAdmissionNew>,
    blob_id: Option<Uuid>,
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    validate_prepared_memory_head(tx, &prepared).await?;
    if blob_id.is_some() {
        prepared.draft.blob_id = blob_id;
    }
    let kind = admission_kind(&prepared);
    let content_id = resolve_admission_content_id(tx, &prepared, kind, content_id).await?;
    advance_memory_head(tx, &prepared, kind).await?;
    insert_memory_revision(tx, &prepared, kind, content_id).await?;
    refresh_memory_sketch(tx, &prepared, kind).await?;
    Ok(FactIngestOutcome {
        receipt_id: None,
        memory_id: MemoryId::new(prepared.t),
        idempotent_replay: false,
        // Reports the blob this admission actually persisted. The predecessor
        // always returned `None` here and left `ingest_core` to fill it in;
        // every other caller passes a draft with no `blob_id`, so this is a
        // narrowing of that gap rather than a change any caller observes.
        cited_object_id: prepared.draft.blob_id,
        handle: prepared.handle,
        forgotten: false,
    })
}

/// The `memory_kind` label an admission stores; a draft that names none is a
/// Fact.
fn admission_kind(prepared: &PreparedMemoryAdmissionNew) -> &str {
    if prepared.draft.kind.is_empty() {
        "fact"
    } else {
        prepared.draft.kind.as_str()
    }
}

/// The owner-scoped content row this admission stores, materializing one from
/// the draft payload when the caller did not already resolve it. Facts carry
/// their payload inline and need none.
async fn resolve_admission_content_id(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
    kind: &str,
    content_id: Option<Uuid>,
) -> Result<Option<Uuid>, StorageError> {
    if let Some(id) = content_id {
        return Ok(Some(id));
    }
    if kind == "fact" {
        return Ok(None);
    }
    let draft = &prepared.draft;
    let text = draft
        .rendered_text
        .as_deref()
        .map_or(draft.payload.as_slice(), str::as_bytes);
    Ok(Some(
        super::content::ensure_text_content(
            tx,
            prepared.owner_id,
            kind,
            draft.schema_id.as_str(),
            text,
        )
        .await?,
    ))
}

/// Move the series head to this admission's `t`, refusing a handle already
/// held under a different kind, schema or owner.
async fn advance_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
    kind: &str,
) -> Result<(), StorageError> {
    if prepared.fresh_handle {
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t) VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
        )
        .bind(prepared.handle)
        .bind(kind)
        .bind(prepared.draft.schema_id.as_str())
        .bind(prepared.owner_id)
        .bind(prepared.t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        return Ok(());
    }
    // Preparation mints strictly above the expected head, and the locked
    // comparison rejects a changed head. Reserved lifecycle identities use
    // fresh handles. A successful admission therefore becomes the head even
    // when another process minted its predecessor with a clock ahead of ours.
    let head = sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.memory_head.kind = EXCLUDED.kind
           AND proxima_core.memory_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.memory_head.owner_id = EXCLUDED.owner_id
         RETURNING handle",
    )
    .bind(prepared.handle)
    .bind(kind)
    .bind(prepared.draft.schema_id.as_str())
    .bind(prepared.owner_id)
    .bind(prepared.t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        return Err(StorageError::ConstraintViolation(
            "memory_head kind/schema/owner mismatch".into(),
        ));
    }
    Ok(())
}

/// The language a Memory row is stamped with: the draft's own, or `None`
/// (the deployment configuration) when it named none or asked for the
/// deployment default. The projection row's language follows the same
/// draft, so a rebuild from the Memory row projects the words alike.
fn stamped_language(draft_language: Option<&str>) -> Option<&str> {
    draft_language.filter(|language| {
        *language != proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT
    })
}

/// Append the immutable Memory revision row itself.
async fn insert_memory_revision(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
    kind: &str,
    content_id: Option<Uuid>,
) -> Result<(), StorageError> {
    let draft = &prepared.draft;
    sqlx::query(
        "INSERT INTO proxima_core.memory /* FORGET-POLICY: includes-forgotten (a new row is live; the insert reads no other row) */
            (handle, t, kind, owner_id, schema_id, source_id, ingest_key, blob_id,
             content_id, origins, refs, goal_refs, sidecar_tables, occurred_at, \"by\", via,
             lexical_language)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16,
                 COALESCE($17::regconfig, proxima_core.lexical_config()))",
    )
    .bind(prepared.handle)
    .bind(prepared.t)
    .bind(kind)
    .bind(prepared.owner_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.source_id.as_deref())
    .bind(draft.ingest_key.as_deref())
    .bind(draft.blob_id)
    .bind(content_id)
    .bind(&prepared.origins)
    .bind(&prepared.refs)
    .bind(&prepared.goal_refs)
    .bind(&prepared.sidecar_tables)
    .bind(prepared.occurred_at)
    .bind(prepared.writer.by)
    .bind(prepared.writer.via)
    .bind(stamped_language(draft.lexical_language.as_deref()))
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Refresh the Memory sketch after the admission.
async fn refresh_memory_sketch(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
    kind: &str,
) -> Result<(), StorageError> {
    super::sketch::upsert_sketch(
        tx,
        prepared.owner_id,
        prepared.t,
        kind,
        &super::sketch::sketch_line(kind, prepared.draft.rendered_text.as_deref(), &[]),
    )
    .await?;

    Ok(())
}

/// Project authorized endpoints to the UUID array stored by Postgres while
/// preserving declaration order and removing duplicate pins. Origins are a
/// Memory-only column; a Goal origin is a malformed authorized carrier and
/// must not be silently discarded.
fn pin_memory_ids(pins: &[EdgeEndpoint]) -> Result<Vec<Uuid>, StorageError> {
    let mut ids = Vec::with_capacity(pins.len());
    for pin in pins {
        let Some(memory_id) = pin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "memory origins must target a Memory".into(),
            ));
        };
        let id = memory_id.into_inner();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Split reference endpoints into the two columns that now carry them:
/// `memory.refs` for Memory targets, `memory.goal_refs` for Goal targets.
/// The column is the type — the trigger checks each against its own spine —
/// so the endpoint kind survives persistence instead of being re-derived by
/// every reader. Declaration order is preserved within each column and
/// duplicate pins are dropped.
pub(crate) fn pin_reference_ids(pins: &[EdgeEndpoint]) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut memory_ids = Vec::with_capacity(pins.len());
    let mut goal_ids = Vec::new();
    for pin in pins {
        let id = pin.entity_id();
        let column = if matches!(pin.entity, proxima_core::EntityRef::Goal(_)) {
            &mut goal_ids
        } else {
            &mut memory_ids
        };
        if !column.contains(&id) {
            column.push(id);
        }
    }
    (memory_ids, goal_ids)
}

/// Read one admission row by `t`, inside the caller's transaction.
///
/// Test-only, and the gate says so rather than a comment: the only consumer
/// is the suite below, which asserts on the row a write just landed before
/// that write commits. The authorized read surface is the query ports; this
/// reaches the table directly and must not become a second one.
#[cfg(test)]
pub(crate) async fn read_memory_by_t(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT handle, t, kind::text, owner_id, \"by\", via, source_id, ingest_key, origins, refs, goal_refs,
                occurred_at
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (test helper: the row the test just wrote) */
          WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

/// Read a series' current head row, inside the caller's transaction.
/// Test-only for the same reason as [`read_memory_by_t`].
#[cfg(test)]
pub(crate) async fn read_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT m.handle, m.t, m.kind::text, m.owner_id, m.\"by\", m.via, m.source_id, m.ingest_key,
                m.origins, m.refs, m.goal_refs, m.occurred_at
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m /* FORGET-POLICY: includes-forgotten (test helper: the head the test just wrote) */ ON m.handle = h.handle AND m.t = h.t
          WHERE h.handle = $1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

// The admission-row suite. Crate-internal for the same reason the verb is:
// it reads the row back through `read_memory_by_t` / `read_memory_head`
// inside the write's own transaction, which is not something the read ports
// offer and should not be.
#[cfg(test)]
#[path = "memory_timeseries_pg_tests.rs"]
mod memory_timeseries_pg_tests;
