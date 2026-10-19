//! Goal timeseries write.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use proxima_core::publication::PublicationStamp;
use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    FactPayload, GoalChange, GoalChangeV1, GoalId, OwnerId, SchemaVersion, SidecarPayload,
    StorageError,
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::memory_timeseries::{ingest_fact_timeseries, ingest_unpinned_fact_at};

pub const WRITE_ACT_SCHEMA: &str = "core/write-act-v1";

#[derive(Debug, Clone)]
pub struct GoalWriteCommand {
    pub handle: Option<Uuid>,
    pub schema_id: String,
    pub title: String,
    /// This version's prompt text; `None` is unknown. Never blank: the column
    /// refuses it. The Goal write verbs fill it, and on a lifecycle write
    /// (transition, achieve) they carry the prior version's text.
    pub text: Option<String>,
    pub state: GoalState,
    pub request_id: String,
    pub close_fact_t: Option<Uuid>,
    pub assignment_t: Option<Uuid>,
    pub dependency_t: Vec<Uuid>,
    pub evidence_t: Vec<Uuid>,
    pub mint_write_act: bool,
    /// Attach an already-minted write-act when `mint_write_act` is false.
    pub write_act_t: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct GoalWriteOutcome {
    pub handle: Uuid,
    pub t: Uuid,
    pub write_act_t: Option<Uuid>,
    pub replay: bool,
}

/// An exact identity reserved for a lifecycle Fact. Reservation mints no
/// database row; persistence happens only after the complete Goal target union
/// has acquired its advisory locks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReservedFactIdentity {
    pub(crate) handle: Uuid,
    pub(crate) t: Uuid,
}

/// A Goal write after replay detection and before its one lifecycle lock.
/// Decomposition prepares every child first, unions these target sets, then
/// persists them in order without extending an already-held advisory set.
#[derive(Debug)]
pub(crate) struct PreparedGoalWrite {
    fresh_handle: bool,
    owner: OwnerId,
    owner_id: Uuid,
    writer: proxima_core::WriterAttribution,
    handle: Uuid,
    t: Uuid,
    /// The predecessor named by a Goal successor request. This is kept
    /// separate from the head snapshot so a successor prepared after its
    /// predecessor stopped being current cannot silently advance the series.
    expected_prior_t: Option<Uuid>,
    /// The head observed for `handle` before the lifecycle lock. The row is
    /// re-read under `FOR UPDATE` after that lock and must still be this t.
    expected_head_t: Option<Uuid>,
    command: GoalWriteCommand,
    targets: Vec<Uuid>,
    write_act_identity: Option<ReservedFactIdentity>,
    close_fact_identity: Option<ReservedFactIdentity>,
    /// The goal-change Fact's reserved identity. Held here only so it joins
    /// the lock union; the Fact itself is written by
    /// [`persist_goal_change_fact`] from the [`GoalChangeFact`] that owns the
    /// same identity.
    change_fact_identity: Option<ReservedFactIdentity>,
}

impl PreparedGoalWrite {
    /// Every Fact identity this write reserved: the write-act, the close
    /// Fact and the goal-change Fact.
    fn lifecycle_identities(&self) -> impl Iterator<Item = ReservedFactIdentity> {
        [
            self.write_act_identity,
            self.close_fact_identity,
            self.change_fact_identity,
        ]
        .into_iter()
        .flatten()
    }

    #[cfg(test)]
    pub(crate) fn reserved_write_act_t(&self) -> Option<Uuid> {
        self.write_act_identity.map(|identity| identity.t)
    }

    #[cfg(test)]
    pub(crate) fn reserved_close_fact_t(&self) -> Option<Uuid> {
        self.close_fact_identity.map(|identity| identity.t)
    }
}

#[derive(Debug)]
pub(crate) enum GoalWritePreparation {
    Replay(GoalWriteOutcome),
    New(Box<PreparedGoalWrite>),
}

/// Existing Goal row for `(owner, request_id)`, if any.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GoalRequestRow {
    pub handle: Uuid,
    pub t: Uuid,
    pub write_act_t: Option<Uuid>,
}

