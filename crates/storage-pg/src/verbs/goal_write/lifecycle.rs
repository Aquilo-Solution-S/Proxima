use super::{
    FactPayload, FactReceiptDraft, FactWriteCommand, GoalAbandonedV1, GoalAchievedV1,
    GoalActivatedV1, GoalAtomicContext, GoalId, GoalLifecycleFact, GoalPausedV1, GoalWriteOutcome,
    InsertedGoal, MemoryId, Owner, OwnerWritePermit, Postgres, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, StorageError, Transaction, append_goal_to_self_edge,
    append_lifecycle_authored_edge, edge_ids_for_goal_relations, edge_ids_for_lifecycle_memory,
    ingest_fact_command_in_tx, map_err,
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

pub(super) async fn emit_lifecycle_fact(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    lifecycle: GoalLifecycleFact,
) -> Result<MemoryId, StorageError> {
    let now = time::OffsetDateTime::now_utc();
    match lifecycle {
        GoalLifecycleFact::Activated => {
            let payload = GoalActivatedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload).await
        }
        GoalLifecycleFact::Paused => {
            let payload = GoalPausedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload).await
        }
        GoalLifecycleFact::Achieved => {
            let payload = GoalAchievedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload).await
        }
        GoalLifecycleFact::Abandoned => {
            let payload = GoalAbandonedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, permit, context, owner, &payload).await
        }
    }
}

async fn ingest_lifecycle_fact<T>(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    payload: &T,
) -> Result<MemoryId, StorageError>
where
    T: GoalLifecyclePayload,
{
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand {
        schema_id: SchemaId::new(T::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(T::SCHEMA_VERSION),
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
    };
    if permit.owner() != owner {
        return Err(StorageError::ConstraintViolation(
            "OwnerWritePermit owner does not match lifecycle Fact owner".into(),
        ));
    }
    let outcome = ingest_fact_command_in_tx(tx, permit, &draft, context.embedding_model_id).await?;
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

pub(super) async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    owner: &Owner,
    context: GoalAtomicContext<'_>,
    inserted: InsertedGoal,
    lifecycle: GoalLifecycleFact,
    assignment: MemoryId,
) -> Result<GoalWriteOutcome, StorageError> {
    if inserted.idempotent_replay {
        return replay_goal_outcome(
            tx,
            inserted,
            lifecycle,
            &[proxima_core::relation::CORE_INSPIRES_RELATION],
        )
        .await;
    }
    let lifecycle_memory_id =
        Some(emit_lifecycle_fact(tx, permit, context, owner, inserted.goal_id, lifecycle).await?);
    let mut edge_ids = Vec::new();
    edge_ids
        .push(append_goal_to_self_edge(tx, context, owner, inserted.goal_id, assignment).await?);
    if let Some(lifecycle_id) = lifecycle_memory_id
        && let Some(edge_id) =
            append_lifecycle_authored_edge(tx, context, owner, lifecycle_id).await?
    {
        edge_ids.push(edge_id);
    }
    Ok(GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id,
        edge_ids,
        idempotent_replay: false,
    })
}

pub(super) async fn replay_goal_outcome(
    tx: &mut Transaction<'_, Postgres>,
    inserted: InsertedGoal,
    lifecycle: GoalLifecycleFact,
    source_goal_relations: &[&str],
) -> Result<GoalWriteOutcome, StorageError> {
    let lifecycle_memory_id = lifecycle_memory_for_goal(tx, inserted.goal_id, lifecycle).await?;
    let mut edge_ids =
        edge_ids_for_goal_relations(tx, inserted.goal_id, source_goal_relations).await?;
    if let Some(memory_id) = lifecycle_memory_id {
        edge_ids.extend(edge_ids_for_lifecycle_memory(tx, memory_id).await?);
    }
    Ok(GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id,
        edge_ids,
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
