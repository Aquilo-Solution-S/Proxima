use super::{
    EdgeEndpoint, EntityKind, EvidenceTarget, FactPayload, FactReceiptDraft, FactWriteCommand,
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalAtomicContext, GoalId, GoalLifecycleFact,
    GoalPausedV1, GoalWriteOutcome, InsertedGoal, MemoryId, Owner, OwnerWritePermit, Postgres,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError, Transaction,
    assert_goal_topology_references, goal_evidence_matches, goal_topology_edge_count,
    idempotency_conflict, ingest_fact_command_in_tx, map_err,
};

const LIFECYCLE_SOURCE_ID: &str = "core/goal-lifecycle";

trait GoalLifecyclePayload: FactPayload {
    const SIDECAR_TABLE: &'static str;

    fn goal_id(&self) -> uuid::Uuid;

    fn transitioned_at(&self) -> time::OffsetDateTime;
}

impl GoalLifecyclePayload for GoalActivatedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_activated_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalPausedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_paused_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalAchievedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_achieved_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalAbandonedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_abandoned_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

/// Emit the Fact that records one lifecycle transition.
///
/// `evidence` is what the transition rested on, and an achievement Fact
/// declares it as `derived_from` — a derivation declaration, so the index
/// rows that follow are `origin` rows. The Perspective that drove the
/// transition is stamped on the Fact row as its author, which is where the
/// old `core/authored` edge went.
async fn emit_lifecycle_fact(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    lifecycle: GoalLifecycleFact,
    evidence: &[EvidenceTarget],
) -> Result<MemoryId, StorageError> {
    let now = time::OffsetDateTime::now_utc();
    match lifecycle {
        GoalLifecycleFact::Activated => {
            let payload = GoalActivatedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload, evidence).await
        }
        GoalLifecycleFact::Paused => {
            let payload = GoalPausedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload, evidence).await
        }
        GoalLifecycleFact::Achieved => {
            let payload = GoalAchievedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload, evidence).await
        }
        GoalLifecycleFact::Abandoned => {
            let payload = GoalAbandonedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload, evidence).await
        }
    }
}

