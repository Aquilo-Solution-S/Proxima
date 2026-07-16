//! Goal-owned wake candidate reads.
//!
//! PR6 exposes candidate/admission data only. No scheduler, executor, emitted
//! Fact write, or tool invocation row is created here.

use proxima_core::{
    EntityKind, GoalId, GoalWakeCandidate, GoalWakeCandidateRequest, GoalWakeHardMemory, MemoryId,
    StorageError, ToolScope,
};
use sqlx::PgPool;

use crate::access::owner_columns::{owner_arrays, owner_binds};
use crate::error::map_err;

#[derive(Debug, sqlx::FromRow)]
struct CandidateRow {
    goal_id: uuid::Uuid,
    tool_ids: Vec<String>,
    prompt: String,
    hard_memory_ids: Vec<uuid::Uuid>,
    hard_memory_kinds: Vec<String>,
}

pub(crate) async fn list_goal_wake_candidates(
    pool: &PgPool,
    req: &GoalWakeCandidateRequest<'_>,
) -> Result<Vec<GoalWakeCandidate>, StorageError> {
    let Some(args) = CandidateQueryArgs::from_request(req)? else {
        return Ok(Vec::new());
    };
    let allowed_scope = args.allowed_scope.clone();
    let rows = query_candidate_rows(pool, req, &args).await?;

    rows.into_iter()
        .filter(|row| row.tool_ids.iter().all(|tool| allowed_scope.allows(tool)))
        .map(|row| {
            Ok(GoalWakeCandidate {
                goal_id: GoalId::new(row.goal_id),
                tool_ids: row.tool_ids,
                prompt: row.prompt,
                hard_memories: hard_memories(row.hard_memory_ids, &row.hard_memory_kinds)?,
                actor_write_owners: req.actor_write_owners.to_vec(),
            })
        })
        .collect()
}

fn hard_memories(
    ids: Vec<uuid::Uuid>,
    kinds: &[String],
) -> Result<Vec<GoalWakeHardMemory>, StorageError> {
    if ids.len() != kinds.len() {
        return Err(StorageError::Internal(
            "hard memory id/kind arrays diverged".into(),
        ));
    }
    ids.into_iter()
        .zip(kinds)
        .map(|(memory_id, kind)| {
            let kind = match kind.as_str() {
                "Fact" => EntityKind::Fact,
                "Abstraction" => EntityKind::Abstraction,
                "Perspective" => EntityKind::Perspective,
                other => {
                    return Err(StorageError::Internal(format!(
                        "unexpected hard memory kind {other}"
                    )));
                }
            };
            Ok(GoalWakeHardMemory {
                memory_id: MemoryId::new(memory_id),
                kind,
            })
        })
        .collect()
}

#[derive(Debug)]
struct CandidateQueryArgs {
    limit: i64,
    read_owner_kinds: Vec<proxima_core::OwnerRefKind>,
    read_owner_ids: Vec<Option<uuid::Uuid>>,
    trigger_owner_kind: proxima_core::OwnerRefKind,
    trigger_owner_id: Option<uuid::Uuid>,
    trigger_schema_version: i32,
    allowed_scope: ToolScope,
    allowed_tool_ids: Option<Vec<String>>,
}

impl CandidateQueryArgs {
    fn from_request(req: &GoalWakeCandidateRequest<'_>) -> Result<Option<Self>, StorageError> {
        if req.limit == 0 || req.actor_read_owners.is_empty() {
            return Ok(None);
        }
        let limit = i64::try_from(req.limit).map_err(|_| {
            StorageError::ConstraintViolation("wake candidate limit too large".into())
        })?;
        let allowed_scope = req.actor_tool_scope.intersect(req.deployment_tool_scope);
        let allowed_tool_ids = match &allowed_scope {
            ToolScope::All => None,
            ToolScope::Palette(ids) if ids.is_empty() => return Ok(None),
            ToolScope::Palette(ids) => Some(ids.clone()),
        };
        let (read_owner_kinds, read_owner_ids) = owner_arrays(req.actor_read_owners);
        let (trigger_owner_kind, trigger_owner_id) = owner_binds(&req.trigger_owner);
        Ok(Some(Self {
            limit,
            read_owner_kinds,
            read_owner_ids,
            trigger_owner_kind,
            trigger_owner_id,
            trigger_schema_version: req.trigger_schema_version.into_inner().cast_signed(),
            allowed_scope,
            allowed_tool_ids,
        }))
    }
}