/// Replay lookup: same `(owner, request_id)` is one Goal version.
///
/// # Errors
///
/// `Internal` on query failure.
pub(crate) async fn load_goal_by_request_id(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    request_id: &str,
) -> Result<Option<GoalRequestRow>, StorageError> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        "SELECT handle, t, write_act_t FROM proxima_core.goal
          WHERE owner_id = $1 AND request_id = $2",
    )
    .bind(owner.into_inner())
    .bind(request_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(row.map(|(handle, t, write_act_t)| GoalRequestRow {
        handle,
        t,
        write_act_t,
    }))
}

pub async fn ingest_write_act(
    tx: &mut Transaction<'_, Postgres>,
    permit: &proxima_core::storage_ports::OwnerWritePermit,
    store_number: u16,
) -> Result<FactIngestOutcome, StorageError> {
    let draft = FactWriteCommand {
        schema_id: proxima_core::SchemaId::new(WRITE_ACT_SCHEMA.to_string()),
        schema_version: proxima_core::SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    let writer = crate::owner_scope::require_owner_scope(permit.owner_scope())?.writer();
    ingest_fact_timeseries(
        tx,
        crate::verbs::memory_timeseries::MemoryAdmissionDraft {
            store_number,
            natural_key: None,
            owner: permit.owner(),
            writer,
            draft: &draft,
            origins: &[],
            references: &[],
            sidecar_tables: &[],
            scopes: &[],
        },
        None,
    )
    .await
}

pub(crate) fn reserve_fact_identity(store_number: u16) -> ReservedFactIdentity {
    let t = proxima_core::new_memory_uuid(store_number);
    ReservedFactIdentity {
        handle: proxima_core::new_memory_uuid(store_number),
        t,
    }
}

pub(crate) async fn ingest_write_act_at(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    writer: proxima_core::WriterAttribution,
    identity: ReservedFactIdentity,
) -> Result<FactIngestOutcome, StorageError> {
    let draft = FactWriteCommand {
        schema_id: proxima_core::SchemaId::new(WRITE_ACT_SCHEMA.to_string()),
        schema_version: proxima_core::SchemaVersion::new(1),
        handle: Some(identity.handle),
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    ingest_unpinned_fact_at(
        tx,
        owner,
        writer,
        &draft,
        (identity.handle, identity.t),
        &[],
    )
    .await
    .map(|(outcome, _)| outcome)
}

/// Test-fixtures only: it writes `draft.text` as given and carries nothing.
/// Production writes go through the Goal write verbs (`goal_write`).
#[cfg(any(test, feature = "test-fixtures"))]
pub async fn write_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    writer: proxima_core::WriterAttribution,
    draft: &GoalWriteCommand,
    store_number: u16,
) -> Result<GoalWriteOutcome, StorageError> {
    if let Some(existing) = load_goal_by_request_id(tx, owner, &draft.request_id).await? {
        return Ok(GoalWriteOutcome {
            handle: existing.handle,
            t: existing.t,
            write_act_t: existing.write_act_t,
            replay: true,
        });
    }
    match prepare_goal_write(tx, owner, writer, draft, None, store_number).await? {
        GoalWritePreparation::Replay(outcome) => Ok(outcome),
        GoalWritePreparation::New(prepared) => {
            lock_prepared_goal_write(tx, &prepared).await?;
            persist_prepared_goal_write(tx, &prepared).await
        }
    }
}

/// Prepare one low-level Goal write. Replay is checked before owner creation,
/// UUID minting or head mutation so a replay has no side
/// effects to roll back.
pub(crate) async fn prepare_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    writer: proxima_core::WriterAttribution,
    draft: &GoalWriteCommand,
    expected_prior_t: Option<Uuid>,
    store_number: u16,
) -> Result<GoalWritePreparation, StorageError> {
    if let Some(existing) = load_goal_by_request_id(tx, owner, &draft.request_id).await? {
        return Ok(GoalWritePreparation::Replay(GoalWriteOutcome {
            handle: existing.handle,
            t: existing.t,
            write_act_t: existing.write_act_t,
            replay: true,
        }));
    }

    // Goal admission takes the owner fence before owner-row arbitration. An
    // absent destination owner must never be inserted while a transfer holds
    // its exclusive fence and then wait on that fence with an uncommitted row.
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, owner).await?;
    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;
    // The fence is held for the entire transaction. An owner erase therefore
    // either waits for this complete Goal (including lifecycle Facts) or
    // observes it in exact-scope revalidation.
    let write_act_identity = if draft.mint_write_act {
        Some(reserve_fact_identity(store_number))
    } else {
        None
    };
    let write_act_t = write_act_identity
        .map(|identity| identity.t)
        .or(draft.write_act_t);
    let handle = draft
        .handle
        .unwrap_or_else(|| proxima_core::new_memory_uuid(store_number));
    let mut command = draft.clone();
    command.mint_write_act = false;
    command.write_act_t = write_act_t;
    let expected_head_t: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1")
            .bind(handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let t = match expected_head_t.max(expected_prior_t) {
        Some(floor) => {
            proxima_core::new_memory_uuid_after(store_number, floor).ok_or_else(|| {
                StorageError::ConstraintViolation("memory UUID timestamp range exhausted".into())
            })?
        }
        None => proxima_core::new_memory_uuid(store_number),
    };
    let mut targets = Vec::with_capacity(
        1 + usize::from(command.assignment_t.is_some())
            + command.dependency_t.len()
            + command.evidence_t.len()
            + usize::from(command.close_fact_t.is_some())
            + usize::from(command.write_act_t.is_some())
            + usize::from(expected_prior_t.is_some())
            + usize::from(expected_head_t.is_some()),
    );
    targets.push(t);
    targets.extend(expected_prior_t);
    targets.extend(expected_head_t);
    targets.extend(command.assignment_t);
    targets.extend(command.dependency_t.iter().copied());
    targets.extend(command.evidence_t.iter().copied());
    targets.extend(command.close_fact_t);
    targets.extend(command.write_act_t);
    Ok(GoalWritePreparation::New(Box::new(PreparedGoalWrite {
        fresh_handle: draft.handle.is_none(),
        owner: *owner,
        owner_id,
        writer,
        handle,
        t,
        expected_prior_t,
        expected_head_t,
        command,
        targets,
        write_act_identity,
        close_fact_identity: None,
        change_fact_identity: None,
    })))
}