async fn ingest_lifecycle_fact<T>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    payload: &T,
    evidence: &[EvidenceTarget],
) -> Result<MemoryId, StorageError>
where
    T: GoalLifecyclePayload,
{
    let now = time::OffsetDateTime::now_utc();
    // Only Fact evidence can ground a Fact: a Fact asserts no judgment, so a
    // Fact origin pointing up the F/A/P order is not a thing the layering
    // rule admits.
    let derived_from = evidence
        .iter()
        .filter(|target| target.kind == EntityKind::Fact)
        .map(|target| EdgeEndpoint::memory(EntityKind::Fact, target.memory_id))
        .collect::<Vec<_>>();
    let draft = FactWriteCommand {
        schema_id: SchemaId::new(T::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(T::SCHEMA_VERSION),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: payload.receipt_key(),
        rendered_text: Some(payload.render()),
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(LIFECYCLE_SOURCE_ID),
            source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from,
    };
    if permit.owner() != owner {
        return Err(StorageError::ConstraintViolation(
            "OwnerWritePermit owner does not match lifecycle Fact owner".into(),
        ));
    }
    let outcome = ingest_fact_command_in_tx(
        tx,
        permit,
        &draft,
        context.embedding_model_id,
        context.author_self_perspective_id,
    )
    .await?;
    if !outcome.idempotent_replay {
        insert_lifecycle_sidecar(tx, outcome.memory_id, payload).await?;
    }
    Ok(outcome.memory_id)
}

async fn insert_lifecycle_sidecar<T>(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &T,
) -> Result<(), StorageError>
where
    T: GoalLifecyclePayload,
{
    let table = T::SIDECAR_TABLE;
    let sql = format!(
        "INSERT INTO {table} (memory_id, goal_id, transitioned_at)
         VALUES ($1, $2, $3)"
    );
    // SQL-POLICY: fixed-fragment — {table} is the GoalLifecyclePayload
    // SIDECAR_TABLE associated const (compile-time literal, four in-crate
    // impls); all runtime values are $-bound.
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(memory_id.into_inner())
        .bind(payload.goal_id())
        .bind(payload.transitioned_at())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// What a Goal write states about itself once its row is in place, and the
/// authority to record it: the same inputs for every verb, which is why the
/// tail below is one function rather than five.
pub(super) struct LifecycleWrite<'a> {
    pub(super) permit: &'a OwnerWritePermit,
    pub(super) owner: &'a Owner,
    pub(super) context: GoalAtomicContext<'a>,
    pub(super) inserted: InsertedGoal,
    pub(super) lifecycle: GoalLifecycleFact,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: &'a [GoalId],
    /// What the write rests on, empty for the verbs that name nothing.
    pub(super) evidence: &'a [EvidenceTarget],
    /// The key this write was claimed under, for the replay conflict below.
    pub(super) request_id: &'a str,
}

/// The tail every Goal write shares: record the lifecycle transition as a
/// Fact, derive the reference rows the Goal's topology columns imply, and
/// report what was written.
///
/// A replay skips both — the rows are already there — and reports the counts
/// the declaration implies rather than re-reading them.
pub(super) async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    write: LifecycleWrite<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    if write.inserted.idempotent_replay {
        // The idempotency key is one namespace across every Goal verb, and
        // the body comparison that admitted this replay reads the content
        // columns — `evidence_memory_ids` is not one of them. Evidence is
        // half of what a write claimed, so a key that returns a row resting
        // on other evidence is a conflict, not a replay.
        if !goal_evidence_matches(tx, write.inserted.goal_id, write.evidence).await? {
            return Err(idempotency_conflict(write.request_id));
        }
        return replay_goal_outcome(
            tx,
            write.inserted,
            write.lifecycle,
            goal_topology_edge_count(write.dependencies, write.evidence),
        )
        .await;
    }
    // The lifecycle Fact declares the evidence it was made from, so its own
    // origin rows land inside its ingest transaction — before the reference
    // rows below, which are derived from the Goal's columns instead.
    let lifecycle_memory_id = Some(
        emit_lifecycle_fact(
            tx,
            write.permit,
            write.context,
            write.owner,
            write.inserted.goal_id,
            write.lifecycle,
            write.evidence,
        )
        .await?,
    );
    let edge_count = assert_goal_topology_references(
        tx,
        write.owner,
        write.inserted.goal_id,
        write.assignment,
        write.dependencies,
        write.evidence,
    )
    .await?;
    Ok(GoalWriteOutcome {
        goal_id: write.inserted.goal_id,
        change_event_seq: write.inserted.change_event_seq,
        lifecycle_memory_id,
        edge_count,
        idempotent_replay: false,
    })
}

async fn replay_goal_outcome(
    tx: &mut Transaction<'_, Postgres>,
    inserted: InsertedGoal,
    lifecycle: GoalLifecycleFact,
    edge_count: usize,
) -> Result<GoalWriteOutcome, StorageError> {
    let lifecycle_memory_id = lifecycle_memory_for_goal(tx, inserted.goal_id, lifecycle).await?;
    Ok(GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id,
        // A replay re-asserts the same rows and therefore reports the same
        // count; there are no ids to hand back and nothing to re-read.
        edge_count,
        idempotent_replay: true,
    })
}

pub(super) async fn lifecycle_memory_for_goal(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    lifecycle: GoalLifecycleFact,
) -> Result<Option<MemoryId>, StorageError> {
    let table = match lifecycle {
        GoalLifecycleFact::Activated => "proxima_core.goal_activated_v1",
        GoalLifecycleFact::Paused => "proxima_core.goal_paused_v1",
        GoalLifecycleFact::Achieved => "proxima_core.goal_achieved_v1",
        GoalLifecycleFact::Abandoned => "proxima_core.goal_abandoned_v1",
    };
    let sql = format!("SELECT memory_id FROM {table} WHERE goal_id = $1 LIMIT 1");
    // SQL-POLICY: fixed-fragment — {table} is selected from the closed
    // GoalLifecycleFact enum-to-table mapping above; goal_id is $-bound.
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(goal_id.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}
