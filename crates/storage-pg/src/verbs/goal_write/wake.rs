use super::{
    EntityKind, GoalAtomicContext, GoalId, GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger,
    MemoryId, PayloadKind, Postgres, StorageError, Transaction, WakeConfigShape, WakeWrite,
    map_err,
};
use crate::verbs::wake_timeseries::{WakeConfigDraft, WakeTriggerKind, insert_wake_config};

type WakeShapeRow = (
    String,
    Option<String>,
    Option<uuid::Uuid>,
    Vec<String>,
    String,
    Vec<uuid::Uuid>,
);

pub(super) async fn write_goal_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &proxima_core::Owner,
    wake_write: WakeWrite<'_>,
) -> Result<Option<uuid::Uuid>, StorageError> {
    match wake_write {
        WakeWrite::Explicit(Some(config)) => {
            validate_wake_config_storage(tx, context, config).await?;
            let draft = wake_draft_from_config(config);
            Ok(Some(insert_wake_config(tx, owner, &draft).await?))
        }
        WakeWrite::Explicit(None) => Ok(None),
        WakeWrite::CarryFrom(source_goal_id) => {
            let wake_id: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
                "SELECT wake_id FROM proxima_core.goal WHERE t = $1 AND owner_id = $2",
            )
            .bind(source_goal_id.into_inner())
            .bind(owner.stored_owner_id())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
            Ok(wake_id.flatten())
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
    let row: Option<(String,)> =
        sqlx::query_as("SELECT kind::text FROM proxima_core.memory WHERE t = $1")
            .bind(memory_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    let Some((kind,)) = row else {
        return Err(StorageError::ConstraintViolation(
            "wake memory does not exist".into(),
        ));
    };
    let kind = match kind.as_str() {
        "fact" => EntityKind::Fact,
        "abstraction" => EntityKind::Abstraction,
        "perspective" => EntityKind::Perspective,
        _ => {
            return Err(StorageError::ConstraintViolation(
                "wake memory does not exist".into(),
            ));
        }
    };
    if expected.is_some_and(|expected| expected != kind) {
        return Err(StorageError::ConstraintViolation(
            "wake trigger memory must be a Fact".into(),
        ));
    }
    Ok(())
}

fn wake_draft_from_config(config: &GoalWakeConfigWrite) -> WakeConfigDraft {
    match config.trigger() {
        GoalWakeTrigger::FactSchema { schema_id, .. } => WakeConfigDraft {
            trigger_kind: WakeTriggerKind::FactSchema,
            trigger_schema_id: Some(schema_id.as_str().to_string()),
            trigger_t: None,
            tool_ids: config
                .tool_ids()
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
            prompt: config.prompt().to_string(),
            hard_memory_t: config
                .hard_memory_ids()
                .iter()
                .map(|id| id.into_inner())
                .collect(),
        },
        GoalWakeTrigger::FactMemory { memory_id } => WakeConfigDraft {
            trigger_kind: WakeTriggerKind::FactMemory,
            trigger_schema_id: None,
            trigger_t: Some(memory_id.into_inner()),
            tool_ids: config
                .tool_ids()
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
            prompt: config.prompt().to_string(),
            hard_memory_t: config
                .hard_memory_ids()
                .iter()
                .map(|id| id.into_inner())
                .collect(),
        },
    }
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
    let row: Option<WakeShapeRow> = sqlx::query_as(
        "SELECT w.trigger_kind::text, w.trigger_schema_id, w.trigger_t,
                    w.tool_ids, w.prompt, w.hard_memory_t
               FROM proxima_core.goal g
               JOIN proxima_core.wake_config w ON w.wake_id = g.wake_id
              WHERE g.t = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(row.map(
        |(trigger_kind, trigger_schema_id, trigger_t, tool_ids, prompt, hard_memory_t)| {
            WakeConfigShape {
                trigger_kind,
                trigger_schema_id,
                trigger_schema_version: None,
                trigger_memory_id: trigger_t,
                tool_ids,
                prompt,
                hard_memory_ids: hard_memory_t,
            }
        },
    ))
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
            .map(|id| id.into_inner())
            .collect(),
    }
}