async fn query_candidate_rows(
    pool: &PgPool,
    req: &GoalWakeCandidateRequest<'_>,
    args: &CandidateQueryArgs,
) -> Result<Vec<CandidateRow>, StorageError> {
    sqlx::query_as(
        "WITH read_owners(owner_kind, owner_id) AS (
             SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
         ),
         trigger_fact AS (
             SELECT m.memory_id
               FROM proxima_core.memories m
               JOIN read_owners ro
                 ON ro.owner_kind = m.owner_kind
                AND ro.owner_id IS NOT DISTINCT FROM m.owner_id
              WHERE m.memory_id = $3
                AND m.owner_kind = $4
                AND m.owner_id IS NOT DISTINCT FROM $5
                AND m.kind IS NULL
                AND m.tombstoned_at IS NULL
                AND m.schema_id = $6
                AND m.schema_version = $7
         )
         SELECT g.goal_id,
                w.tool_ids,
                w.prompt,
                COALESCE(hard.ids, '{}') AS hard_memory_ids,
                COALESCE(hard.kinds, '{}') AS hard_memory_kinds
           FROM proxima_core.goals g
           JOIN proxima_core.goal_wake_config w
             ON w.goal_id = g.goal_id
           LEFT JOIN LATERAL (
                SELECT array_agg(hm.memory_id ORDER BY h.ord) AS ids,
                       array_agg(COALESCE(hm.kind, 'Fact'::proxima_core.entity_kind)::text
                                 ORDER BY h.ord) AS kinds
                  FROM unnest(w.hard_memory_ids) WITH ORDINALITY AS h(memory_id, ord)
                  JOIN proxima_core.memories hm
                    ON hm.memory_id = h.memory_id
           ) hard ON TRUE
           JOIN read_owners goal_ro
             ON goal_ro.owner_kind = g.owner_kind
            AND goal_ro.owner_id IS NOT DISTINCT FROM g.owner_id
          WHERE g.state = 'Active'
            AND EXISTS (SELECT 1 FROM trigger_fact)
            AND NOT EXISTS (
                 SELECT 1
                   FROM proxima_core.goals newer
                  WHERE newer.supersedes = g.goal_id
             )
            AND (
                 (w.trigger_kind = 'fact_memory'
                  AND w.trigger_memory_id = $3)
                 OR
                 (w.trigger_kind = 'fact_schema'
                  AND w.trigger_schema_id = $6
                  AND w.trigger_schema_version = $7)
            )
            AND (
                 $8::text[] IS NULL
                 OR NOT EXISTS (
                     SELECT 1
                       FROM unnest(w.tool_ids) AS configured(tool_id)
                      WHERE configured.tool_id <> ALL($8::text[])
                 )
            )
            AND NOT EXISTS (
                 SELECT 1
                   FROM unnest(w.hard_memory_ids) AS hard(memory_id)
                  WHERE NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.memories hm
                          JOIN read_owners hard_ro
                            ON hard_ro.owner_kind = hm.owner_kind
                           AND hard_ro.owner_id IS NOT DISTINCT FROM hm.owner_id
                         WHERE hm.memory_id = hard.memory_id
                           AND hm.tombstoned_at IS NULL
                  )
            )
          ORDER BY g.created_at, g.goal_id
          LIMIT $9",
    )
    .bind(&args.read_owner_kinds)
    .bind(&args.read_owner_ids)
    .bind(req.trigger_fact_id.into_inner())
    .bind(args.trigger_owner_kind)
    .bind(args.trigger_owner_id)
    .bind(req.trigger_schema_id.as_str())
    .bind(args.trigger_schema_version)
    .bind(args.allowed_tool_ids.clone())
    .bind(args.limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

#[derive(Debug, sqlx::FromRow)]
struct WakeConfigDbRow {
    goal_id: uuid::Uuid,
    trigger_memory_id: Option<uuid::Uuid>,
    trigger_schema_id: Option<String>,
    trigger_schema_version: Option<i32>,
    tool_ids: Vec<String>,
    prompt: String,
    hard_memory_ids: Vec<uuid::Uuid>,
    hard_memory_kinds: Vec<String>,
}

/// Wake-config read-back for goal introspection. Owner-scoped through the
/// caller's read set; goals without a wake config simply produce no row.
pub(crate) async fn load_goal_wake_configs(
    pool: &PgPool,
    read_owners: &[proxima_core::OwnerRef],
    goal_ids: &[GoalId],
) -> Result<Vec<proxima_core::read_models::GoalWakeConfigRow>, StorageError> {
    if goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kinds, owner_ids) = owner_arrays(read_owners);
    let ids: Vec<uuid::Uuid> = goal_ids.iter().map(|id| id.into_inner()).collect();
    let rows: Vec<WakeConfigDbRow> = sqlx::query_as(
        "WITH read_owners AS (
             SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
                 AS s(owner_kind, owner_id)
         )
         SELECT w.goal_id,
                w.trigger_memory_id,
                w.trigger_schema_id,
                w.trigger_schema_version,
                w.tool_ids,
                w.prompt,
                COALESCE(hard.ids, '{}') AS hard_memory_ids,
                COALESCE(hard.kinds, '{}') AS hard_memory_kinds
           FROM proxima_core.goal_wake_config w
           JOIN proxima_core.goals g
             ON g.goal_id = w.goal_id
           JOIN read_owners goal_ro
             ON goal_ro.owner_kind = g.owner_kind
            AND goal_ro.owner_id IS NOT DISTINCT FROM g.owner_id
           LEFT JOIN LATERAL (
                SELECT array_agg(hm.memory_id ORDER BY h.ord) AS ids,
                       array_agg(COALESCE(hm.kind, 'Fact'::proxima_core.entity_kind)::text
                                 ORDER BY h.ord) AS kinds
                  FROM unnest(w.hard_memory_ids) WITH ORDINALITY AS h(memory_id, ord)
                  JOIN proxima_core.memories hm
                    ON hm.memory_id = h.memory_id
           ) hard ON TRUE
          WHERE w.goal_id = ANY($3::uuid[])
          ORDER BY w.goal_id",
    )
    .bind(owner_kinds)
    .bind(owner_ids)
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    rows.into_iter()
        .map(|row| {
            let hard = hard_memories(row.hard_memory_ids, &row.hard_memory_kinds)?;
            Ok(proxima_core::read_models::GoalWakeConfigRow {
                goal_id: GoalId::new(row.goal_id),
                trigger_memory_id: row.trigger_memory_id.map(MemoryId::new),
                trigger_schema_id: row.trigger_schema_id.map(proxima_core::SchemaId::new),
                trigger_schema_version: row
                    .trigger_schema_version
                    .map(|version| proxima_core::SchemaVersion::new(version.cast_unsigned())),
                tool_ids: row.tool_ids,
                prompt: row.prompt,
                hard_memories: hard,
            })
        })
        .collect()
}
