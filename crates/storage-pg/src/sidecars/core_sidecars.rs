use super::{
    GoalId, MemoryId, PgGoalSidecar, PgMemoryPayload, PgMemoryPayloadBatchFuture, PgMemorySidecar,
    PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry, PgSidecarRegistryFrozen, Postgres,
    SidecarInsertPermit, SidecarPayload, StorageError, Transaction,
};
use proxima_core::verbs::schema::PayloadKind;

fn parse_utterance_speaker(value: &str) -> Result<proxima_core::Speaker, StorageError> {
    match value {
        "user" => Ok(proxima_core::Speaker::User),
        "agent" => Ok(proxima_core::Speaker::Agent),
        other => Err(StorageError::Internal(format!(
            "invalid utterance speaker {other}"
        ))),
    }
}

crate::pg_sidecar! {
    payload: proxima_core::WriteActV1,
    row: WriteActPayloadRow,
    kinds: [Fact],
    table: "proxima_core.write_act_v1",
    key: t,
    fields: {
        episode_id => episode_id: (uuid),
    },
}

crate::pg_sidecar! {
    payload: proxima_core::AgentNoteV1,
    row: AgentNotePayloadRow,
    kinds: [Fact],
    table: "proxima_core.agent_note_v1",
    key: t,
    fields: {
        note_id => note_id: (uuid),
        title => title: (text),
        body => body: (text),
        tags => tags: (text_array),
        idempotency_key => idempotency_key: (opt_text),
    },
}

crate::pg_sidecar! {
    payload: proxima_core::UtteranceV1,
    row: UtterancePayloadRow,
    kinds: [Fact],
    table: "proxima_core.utterance_v1",
    key: t,
    fields: {
        speaker => speaker: (enum {
            to_str: proxima_core::Speaker::as_str,
            pg_type: "text",
            from_str: parse_utterance_speaker
        }),
        conversation_id => conversation_id: (text),
        text => text: (text),
    },
}

crate::pg_sidecar! {
    payload: proxima_core::AgentDerivationV1,
    row: AgentDerivationPayloadRow,
    kinds: [Abstraction, Perspective],
    table: "proxima_core.agent_derivation_v1",
    key: t,
    fields: {
        title => title: (text),
        body => body: (text),
        tags => tags: (text_array),
        idempotency_key => idempotency_key: (opt_text),
        source_memory_ids => source_memory_ids: (uuid_array),
        model_id => model_id: (text),
        client_name => client_name: (text),
        client_version => client_version: (text),
    },
}

crate::pg_sidecar! {
    payload: proxima_core::verbs::persist_mcp_call::McpCallLoggedV1,
    row: McpCallLoggedPayloadRow,
    kinds: [Fact],
    table: "proxima_core.mcp_call_logged_v1",
    key: t,
    owner_pinned: true,
    fields: {
        tool_name => tool_name: (text),
        actor_oid => actor_oid: (text),
        actor_upn => actor_upn: (text),
        ok => ok: (bool),
        error => error: (opt_text),
        latency_ms => latency_ms: (u32_as_i64),
        io_byte_len => io_byte_len: (u64_as_i64),
        io_truncated => io_truncated: (bool),
        io_content_hash => io_content_hash: (bytea32),
    },
}

impl PgGoalSidecar for proxima_core::TaskGoalV1 {
    fn insert_goal_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (t, due_at, priority)
                 VALUES ($1, $2, $3::proxima_core.task_priority)",
            )
            .bind(goal_id.into_inner())
            .bind(self.due_at)
            .bind(self.priority.map(proxima_core::TaskPriority::as_str))
            .execute(tx.as_mut())
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }

    fn copy_goal_sidecar<'t>(
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
        source_goal_id: GoalId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let result = sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (t, due_at, priority)
                 SELECT $1, due_at, priority
                   FROM proxima_core.task_goal_v1
                  WHERE t = $2",
            )
            .bind(goal_id.into_inner())
            .bind(source_goal_id.into_inner())
            .execute(tx.as_mut())
            .await
            .map_err(crate::error::map_err)?;
            if result.rows_affected() == 0 {
                return Err(StorageError::ConstraintViolation(format!(
                    "missing source Goal sidecar for {}",
                    source_goal_id.into_inner(),
                )));
            }
            Ok(())
        })
    }
}

/// The interpretation Perspective.
///
/// Hand-written rather than `pg_sidecar!`-generated: the payload carries a
/// `u8` confidence and a positionally aligned enum array, and neither is a
/// column shape the macro spells. The subjects are payload fields, so the
/// reference rows that connect an interpretation to what it interprets are
/// re-derivable from this row alone.
#[derive(Debug, sqlx::FromRow)]
struct InterpretationPayloadRow {
    t: uuid::Uuid,
    claim: String,
    confidence: i16,
    subject_memory_ids: Vec<uuid::Uuid>,
    subject_kinds: Vec<proxima_core::InterpretationSubjectKind>,
    model_id: String,
    client_name: String,
    client_version: String,
}

