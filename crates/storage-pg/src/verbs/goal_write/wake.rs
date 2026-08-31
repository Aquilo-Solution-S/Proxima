use super::{
    EntityKind, GoalAtomicContext, GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, MemoryId,
    PayloadKind, Postgres, StorageError, Transaction, WakeWrite, map_err,
};
use crate::verbs::goal_timeseries::{GoalWakePlan, load_goal_wake_plan};
use crate::verbs::wake_timeseries::{WakeConfigDraft, WakeTriggerKind};

pub(super) async fn prepare_goal_wake_plan(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &proxima_core::Owner,
    wake_write: WakeWrite<'_>,
) -> Result<GoalWakePlan, StorageError> {
    match wake_write {
        WakeWrite::Explicit(Some(config)) => {
            validate_wake_config_storage(tx, context, config).await?;
            let draft = wake_draft_from_config(config);
            Ok(GoalWakePlan::New(draft))
        }
        WakeWrite::Explicit(None) => Ok(GoalWakePlan::None),
        WakeWrite::CarryFrom(source_goal_id) => {
            let wake_id: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
                "SELECT wake_id FROM proxima_core.goal WHERE t = $1 AND owner_id = $2",
            )
            .bind(source_goal_id.into_inner())
            .bind(owner.stored_owner_id())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
            match wake_id.flatten() {
                Some(wake_id) => load_goal_wake_plan(tx, wake_id).await,
                None => Ok(GoalWakePlan::None),
            }
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