pub(crate) async fn lock_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    lock_and_validate_prepared_goal_write(tx, prepared).await
}

/// Hold reserved Memory handles and then the complete lifecycle footprint
/// before touching the Goal head. This preserves owner -> Memory handles ->
/// lifecycle -> head across the Goal and its lifecycle Facts. A named
/// predecessor that is no longer current is a caller-fixable conflict; an
/// unnamed snapshot drift is retryable before any write-act, close Fact,
/// sidecar, Goal or sketch row is persisted.
async fn lock_and_validate_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, &prepared.owner).await?;
    let handles = prepared
        .lifecycle_identities()
        .map(|identity| identity.handle)
        .collect::<Vec<_>>();
    crate::verbs::forget::lock_memory_handles_tx(tx, &handles).await?;
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &prepared.targets).await?;
    let current_head: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1 FOR UPDATE")
            .bind(prepared.handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    if let Some(expected_prior_t) = prepared.expected_prior_t
        && current_head != Some(expected_prior_t)
    {
        return Err(StorageError::Conflict(
            "goal successor prior is no longer current".into(),
        ));
    }
    if current_head != prepared.expected_head_t {
        return Err(StorageError::Retryable(
            "goal series head changed while preparing successor".into(),
        ));
    }
    Ok(())
}

