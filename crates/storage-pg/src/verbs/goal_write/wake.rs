use super::{
    EntityKind, GoalAtomicContext, GoalId, GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger,
    MemoryId, PayloadKind, Postgres, StorageError, Transaction, WakeConfigRow, WakeConfigShape,
    WakeWrite, map_err,
};

pub(super) async fn write_goal_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    goal_id: GoalId,
    wake_write: WakeWrite<'_>,
) -> Result<(), StorageError> {
    match wake_write {
        WakeWrite::Explicit(Some(config)) => {
            validate_wake_config_storage(tx, context, config).await?;
            insert_goal_wake_config(tx, goal_id, config).await
        }
        WakeWrite::Explicit(None) => Ok(()),
        WakeWrite::CarryFrom(source_goal_id) => {
            sqlx::query(
                "INSERT INTO proxima_core.goal_wake_config
                    (goal_id, trigger_kind, trigger_schema_id, trigger_schema_version,
                     trigger_memory_id, tool_ids, prompt, hard_memory_ids)
                 SELECT $1, trigger_kind, trigger_schema_id, trigger_schema_version,
                        trigger_memory_id, tool_ids, prompt, hard_memory_ids
                   FROM proxima_core.goal_wake_config
                  WHERE goal_id = $2",
            )
            .bind(goal_id.into_inner())
            .bind(source_goal_id.into_inner())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            Ok(())
        }
    }
}

async fn validate_wake_config_storage(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    config: &GoalWakeConfigWrite,
) -> Result<(), StorageError> {
    match config.trigger() {
        proxima_core::GoalWakeTrigger::FactSchema {
            schema_id,
            schema_version,
        } => {
            context
                .registry
                .lookup_payload(schema_id, *schema_version, PayloadKind::Fact)
                .ok_or_else(|| {
                    StorageError::ConstraintViolation(format!(
                        "unregistered wake trigger Fact schema {} v{}",
                        schema_id.as_str(),
                        schema_version.into_inner()
                    ))
                })?;
        }
        proxima_core::GoalWakeTrigger::FactMemory { memory_id } => {
            validate_wake_memory_exists(tx, *memory_id, Some(EntityKind::Fact)).await?;
        }
    }
    for tool_id in config.tool_ids() {
        GoalWakeToolId::parse(tool_id.as_str(), context.registry).map_err(|err| {
            StorageError::ConstraintViolation(format!(
                "invalid wake tool id {}: {}",
                tool_id.as_str(),
                err.message
            ))
        })?;
    }
    let mut seen = std::collections::HashSet::with_capacity(config.hard_memory_ids().len());
    for memory_id in config.hard_memory_ids() {
        if !seen.insert(*memory_id) {
            return Err(StorageError::ConstraintViolation(
                "duplicate wake hard memory".into(),
            ));
        }
        validate_wake_memory_exists(tx, *memory_id, None).await?;
    }
    Ok(())
}

async fn validate_wake_memory_exists(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    expected: Option<EntityKind>,
) -> Result<(), StorageError> {
    let row: Option<(Option<EntityKind>,)> = sqlx::query_as(
        "SELECT kind FROM proxima_core.memories WHERE memory_id = $1 AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some((stored_kind,)) = row else {
        return Err(StorageError::ConstraintViolation(
            "wake memory does not exist".into(),
        ));
    };
    let kind = stored_kind.unwrap_or(EntityKind::Fact);
    if expected.is_some_and(|expected| expected != kind) {
        return Err(StorageError::ConstraintViolation(
            "wake trigger memory must be a Fact".into(),
        ));
    }
    Ok(())
}

async fn insert_goal_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    config: &GoalWakeConfigWrite,
) -> Result<(), StorageError> {
    let shape = wake_shape_from_config(config);
    sqlx::query(
        "INSERT INTO proxima_core.goal_wake_config
            (goal_id, trigger_kind, trigger_schema_id, trigger_schema_version,
             trigger_memory_id, tool_ids, prompt, hard_memory_ids)
         VALUES ($1, $2::proxima_core.goal_wake_trigger_kind, $3, $4, $5, $6, $7, $8)",
    )
    .bind(goal_id.into_inner())
    .bind(&shape.trigger_kind)
    .bind(&shape.trigger_schema_id)
    .bind(shape.trigger_schema_version)
    .bind(shape.trigger_memory_id)
    .bind(&shape.tool_ids)
    .bind(&shape.prompt)
    .bind(&shape.hard_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub(super) async fn goal_wake_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    wake_write: WakeWrite<'_>,
    expected_prior: Option<GoalId>,
) -> Result<bool, StorageError> {
    let expected = match wake_write {
        WakeWrite::Explicit(config) => config.map(wake_shape_from_config),
        WakeWrite::CarryFrom(source_goal_id) => load_wake_shape(tx, source_goal_id).await?,
    };
    let _ = expected_prior;
    Ok(load_wake_shape(tx, goal_id).await? == expected)
}

async fn load_wake_shape(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<Option<WakeConfigShape>, StorageError> {
    let row: Option<WakeConfigRow> = sqlx::query_as(
        "SELECT trigger_kind::text AS trigger_kind,
                trigger_schema_id,
                trigger_schema_version,
                trigger_memory_id,
                tool_ids,
                prompt,
                hard_memory_ids
           FROM proxima_core.goal_wake_config
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(row.map(|row| WakeConfigShape {
        trigger_kind: row.trigger_kind,
        trigger_schema_id: row.trigger_schema_id,
        trigger_schema_version: row.trigger_schema_version,
        trigger_memory_id: row.trigger_memory_id,
        tool_ids: row.tool_ids,
        prompt: row.prompt,
        hard_memory_ids: row.hard_memory_ids,
    }))
}

fn wake_shape_from_config(config: &GoalWakeConfigWrite) -> WakeConfigShape {
    let (trigger_kind, trigger_schema_id, trigger_schema_version, trigger_memory_id) =
        match config.trigger() {
            GoalWakeTrigger::FactSchema {
                schema_id,
                schema_version,
            } => (
                "fact_schema".to_string(),
                Some(schema_id.as_str().to_string()),
                Some(schema_version.into_inner().cast_signed()),
                None,
            ),
            GoalWakeTrigger::FactMemory { memory_id } => (
                "fact_memory".to_string(),
                None,
                None,
                Some(memory_id.into_inner()),
            ),
        };
    WakeConfigShape {
        trigger_kind,
        trigger_schema_id,
        trigger_schema_version,
        trigger_memory_id,
        tool_ids: config
            .tool_ids()
            .iter()
            .map(|tool| tool.as_str().to_string())
            .collect(),
        prompt: config.prompt().to_string(),
        hard_memory_ids: config
            .hard_memory_ids()
            .iter()
            .map(|memory_id| memory_id.into_inner())
            .collect(),
    }
}
