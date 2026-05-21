use super::*;

pub(super) async fn ingest_chat_fact<F: FactPayload + Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &F,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(format!("serialize payload: {err}")))?;
    let content_hash = blake3::hash(&payload_bytes);
    let now = OffsetDateTime::now_utc();
    let (object_schema, whole_schema) = match F::SCHEMA_ID {
        ChatStartedV1::SCHEMA_ID => (STARTED_OBJECT_SCHEMA, STARTED_WHOLE_SCHEMA),
        ChatMessageV1::SCHEMA_ID => (MESSAGE_OBJECT_SCHEMA, MESSAGE_WHOLE_SCHEMA),
        ChatReplyV1::SCHEMA_ID => (REPLY_OBJECT_SCHEMA, REPLY_WHOLE_SCHEMA),
        ChatEndRequestedV1::SCHEMA_ID => (END_REQUESTED_OBJECT_SCHEMA, END_REQUESTED_WHOLE_SCHEMA),
        ChatEndedV1::SCHEMA_ID => (ENDED_OBJECT_SCHEMA, ENDED_WHOLE_SCHEMA),
        _ => return Err(McpToolError::Other("unsupported chat payload".into())),
    };
    let draft = EventDraft {
        source_id: SourceId::new(CHAT_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(F::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(object_schema.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(whole_schema.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft).await
}

#[allow(clippy::too_many_lines)]
pub(super) async fn ingest_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &EventDraft,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&draft.owner);
    let existing: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT memory_id FROM proxima_core.memories WHERE event_id = $1")
            .bind(&event_id_bytes[..])
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sql)?;
    if let Some(memory_id) = existing {
        let seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sql)?;
        return Ok(crate::verbs::event_ingest::EventIngestOutcome {
            event_id,
            memory_id: MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let memory_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let cited_object_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    let cited_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, content_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      owner_org_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(draft.cited_object.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&draft.cited_object.content_hash[..])
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&event_id_bytes[..])
    .bind(draft.source_id.as_str())
    .bind(draft.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(draft.observed_at)
    .bind(draft.occurred_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 '00000000-0000-0000-0000-000000000000'::uuid)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, memory_id, cited_object_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(draft.citation_mapping.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(cited_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_memory_id, entity_kind, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityAppend', $5, 'Fact', $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(crate::verbs::event_ingest::EventIngestOutcome {
        event_id,
        memory_id: MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

pub(super) async fn insert_started_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatStartedV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_started_v1
            (memory_id, thread_key, started_by_self_perspective_memory_id,
             target_personality_instance_id, target_self_perspective_memory_id,
             title, idempotency_key, started_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(payload.started_by_self_perspective_memory_id)
    .bind(payload.target_personality_instance_id)
    .bind(payload.target_self_perspective_memory_id)
    .bind(&payload.title)
    .bind(&payload.idempotency_key)
    .bind(payload.started_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_message_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatMessageV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_message_v1
            (memory_id, thread_key, message, target_personality_instance_id,
             target_self_perspective_memory_id, sent_by_self_perspective_memory_id,
             parent_memory_id, context_memory_ids, context_goal_ids,
             idempotency_key, sent_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(&payload.message)
    .bind(payload.target_personality_instance_id)
    .bind(payload.target_self_perspective_memory_id)
    .bind(payload.sent_by_self_perspective_memory_id)
    .bind(payload.parent_memory_id)
    .bind(&payload.context_memory_ids)
    .bind(&payload.context_goal_ids)
    .bind(&payload.idempotency_key)
    .bind(payload.sent_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_reply_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatReplyV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_reply_v1
            (memory_id, message_memory_id, thread_key, reply,
             replied_by_personality_instance_id, replied_by_self_perspective_memory_id,
             context_memory_ids_used, idempotency_key, replied_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.message_memory_id)
    .bind(&payload.thread_key)
    .bind(&payload.reply)
    .bind(payload.replied_by_personality_instance_id)
    .bind(payload.replied_by_self_perspective_memory_id)
    .bind(&payload.context_memory_ids_used)
    .bind(&payload.idempotency_key)
    .bind(payload.replied_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_end_requested_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatEndRequestedV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_end_requested_v1
            (memory_id, thread_key, target_personality_instance_id,
             target_self_perspective_memory_id, requested_by_self_perspective_memory_id,
             reason, idempotency_key, requested_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(payload.target_personality_instance_id)
    .bind(payload.target_self_perspective_memory_id)
    .bind(payload.requested_by_self_perspective_memory_id)
    .bind(&payload.reason)
    .bind(&payload.idempotency_key)
    .bind(payload.requested_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_ended_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatEndedV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_ended_v1
            (memory_id, thread_key, request_memory_id,
             ended_by_personality_instance_id, ended_by_self_perspective_memory_id,
             summary_memory_id, idempotency_key, ended_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(payload.request_memory_id)
    .bind(payload.ended_by_personality_instance_id)
    .bind(payload.ended_by_self_perspective_memory_id)
    .bind(payload.summary_memory_id)
    .bind(&payload.idempotency_key)
    .bind(payload.ended_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_chat_compaction_abstraction(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
    payload: &ChatCompactionV1,
) -> Result<bool, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1,$2,$3,$4,$5,$6,'Abstraction',$7,$8,$9,$10,$11,0)
         ON CONFLICT (memory_id) DO NOTHING
         RETURNING memory_id",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(ChatCompactionV1::SCHEMA_ID)
    .bind(i32::try_from(ChatCompactionV1::SCHEMA_VERSION).unwrap_or(1))
    .bind(&payload.summary)
    .bind(MemoryOperatorKind::Wake)
    .bind(&ctx.author.model_id)
    .bind("core/compact_chat_thread-v1")
    .bind(payload.compacted_by_personality_instance_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sql)?;
    if inserted.is_none() {
        return Ok(false);
    }
    insert_compaction_sidecar(tx, MemoryId::new(memory_id), payload).await?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_instance_id, wake_chain_depth)
         VALUES ($1,$2,$3,$4,'EntityAppend','Abstraction',$5,$6,$7,$8,0)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(ChatCompactionV1::SCHEMA_ID)
    .bind(i32::try_from(ChatCompactionV1::SCHEMA_VERSION).unwrap_or(1))
    .bind(payload.compacted_by_personality_instance_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(true)
}

pub(super) async fn insert_chat_summary_abstraction(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
    payload: &ChatSummaryV1,
) -> Result<bool, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1,$2,$3,$4,$5,$6,'Abstraction',$7,$8,$9,$10,$11,0)
         ON CONFLICT (memory_id) DO NOTHING
         RETURNING memory_id",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(ChatSummaryV1::SCHEMA_ID)
    .bind(i32::try_from(ChatSummaryV1::SCHEMA_VERSION).unwrap_or(1))
    .bind(&payload.summary)
    .bind(MemoryOperatorKind::Wake)
    .bind(&ctx.author.model_id)
    .bind("core/end_chat-v1")
    .bind(payload.summarized_by_personality_instance_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sql)?;
    if inserted.is_none() {
        return Ok(false);
    }
    insert_summary_sidecar(tx, MemoryId::new(memory_id), payload).await?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_instance_id, wake_chain_depth)
         VALUES ($1,$2,$3,$4,'EntityAppend','Abstraction',$5,$6,$7,$8,0)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(ChatSummaryV1::SCHEMA_ID)
    .bind(i32::try_from(ChatSummaryV1::SCHEMA_VERSION).unwrap_or(1))
    .bind(payload.summarized_by_personality_instance_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(true)
}

pub(super) async fn insert_summary_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatSummaryV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_summary_v1
            (memory_id, thread_key, request_memory_id, ended_memory_id,
             summarized_by_personality_instance_id, summarized_by_self_perspective_memory_id,
             summary, included_memory_ids, context_memory_ids_used,
             idempotency_key, summarized_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(payload.request_memory_id)
    .bind(payload.ended_memory_id)
    .bind(payload.summarized_by_personality_instance_id)
    .bind(payload.summarized_by_self_perspective_memory_id)
    .bind(&payload.summary)
    .bind(&payload.included_memory_ids)
    .bind(&payload.context_memory_ids_used)
    .bind(&payload.idempotency_key)
    .bind(payload.summarized_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

pub(super) async fn insert_compaction_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatCompactionV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.chat_compaction_v1
            (memory_id, thread_key, compacted_by_personality_instance_id,
             compacted_by_self_perspective_memory_id, summary,
             included_memory_ids, context_memory_ids_used,
             idempotency_key, compacted_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(payload.compacted_by_personality_instance_id)
    .bind(payload.compacted_by_self_perspective_memory_id)
    .bind(&payload.summary)
    .bind(&payload.included_memory_ids)
    .bind(&payload.context_memory_ids_used)
    .bind(&payload.idempotency_key)
    .bind(payload.compacted_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn append_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    relation_id: &str,
    source_kind: EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_kind: EntityKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<uuid::Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(relation_id)
        .ok_or_else(|| McpToolError::Other(format!("relation {relation_id} not registered")))?;
    relation
        .descriptor
        .validate_edge_shape(
            source_kind.as_str(),
            target_kind.as_str(),
            authorship_kind.as_str(),
        )
        .map_err(McpToolError::LayeringViolation)?;
    let edge_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
         ON CONFLICT (edge_id) DO NOTHING",
    )
    .bind(edge_id)
    .bind(relation.descriptor.relation.as_str())
    .bind(relation.descriptor.class)
    .bind(source_kind)
    .bind(source_memory_id)
    .bind(source_goal_id)
    .bind(target_kind)
    .bind(target_memory_id)
    .bind(target_goal_id)
    .bind(authorship_kind)
    .bind(ctx.caller_self_perspective.map(MemoryId::into_inner))
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             edge_id, edge_relation,
             edge_source_kind, edge_source_memory_id, edge_source_goal_id,
             edge_target_kind, edge_target_memory_id, edge_target_goal_id)
         VALUES ($1,$2,$3,$4,'EdgeAppend',$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(edge_id)
    .bind(relation.descriptor.relation.as_str())
    .bind(source_kind)
    .bind(source_memory_id)
    .bind(source_goal_id)
    .bind(target_kind)
    .bind(target_memory_id)
    .bind(target_goal_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(edge_id)
}

pub(super) fn edge_authorship_for_ctx(ctx: &McpToolCtx) -> EdgeAuthorshipKind {
    if ctx.master_token_id.is_some() {
        EdgeAuthorshipKind::User
    } else {
        EdgeAuthorshipKind::ExternalAgent
    }
}

pub(super) fn chat_compaction_memory_id(
    owner: &Owner,
    thread_key: &str,
    idempotency_key: &str,
) -> uuid::Uuid {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let mut key = Vec::with_capacity(96 + thread_key.len() + idempotency_key.len());
    key.extend_from_slice(owner_kind.as_str().as_bytes());
    key.push(0);
    key.extend_from_slice(owner_id.as_bytes());
    key.push(0);
    key.extend_from_slice(owner_org_id.as_bytes());
    key.push(0);
    key.extend_from_slice(thread_key.as_bytes());
    key.push(0);
    key.extend_from_slice(idempotency_key.as_bytes());
    uuid::Uuid::new_v5(&CHAT_COMPACTION_DERIVED_NAMESPACE, &key)
}

pub(super) fn chat_summary_memory_id(
    owner: &Owner,
    thread_key: &str,
    request_memory_id: uuid::Uuid,
    idempotency_key: &str,
) -> uuid::Uuid {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let mut key = Vec::with_capacity(128 + thread_key.len() + idempotency_key.len());
    key.extend_from_slice(owner_kind.as_str().as_bytes());
    key.push(0);
    key.extend_from_slice(owner_id.as_bytes());
    key.push(0);
    key.extend_from_slice(owner_org_id.as_bytes());
    key.push(0);
    key.extend_from_slice(thread_key.as_bytes());
    key.push(0);
    key.extend_from_slice(request_memory_id.as_bytes());
    key.push(0);
    key.extend_from_slice(idempotency_key.as_bytes());
    uuid::Uuid::new_v5(&CHAT_SUMMARY_DERIVED_NAMESPACE, &key)
}

pub(super) fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, McpToolError> {
    let trimmed = value.trim();
    if trimmed.len() < min || trimmed.len() > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} length must be between {min} and {max}"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn map_sql(err: sqlx::Error) -> McpToolError {
    McpToolError::Storage(StorageError::Internal(err.to_string()))
}

pub(super) fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