impl PgMemorySidecar for proxima_core::InterpretationV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.interpretation_v1
                    (t, claim, confidence, subject_memory_ids, subject_kinds,
                     model_id, client_name, client_version)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.claim)
            .bind(i16::from(self.confidence))
            .bind(&self.subject_memory_ids)
            .bind(&self.subject_kinds)
            .bind(&self.model_id)
            .bind(&self.client_name)
            .bind(&self.client_version)
            .execute(tx.as_mut())
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for proxima_core::InterpretationV1 {
    // The column this table stores its memory `t` under, spelled by
    // every statement below. Freeze holds it equal to the contract
    // `Surface`'s `KeyShape::MemoryT { column }` for proxima_core.interpretation_v1.
    const OWNER_PINNED: bool = false;
    const MEMORY_KEY_COLUMN: &'static str = "t";

    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        _kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let rows: Vec<InterpretationPayloadRow> = ctx
                .fetch_all_by_memory_ids(
                    "SELECT t, claim, confidence, subject_memory_ids, subject_kinds,
                            model_id, client_name, client_version
                       FROM proxima_core.interpretation_v1
                      WHERE t = ANY($1::uuid[])",
                    memory_ids,
                )
                .await?;
            rows.into_iter()
                .map(|row| {
                    let confidence = u8::try_from(row.confidence).map_err(|_| {
                        StorageError::Internal(format!(
                            "interpretation_v1 confidence {} out of range for memory {}",
                            row.confidence, row.t
                        ))
                    })?;
                    Ok((
                        MemoryId::new(row.t),
                        SidecarPayload::perspective(proxima_core::InterpretationV1 {
                            claim: row.claim,
                            confidence,
                            subject_memory_ids: row.subject_memory_ids,
                            subject_kinds: row.subject_kinds,
                            model_id: row.model_id,
                            client_name: row.client_name,
                            client_version: row.client_version,
                        }),
                    ))
                })
                .collect()
        })
    }
}

/// One goal write, as `proxima_core.goal_change_v1` stores it.
///
/// Hand-written rather than `pg_sidecar!`-generated: `change` is a closed
/// enum whose `parent` and `pending` live in their own columns, and the macro
/// maps one field to one column. The three columns are cut from the enum by
/// `GoalChange::{kind, parent, pending}` and rebuilt by
/// `GoalChange::from_columns`, the one place a stored row becomes a change.
#[derive(Debug, sqlx::FromRow)]
struct GoalChangePayloadRow {
    t: uuid::Uuid,
    series: uuid::Uuid,
    goal_t: uuid::Uuid,
    prior_t: Option<uuid::Uuid>,
    revision: i32,
    state: proxima_core::verbs::goal_write::GoalState,
    change_kind: proxima_core::GoalChangeKind,
    parent_t: Option<uuid::Uuid>,
    pending: Option<proxima_core::PendingWork>,
}

impl PgMemorySidecar for proxima_core::GoalChangeV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.goal_change_v1
                    (t, series, goal_t, prior_t, revision, state, change_kind, parent_t, pending)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(memory_id.into_inner())
            .bind(self.series)
            .bind(self.goal.into_inner())
            .bind(self.prior.map(GoalId::into_inner))
            .bind(
                i32::try_from(self.revision)
                    .map_err(|error| StorageError::Internal(format!("goal revision: {error}")))?,
            )
            .bind(self.state)
            .bind(self.change.kind())
            .bind(self.change.parent().map(GoalId::into_inner))
            .bind(self.change.pending())
            .execute(tx.as_mut())
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for proxima_core::GoalChangeV1 {
    // Freeze holds the key column equal to the contract `Surface`'s
    // `KeyShape::MemoryT { column }` for proxima_core.goal_change_v1.
    const OWNER_PINNED: bool = false;
    const MEMORY_KEY_COLUMN: &'static str = "t";

    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        _kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let rows: Vec<GoalChangePayloadRow> = ctx
                .fetch_all_by_memory_ids(
                    "SELECT t, series, goal_t, prior_t, revision, state, change_kind, parent_t,
                            pending
                       FROM proxima_core.goal_change_v1
                      WHERE t = ANY($1::uuid[])",
                    memory_ids,
                )
                .await?;
            rows.into_iter()
                .map(|row| {
                    let change = proxima_core::GoalChange::from_columns(
                        row.change_kind,
                        row.parent_t.map(GoalId::new),
                        row.pending,
                    )
                    .map_err(|error| {
                        StorageError::Internal(format!(
                            "goal_change_v1 row for memory {} is not a goal change: {error}",
                            row.t
                        ))
                    })?;
                    let revision = u32::try_from(row.revision).map_err(|error| {
                        StorageError::Internal(format!(
                            "goal_change_v1 row for memory {} has revision {}: {error}",
                            row.t, row.revision
                        ))
                    })?;
                    Ok((
                        MemoryId::new(row.t),
                        SidecarPayload::fact(proxima_core::GoalChangeV1 {
                            series: row.series,
                            goal: GoalId::new(row.goal_t),
                            prior: row.prior_t.map(GoalId::new),
                            revision,
                            state: row.state,
                            change,
                        }),
                    ))
                })
                .collect()
        })
    }
}

/// Frozen core sidecar registry used by plain substrate `PgStorage`.
///
/// # Panics
///
/// Panics only if the core hardcoded sidecar registrations drift from
/// the core schema registry.
#[must_use]
pub fn core_pg_sidecars() -> PgSidecarRegistryFrozen {
    let mut registry = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut registry);
    registry
        .freeze_against(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
        .expect("core PG sidecars match core schema registry")
}

pub fn register_core_pg_sidecars(registry: &mut PgSidecarRegistry) {
    registry.add_fact::<proxima_core::WriteActV1>();
    registry.add_fact::<proxima_core::GoalChangeV1>();
    registry.add_fact::<proxima_core::AgentNoteV1>();
    registry.add_fact::<proxima_core::UtteranceV1>();
    registry.add_fact::<proxima_core::verbs::persist_mcp_call::McpCallLoggedV1>();
    registry.add_abstraction::<proxima_core::AgentDerivationV1>();
    registry.add_perspective::<proxima_core::AgentDerivationV1>();
    registry.add_perspective::<proxima_core::InterpretationV1>();
    registry.add_goal::<proxima_core::TaskGoalV1>();
}