pub(crate) fn attach_goal_close_fact_reservation(
    preparation: GoalWritePreparation,
    close_fact: Option<ReservedFactIdentity>,
) -> GoalWritePreparation {
    match preparation {
        GoalWritePreparation::Replay(outcome) => GoalWritePreparation::Replay(outcome),
        GoalWritePreparation::New(mut prepared) => {
            if let Some(close_fact) = close_fact {
                prepared.command.close_fact_t = Some(close_fact.t);
                prepared.targets.push(close_fact.t);
                prepared.close_fact_identity = Some(close_fact);
            }
            GoalWritePreparation::New(prepared)
        }
    }
}

/// What a goal write's change Fact is made of, fixed while the write is
/// prepared: the identity it will take, what the write did, and the stamp it
/// is captured with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GoalChangeFact<'a> {
    identity: ReservedFactIdentity,
    change: GoalChange,
    stamp: PublicationStamp<'a>,
}

impl<'a> GoalChangeFact<'a> {
    pub(crate) const fn new(
        identity: ReservedFactIdentity,
        change: GoalChange,
        stamp: PublicationStamp<'a>,
    ) -> Self {
        Self {
            identity,
            change,
            stamp,
        }
    }
}

/// Join a change Fact's reserved identity to the write's lock union, so it is
/// held with the rest of the footprint before any row is written.
pub(crate) fn attach_goal_change_fact_reservation(
    preparation: GoalWritePreparation,
    change_fact: Option<&GoalChangeFact<'_>>,
) -> GoalWritePreparation {
    match preparation {
        GoalWritePreparation::Replay(outcome) => GoalWritePreparation::Replay(outcome),
        GoalWritePreparation::New(mut prepared) => {
            if let Some(change_fact) = change_fact {
                prepared.targets.push(change_fact.identity.t);
                prepared.change_fact_identity = Some(change_fact.identity);
            }
            GoalWritePreparation::New(prepared)
        }
    }
}

/// Write the Fact that says what this goal write did, and capture its
/// event, in the goal's own transaction.
///
/// The one place a goal change's payload is composed: the version `t` and the
/// series handle are minted by the preparation, so only storage holds them.
/// The Fact goes through the typed admission pieces every Fact does (memory
/// row, typed sidecar, outbox capture), at the identity reserved before the
/// lock union; it is written after the goal row, so a goal that was refused
/// leaves neither.
pub(crate) async fn persist_goal_change_fact(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    prepared: &PreparedGoalWrite,
    fact: &GoalChangeFact<'_>,
) -> Result<(), StorageError> {
    // The goal row is in and the series is locked, so the count is this
    // version's position. A version's `t` is the database clock and is no
    // order.
    let revision: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.goal WHERE handle = $1")
            .bind(prepared.handle)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
    let revision = u32::try_from(revision)
        .map_err(|error| StorageError::Internal(format!("goal revision: {error}")))?;
    let revision_position = std::num::NonZeroU32::new(revision)
        .ok_or_else(|| StorageError::Internal("goal revision: the series has no version".into()))?;
    let payload = SidecarPayload::fact(GoalChangeV1 {
        series: prepared.handle,
        goal: GoalId::new(prepared.t),
        prior: prepared.expected_prior_t.map(GoalId::new),
        revision,
        state: prepared.command.state,
        change: fact.change,
    });
    let data = payload
        .to_protocol_json()
        .map_err(|error| StorageError::Internal(format!("goal change payload: {error}")))?;
    let schema_id = GoalChangeV1::schema_id();
    let schema_version = SchemaVersion::new(GoalChangeV1::SCHEMA_VERSION);
    let plan = fact
        .stamp
        .plan(schema_id.clone(), schema_version, prepared.owner, data);
    let draft = FactWriteCommand {
        schema_id,
        schema_version,
        handle: Some(fact.identity.handle),
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    let tables = sidecars.tables_for_payloads(std::slice::from_ref(&payload))?;
    let (outcome, occurred_at) = ingest_unpinned_fact_at(
        tx,
        &prepared.owner,
        prepared.writer,
        &draft,
        (fact.identity.handle, fact.identity.t),
        &tables,
    )
    .await?;
    sidecars
        .writing(&draft)
        .insert_memory_sidecar(tx, outcome.memory_id, &payload)
        .await?;
    // The event is about the goal, not about the change Fact's own series:
    // its position is the goal version's revision, counted under the goal
    // series lock above.
    let position = proxima_core::publication::SubjectPosition::new(
        prepared.handle,
        std::num::NonZeroU64::from(revision_position),
    );
    crate::verbs::publication_outbox::capture_publication_in_tx(
        tx,
        prepared.owner_id,
        outcome.memory_id.into_inner(),
        occurred_at,
        &plan,
        position,
    )
    .await
}

pub(crate) async fn lock_prepared_goal_writes(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &[&GoalWritePreparation],
) -> Result<(), StorageError> {
    let owners = prepared
        .iter()
        .filter_map(|item| match item {
            GoalWritePreparation::New(item) => Some(item.owner),
            GoalWritePreparation::Replay(_) => None,
        })
        .collect::<Vec<_>>();
    crate::access::owner_columns::lock_owner_fences_shared_tx(tx, &owners).await?;
    let mut handles = Vec::new();
    let mut targets = Vec::new();
    for item in prepared {
        if let GoalWritePreparation::New(item) = item {
            handles.extend(item.lifecycle_identities().map(|identity| identity.handle));
            targets.extend(item.targets.iter().copied());
        }
    }
    crate::verbs::forget::lock_memory_handles_tx(tx, &handles).await?;
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &targets).await
}

