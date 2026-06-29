//! Goal-owned wake candidate reads.
//!
//! PR6 exposes candidate/admission data only. No scheduler, executor, emitted
//! Fact write, or tool invocation row is created here.

use proxima_core::{
    GoalId, GoalWakeCandidate, GoalWakeCandidateRequest, MemoryId, StorageError, ToolScope,
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

    Ok(rows
        .into_iter()
        .filter(|row| row.tool_ids.iter().all(|tool| allowed_scope.allows(tool)))
        .map(|row| GoalWakeCandidate {
            goal_id: GoalId::new(row.goal_id),
            tool_ids: row.tool_ids,
            prompt: row.prompt,
            hard_memory_ids: row.hard_memory_ids.into_iter().map(MemoryId::new).collect(),
            actor_write_owners: req.actor_write_owners.to_vec(),
        })
        .collect())
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
                w.hard_memory_ids
           FROM proxima_core.goals g
           JOIN proxima_core.goal_wake_config w
             ON w.goal_id = g.goal_id
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
