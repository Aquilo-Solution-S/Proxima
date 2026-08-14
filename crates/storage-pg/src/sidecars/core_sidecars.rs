use super::{
    GoalId, MemoryId, PgCitationMappingSidecar, PgCitedObjectSidecar, PgConnection, PgGoalSidecar,
    PgMemoryPayload, PgMemoryPayloadBatchFuture, PgMemorySidecar, PgSidecarFuture,
    PgSidecarReadCtx, PgSidecarRegistry, PgSidecarRegistryFrozen, Postgres, SidecarPayload,
    StorageError, Transaction,
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
    payload: proxima_core::AgentNoteV1,
    row: AgentNotePayloadRow,
    kinds: [Fact],
    table: "proxima_core.agent_note_v1",
    key: memory_id,
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
    key: memory_id,
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
    key: memory_id,
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
    key: memory_id,
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

crate::goal_lifecycle_fact!(
    proxima_core::GoalActivatedV1,
    GoalActivatedPayloadRow,
    "proxima_core.goal_activated_v1"
);
crate::goal_lifecycle_fact!(
    proxima_core::GoalPausedV1,
    GoalPausedPayloadRow,
    "proxima_core.goal_paused_v1"
);
crate::goal_lifecycle_fact!(
    proxima_core::GoalAchievedV1,
    GoalAchievedPayloadRow,
    "proxima_core.goal_achieved_v1"
);
crate::goal_lifecycle_fact!(
    proxima_core::GoalAbandonedV1,
    GoalAbandonedPayloadRow,
    "proxima_core.goal_abandoned_v1"
);

impl PgGoalSidecar for proxima_core::TaskGoalV1 {
    fn insert_goal_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (goal_id, due_at, priority)
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
                "INSERT INTO proxima_core.task_goal_v1 (goal_id, due_at, priority)
                 SELECT $1, due_at, priority
                   FROM proxima_core.task_goal_v1
                  WHERE goal_id = $2",
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

/// The interpretation Perspective — what `core_link` used to put on an edge.
///
/// Hand-written rather than `pg_sidecar!`-generated: the payload carries a
/// `u8` confidence and a positionally aligned enum array, and neither is a
/// column shape the macro spells. The subjects are payload fields, so the
/// reference rows that connect an interpretation to what it interprets are
/// re-derivable from this row alone.
#[derive(Debug, sqlx::FromRow)]
struct InterpretationPayloadRow {
    memory_id: uuid::Uuid,
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
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.interpretation_v1
                    (memory_id, claim, confidence, subject_memory_ids, subject_kinds,
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
    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        _kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let rows: Vec<InterpretationPayloadRow> = ctx
                .fetch_all_by_memory_ids(
                    "SELECT memory_id, claim, confidence, subject_memory_ids, subject_kinds,
                            model_id, client_name, client_version
                       FROM proxima_core.interpretation_v1
                      WHERE memory_id = ANY($1::uuid[])",
                    memory_ids,
                )
                .await?;
            rows.into_iter()
                .map(|row| {
                    let confidence = u8::try_from(row.confidence).map_err(|_| {
                        StorageError::Internal(format!(
                            "interpretation_v1 confidence {} out of range for memory {}",
                            row.confidence, row.memory_id
                        ))
                    })?;
                    Ok((
                        MemoryId::new(row.memory_id),
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

impl PgCitedObjectSidecar for proxima_core::UploadedBlobPayload {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let byte_len = i64::try_from(self.byte_len).map_err(|err| {
                StorageError::ConstraintViolation(format!("byte_len out of range: {err}"))
            })?;
            sqlx::query(
                "INSERT INTO proxima_core.cited_uploaded_blob_v1
                    (cited_object_id, bucket, object_key, sha256, byte_len,
                     mime, filename, etag, uploaded_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(&self.bucket)
            .bind(&self.object_key)
            .bind(&self.sha256[..])
            .bind(byte_len)
            .bind(&self.mime)
            .bind(&self.filename)
            .bind(self.etag.as_deref())
            .bind(self.uploaded_at)
            .execute(tx)
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }
}

impl PgCitationMappingSidecar for proxima_core::UploadedBlobPageSpanV1 {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        citation_mapping_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            // The CHECK constraints on the table are the authority for any
            // client; these conversions only keep an out-of-range u32 from
            // arriving as a negative integer, which would satisfy no check
            // and report as a confusing constraint violation instead of the
            // range error it is.
            let to_i32 = |value: u32, field: &str| {
                i32::try_from(value).map_err(|err| {
                    StorageError::ConstraintViolation(format!("{field} out of range: {err}"))
                })
            };
            let page_from = to_i32(self.page_from, "page_from")?;
            let page_to = to_i32(self.page_to, "page_to")?;
            let char_start = self
                .char_range_start
                .map(|value| to_i32(value, "char_range_start"))
                .transpose()?;
            let char_end = self
                .char_range_end
                .map(|value| to_i32(value, "char_range_end"))
                .transpose()?;
            sqlx::query(
                "INSERT INTO proxima_core.citation_uploaded_blob_page_span_v1
                    (citation_mapping_id, page_from, page_to,
                     char_range_start, char_range_end)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (citation_mapping_id) DO NOTHING",
            )
            .bind(citation_mapping_id)
            .bind(page_from)
            .bind(page_to)
            .bind(char_start)
            .bind(char_end)
            .execute(tx)
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }
}

impl PgCitedObjectSidecar for proxima_core::verbs::persist_mcp_call::McpCallIoV1 {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let byte_len = i64::try_from(self.byte_len).map_err(|err| {
                StorageError::ConstraintViolation(format!("byte_len out of range: {err}"))
            })?;
            sqlx::query(
                "INSERT INTO proxima_core.cited_mcp_call_io_v1
                    (cited_object_id, byte_len, truncated, body)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(byte_len)
            .bind(self.truncated)
            .bind(&self.body)
            .execute(tx)
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
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
        .freeze_against(
            proxima_core::FlavorRegistry::new()
                .freeze_or_panic_for_tests()
                .schemas(),
        )
        .expect("core PG sidecars match core schema registry")
}

pub fn register_core_pg_sidecars(registry: &mut PgSidecarRegistry) {
    registry.add_fact::<proxima_core::AgentNoteV1>();
    registry.add_fact::<proxima_core::UtteranceV1>();
    registry.add_fact::<proxima_core::verbs::persist_mcp_call::McpCallLoggedV1>();
    registry.add_fact::<proxima_core::GoalActivatedV1>();
    registry.add_fact::<proxima_core::GoalPausedV1>();
    registry.add_fact::<proxima_core::GoalAchievedV1>();
    registry.add_fact::<proxima_core::GoalAbandonedV1>();
    registry.add_abstraction::<proxima_core::AgentDerivationV1>();
    registry.add_perspective::<proxima_core::AgentDerivationV1>();
    registry.add_perspective::<proxima_core::InterpretationV1>();
    registry.add_goal::<proxima_core::TaskGoalV1>();
    registry.add_cited_object::<proxima_core::UploadedBlobPayload>();
    registry.add_cited_object::<proxima_core::verbs::persist_mcp_call::McpCallIoV1>();
    // `UploadedBlobWholeV1` is a pure link with no sidecar table, so it needs
    // no entry here — `citation_mappings` is the whole mapping.
    registry.add_citation_mapping::<proxima_core::UploadedBlobPageSpanV1>();
}
