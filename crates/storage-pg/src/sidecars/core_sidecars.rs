use super::{
    EdgeId, GoalId, PgCitationMappingSidecar, PgCitedObjectSidecar, PgConnection, PgEdgePayload,
    PgEdgePayloadBatchFuture, PgEdgeSidecar, PgGoalSidecar, PgSidecarFuture, PgSidecarReadCtx,
    PgSidecarRegistry, PgSidecarRegistryFrozen, Postgres, SidecarPayload, StorageError,
    Transaction,
};

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

impl PgEdgeSidecar for proxima_core::AgentLinkV1 {
    fn insert_edge_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        edge_id: EdgeId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.agent_link_v1
                    (edge_id, reason, confidence)
                 VALUES ($1, $2, $3)",
            )
            .bind(edge_id.into_inner())
            .bind(&self.reason)
            .bind(i16::from(self.confidence))
            .execute(tx)
            .await
            .map_err(crate::error::map_err)?;
            Ok(())
        })
    }
}

impl PgEdgePayload for proxima_core::AgentLinkV1 {
    fn load_edge_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        edge_ids: &'t [EdgeId],
    ) -> PgEdgePayloadBatchFuture<'t> {
        Box::pin(async move {
            let rows: Vec<(uuid::Uuid, String, i16)> = ctx
                .fetch_all_by_edge_ids(
                    "SELECT edge_id, reason, confidence
                       FROM proxima_core.agent_link_v1
                      WHERE edge_id = ANY($1::uuid[])",
                    edge_ids,
                )
                .await?;
            rows.into_iter()
                .map(|(edge_id, reason, confidence)| {
                    let confidence = u8::try_from(confidence).map_err(|_| {
                        StorageError::Internal(format!(
                            "agent_link_v1 confidence {confidence} out of range for edge {edge_id}"
                        ))
                    })?;
                    Ok((
                        EdgeId::new(edge_id),
                        SidecarPayload::edge(proxima_core::AgentLinkV1 { reason, confidence }),
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
    registry.add_goal::<proxima_core::TaskGoalV1>();
    registry.add_edge::<proxima_core::AgentLinkV1>();
    registry.add_cited_object::<proxima_core::UploadedBlobPayload>();
    registry.add_cited_object::<proxima_core::verbs::persist_mcp_call::McpCallIoV1>();
    // `UploadedBlobWholeV1` is a pure link with no sidecar table, so it needs
    // no entry here — `citation_mappings` is the whole mapping.
    registry.add_citation_mapping::<proxima_core::UploadedBlobPageSpanV1>();
}