pub(crate) async fn persist_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<GoalWriteOutcome, StorageError> {
    // This helper is also called directly by decomposition and crate tests;
    // re-enter the advisory set and re-check the head so persistence remains
    // safe even when a caller already performed the union lock.
    lock_and_validate_prepared_goal_write(tx, prepared).await?;
    revalidate_prepared_goal_targets(tx, prepared).await?;
    persist_goal_lifecycle_rows(tx, prepared).await?;
    advance_goal_head(tx, prepared).await?;
    insert_goal_revision(tx, prepared).await?;
    refresh_goal_sketch(tx, prepared).await?;
    Ok(GoalWriteOutcome {
        handle: prepared.handle,
        t: prepared.t,
        write_act_t: prepared.command.write_act_t,
        replay: false,
    })
}

/// Re-check, under the held lifecycle locks, that every target the prepared
/// write names is still live.
async fn revalidate_prepared_goal_targets(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    let reserved_lifecycle = [
        prepared.write_act_identity.map(|identity| identity.t),
        prepared.close_fact_identity.map(|identity| identity.t),
    ];
    validate_goal_targets_live(tx, &prepared.command, &reserved_lifecycle).await?;
    Ok(())
}

/// Write the lifecycle Facts for the Goal revision.
async fn persist_goal_lifecycle_rows(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    // Lifecycle Facts use the exact reservations included in `targets`.
    // Persist them only after the caller has acquired the complete union, so
    // a blocked Goal preparation cannot leave a write-act or close Fact.
    if let Some(identity) = prepared.write_act_identity {
        ingest_write_act_at(tx, &prepared.owner, prepared.writer, identity).await?;
    }
    if let Some(identity) = prepared.close_fact_identity {
        ingest_write_act_at(tx, &prepared.owner, prepared.writer, identity).await?;
    }
    Ok(())
}

/// Move the series head to the prepared `t`, failing with the conflict the
/// current head implies when the compare-and-set does not apply.
async fn advance_goal_head(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    if prepared.fresh_handle {
        sqlx::query("INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t) VALUES ($1, $2, $3, $4)")
            .bind(prepared.handle)
            .bind(&prepared.command.schema_id)
            .bind(prepared.owner_id)
            .bind(prepared.t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
        return Ok(());
    }
    let head = sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.goal_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.goal_head.owner_id = EXCLUDED.owner_id
           AND ($5::uuid IS NOT NULL AND proxima_core.goal_head.t = $5)
         RETURNING handle",
    )
    .bind(prepared.handle)
    .bind(&prepared.command.schema_id)
    .bind(prepared.owner_id)
    .bind(prepared.t)
    .bind(prepared.expected_head_t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_some() {
        return Ok(());
    }
    let current: Option<(String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT schema_id, owner_id, t FROM proxima_core.goal_head WHERE handle = $1",
    )
    .bind(prepared.handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Err(goal_head_conflict(prepared, current))
}

/// The error a refused head compare-and-set implies, given the head row as it
/// stands now.
fn goal_head_conflict(
    prepared: &PreparedGoalWrite,
    current: Option<(String, Uuid, Uuid)>,
) -> StorageError {
    match current {
        Some((schema_id, owner_id, _))
            if schema_id != prepared.command.schema_id || owner_id != prepared.owner_id =>
        {
            StorageError::ConstraintViolation("goal_head schema/owner mismatch".into())
        }
        Some((_, _, current_t))
            if prepared
                .expected_prior_t
                .is_some_and(|expected_prior_t| current_t != expected_prior_t) =>
        {
            StorageError::Conflict("goal successor prior is no longer current".into())
        }
        Some(_) => StorageError::Retryable("goal series head changed while persisting Goal".into()),
        None if prepared.expected_prior_t.is_some() => {
            StorageError::Conflict("goal successor prior is no longer current".into())
        }
        None => {
            StorageError::Retryable("goal series head disappeared while persisting Goal".into())
        }
    }
}

/// Append the immutable Goal revision row itself.
async fn insert_goal_revision(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.goal
            (handle, t, owner_id, title, text, state, request_id, close_fact_t,
            assignment_t, dependency_t, evidence_t, write_act_t, \"by\", via)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(prepared.handle)
    .bind(prepared.t)
    .bind(prepared.owner_id)
    .bind(&prepared.command.title)
    .bind(prepared.command.text.as_deref())
    .bind(prepared.command.state)
    .bind(&prepared.command.request_id)
    .bind(prepared.command.close_fact_t)
    .bind(prepared.command.assignment_t)
    .bind(&prepared.command.dependency_t)
    .bind(&prepared.command.evidence_t)
    .bind(prepared.command.write_act_t)
    .bind(prepared.writer.by)
    .bind(prepared.writer.via)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Refresh the Goal sketch.
async fn refresh_goal_sketch(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    crate::verbs::sketch::upsert_sketch(
        tx,
        prepared.owner_id,
        prepared.t,
        "goal",
        &prepared.command.title,
    )
    .await?;
    Ok(())
}

async fn validate_goal_targets_live(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalWriteCommand,
    reserved_lifecycle: &[Option<Uuid>; 2],
) -> Result<(), StorageError> {
    // Retained Goal rows may keep forgotten or witnessed pins for historical
    // validity; a new successor is fresh admission and must reauthorize hot,
    // readable assignment, evidence, and lifecycle targets. Hydrate first.
    if let Some(assignment) = draft.assignment_t {
        ensure_live_memory_kind(
            tx,
            assignment,
            "goal assignment perspective",
            &["perspective"],
        )
        .await?;
    }
    for dependency in &draft.dependency_t {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = $1)")
                .bind(dependency)
                .fetch_one(tx.as_mut())
                .await
                .map_err(map_err)?;
        if !exists {
            return Err(StorageError::ConstraintViolation(
                "goal dependency does not exist".into(),
            ));
        }
    }
    for evidence in &draft.evidence_t {
        ensure_live_memory_kind(tx, *evidence, "goal evidence", &["fact", "abstraction"]).await?;
    }
    for target in [draft.close_fact_t, draft.write_act_t]
        .into_iter()
        .flatten()
    {
        if reserved_lifecycle.contains(&Some(target)) {
            continue;
        }
        ensure_live_memory_kind(tx, target, "goal lifecycle Fact", &["fact"]).await?;
    }
    Ok(())
}

async fn ensure_live_memory_kind(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    label: &str,
    allowed: &[&str],
) -> Result<(), StorageError> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind::text FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let Some(kind) = kind else {
        return Err(StorageError::ConstraintViolation(format!(
            "{label} does not exist"
        )));
    };
    if allowed.contains(&kind.as_str()) {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "{label} must be {}",
            allowed.join(" or ")
        )))
    }
}
