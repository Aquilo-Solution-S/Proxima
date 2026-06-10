//! `ChatStore` — Postgres data access for the `core/*chat*` MCP tools.
//!
//! Like `ApprovalStore` and `InterventionStore`, `ChatStore` is its own
//! capability trait, so its `PgStorage` impl lives here next to the verb
//! bodies. `proxima-core`'s chat tools call these through `Storage`'s
//! supertrait bound.
//!
//! The write verbs are composite atomic operations. Each reuses
//! [`ingest_event_in_tx`] and [`append_edge_in_tx`] so the Fact / Abstraction
//! materialization, the typed sidecar rows, and the provenance edges land in
//! one transaction.
//!
//! The read verbs project SQL rows into wide tuples before mapping them
//! into the `Loaded*` types — `type_complexity` is allowed module-wide for
//! that, as in `verbs/consolidate`.
#![allow(clippy::type_complexity)]

use std::collections::HashMap;

use async_trait::async_trait;
use proxima_core::{
    AbstractionPayload, ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement,
    ApprovalTargetKind, ApprovalVoteVerdict, ApprovalVoterKind, CORE_DERIVED_FROM_RELATION,
    CORE_HAS_APPROVAL_DECISION_RELATION, CORE_HAS_APPROVAL_POLICY_RELATION,
    CORE_RECEIVES_CHAT_END_REQUEST_RELATION, CORE_RECEIVES_CHAT_MESSAGE_RELATION,
    CORE_REPLIES_TO_MESSAGE_RELATION, CORE_VOTES_ON_RELATION, ChatCompactionV1, ChatEndRequestedV1,
    ChatEndedV1, ChatFactEmitOutcome, ChatMessageV1, ChatReplyV1, ChatStartedV1, ChatStore,
    ChatSummaryV1, CompactChatThreadEmitOutcome, CompactChatThreadInput, EdgeAuthorshipKind,
    EdgeId, EmitChatMessageInput, EmitChatReplyInput, EndChatEmitOutcome, EndChatInput, EntityKind,
    ExistingChatEnd, FlavorRegistryFrozen, LoadedApprovalDecision, LoadedApprovalPolicy,
    LoadedApprovalVote, LoadedCompaction, LoadedEndRequest, LoadedEnded, LoadedMessage,
    LoadedReply, LoadedStarted, LoadedSummary, LoadedThreadEdge, MemoryId, MemoryOperatorKind,
    Owner, OwnerPrincipalKind, Principal, RequestEndChatInput, StartChatEmitOutcome,
    StartChatInput, StorageError, ThreadApprovalCountedVoteRaw, chat_fact_event_draft,
};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use crate::PgStorage;
use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::event_ingest::ingest_event_in_tx;

#[async_trait]
impl ChatStore for PgStorage {
    async fn chat_existing_end_by_request(
        &self,
        owner: &Owner,
        request_memory_id: MemoryId,
    ) -> Result<Option<ExistingChatEnd>, StorageError> {
        chat_existing_end_by_request(self.pool(), owner, request_memory_id).await
    }

    async fn chat_summary_source_memory_ids(
        &self,
        owner: &Owner,
        thread_key: &str,
    ) -> Result<Vec<uuid::Uuid>, StorageError> {
        chat_summary_source_memory_ids(self.pool(), owner, thread_key).await
    }

    async fn chat_count_visible_memories(
        &self,
        owner: &Owner,
        memory_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        chat_count_visible_memories(self.pool(), owner, memory_ids).await
    }

    async fn chat_count_thread_source_memories(
        &self,
        owner: &Owner,
        thread_key: &str,
        memory_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        chat_count_thread_source_memories(self.pool(), owner, thread_key, memory_ids).await
    }

    async fn chat_memory_kinds(
        &self,
        owner: &Owner,
        memory_ids: &[uuid::Uuid],
    ) -> Result<Vec<(uuid::Uuid, String)>, StorageError> {
        chat_memory_kinds(self.pool(), owner, memory_ids).await
    }

    async fn chat_count_visible_goals(
        &self,
        owner: &Owner,
        goal_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        chat_count_visible_goals(self.pool(), owner, goal_ids).await
    }

    async fn chat_load_message(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<ChatMessageV1>, StorageError> {
        chat_load_message(self.pool(), owner, memory_id).await
    }

    async fn chat_parent_thread_key(
        &self,
        owner: &Owner,
        parent_memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        chat_parent_thread_key(self.pool(), owner, parent_memory_id).await
    }

    async fn chat_load_end_request(
        &self,
        owner: &Owner,
        request_memory_id: MemoryId,
    ) -> Result<Option<ChatEndRequestedV1>, StorageError> {
        chat_load_end_request(self.pool(), owner, request_memory_id).await
    }

    async fn chat_policy_fact_target(
        &self,
        owner: &Owner,
        policy_memory_id: uuid::Uuid,
    ) -> Result<Option<Option<uuid::Uuid>>, StorageError> {
        chat_policy_fact_target(self.pool(), owner, policy_memory_id).await
    }

    async fn chat_policy_id_for_vote_or_decision(
        &self,
        owner: &Owner,
        memory_id: uuid::Uuid,
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        chat_policy_id_for_vote_or_decision(self.pool(), owner, memory_id).await
    }

    async fn chat_thread_key_for_memory(
        &self,
        owner: &Owner,
        memory_id: uuid::Uuid,
    ) -> Result<Option<String>, StorageError> {
        chat_thread_key_for_memory(self.pool(), owner, memory_id).await
    }

    async fn chat_thread_started(
        &self,
        owner: &Owner,
        thread_key: &str,
    ) -> Result<Option<LoadedStarted>, StorageError> {
        chat_thread_started(self.pool(), owner, thread_key).await
    }

    async fn chat_thread_messages(
        &self,
        owner: &Owner,
        thread_key: &str,
        limit: i64,
    ) -> Result<Vec<LoadedMessage>, StorageError> {
        chat_thread_messages(self.pool(), owner, thread_key, limit).await
    }

    async fn chat_thread_replies(
        &self,
        owner: &Owner,
        thread_key: &str,
        limit: i64,
    ) -> Result<Vec<LoadedReply>, StorageError> {
        chat_thread_replies(self.pool(), owner, thread_key, limit).await
    }

    async fn chat_thread_end_requests(
        &self,
        owner: &Owner,
        thread_key: &str,
        limit: i64,
    ) -> Result<Vec<LoadedEndRequest>, StorageError> {
        chat_thread_end_requests(self.pool(), owner, thread_key, limit).await
    }

    async fn chat_thread_ended(
        &self,
        owner: &Owner,
        thread_key: &str,
    ) -> Result<Option<LoadedEnded>, StorageError> {
        chat_thread_ended(self.pool(), owner, thread_key).await
    }

    async fn chat_thread_compactions(
        &self,
        owner: &Owner,
        thread_key: &str,
        limit: i64,
    ) -> Result<Vec<LoadedCompaction>, StorageError> {
        chat_thread_compactions(self.pool(), owner, thread_key, limit).await
    }

    async fn chat_thread_summaries(
        &self,
        owner: &Owner,
        thread_key: &str,
        limit: i64,
    ) -> Result<Vec<LoadedSummary>, StorageError> {
        chat_thread_summaries(self.pool(), owner, thread_key, limit).await
    }

    async fn chat_thread_approval_policies(
        &self,
        owner: &Owner,
        target_memory_ids: &[uuid::Uuid],
        limit: i64,
    ) -> Result<Vec<LoadedApprovalPolicy>, StorageError> {
        chat_thread_approval_policies(self.pool(), owner, target_memory_ids, limit).await
    }

    async fn chat_thread_approval_votes(
        &self,
        owner: &Owner,
        policy_memory_ids: &[uuid::Uuid],
        limit: i64,
    ) -> Result<Vec<LoadedApprovalVote>, StorageError> {
        chat_thread_approval_votes(self.pool(), owner, policy_memory_ids, limit).await
    }

    async fn chat_thread_approval_decisions(
        &self,
        owner: &Owner,
        policy_memory_ids: &[uuid::Uuid],
        limit: i64,
    ) -> Result<Vec<LoadedApprovalDecision>, StorageError> {
        chat_thread_approval_decisions(self.pool(), owner, policy_memory_ids, limit).await
    }

    async fn chat_thread_edges(
        &self,
        owner: &Owner,
        thread_memory_ids: &[uuid::Uuid],
        limit: i64,
    ) -> Result<Vec<LoadedThreadEdge>, StorageError> {
        chat_thread_edges(self.pool(), owner, thread_memory_ids, limit).await
    }

    async fn start_chat_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &StartChatInput,
    ) -> Result<StartChatEmitOutcome, StorageError> {
        start_chat_atomic(self.pool(), registry, input).await
    }

    async fn emit_chat_message_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitChatMessageInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        emit_chat_message_atomic(self.pool(), registry, input).await
    }

    async fn emit_chat_reply_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitChatReplyInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        emit_chat_reply_atomic(self.pool(), registry, input).await
    }

    async fn request_end_chat_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &RequestEndChatInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        request_end_chat_atomic(self.pool(), registry, input).await
    }

    async fn compact_chat_thread_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &CompactChatThreadInput,
    ) -> Result<CompactChatThreadEmitOutcome, StorageError> {
        compact_chat_thread_atomic(self.pool(), registry, input).await
    }

    async fn end_chat_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EndChatInput,
    ) -> Result<EndChatEmitOutcome, StorageError> {
        end_chat_atomic(self.pool(), registry, input).await
    }
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

// ---------------------------------------------------------------------------
// Read verbs
// ---------------------------------------------------------------------------

async fn chat_existing_end_by_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<ExistingChatEnd>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT e.memory_id, e.summary_memory_id
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.request_memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY e.ended_at ASC, e.memory_id ASC
          LIMIT 1",
    )
    .bind(request_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(ended, summary)| ExistingChatEnd {
        ended_memory_id: MemoryId::new(ended),
        summary_memory_id: MemoryId::new(summary),
    }))
}

async fn chat_summary_source_memory_ids(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT s.memory_id
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT q.memory_id
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT a.memory_id
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT r.memory_id
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT s.memory_id
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
         WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT c.memory_id
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let mut out: Vec<_> = rows.into_iter().map(|(id,)| id).collect();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

async fn chat_count_visible_memories(
    pool: &PgPool,
    owner: &Owner,
    memory_ids: &[uuid::Uuid],
) -> Result<i64, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND memory_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_ids)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

async fn chat_count_thread_source_memories(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    memory_ids: &[uuid::Uuid],
) -> Result<i64, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "WITH thread_memory AS (
             SELECT s.memory_id
               FROM proxima_core.chat_started_v1 s
               JOIN proxima_core.memories m USING (memory_id)
              WHERE s.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT q.memory_id
               FROM proxima_core.chat_message_v1 q
               JOIN proxima_core.memories m USING (memory_id)
              WHERE q.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT r.memory_id
               FROM proxima_core.chat_reply_v1 r
               JOIN proxima_core.memories m USING (memory_id)
              WHERE r.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT e.memory_id
               FROM proxima_core.chat_end_requested_v1 e
               JOIN proxima_core.memories m USING (memory_id)
              WHERE e.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT c.memory_id
               FROM proxima_core.chat_compaction_v1 c
               JOIN proxima_core.memories m USING (memory_id)
              WHERE c.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT s.memory_id
               FROM proxima_core.chat_summary_v1 s
               JOIN proxima_core.memories m USING (memory_id)
              WHERE s.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
         )
         SELECT count(DISTINCT memory_id)
           FROM thread_memory
          WHERE memory_id = ANY($5::uuid[])",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_ids)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

async fn chat_memory_kinds(
    pool: &PgPool,
    owner: &Owner,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<(uuid::Uuid, String)>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_as(
        "SELECT memory_id, COALESCE(kind::text, 'Fact') AS kind
           FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND memory_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

async fn chat_count_visible_goals(
    pool: &PgPool,
    owner: &Owner,
    goal_ids: &[uuid::Uuid],
) -> Result<i64, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.goals
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND goal_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(goal_ids)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

type ChatMessageRow = (
    String,
    String,
    uuid::Uuid,
    uuid::Uuid,
    uuid::Uuid,
    Option<uuid::Uuid>,
    Vec<uuid::Uuid>,
    Vec<uuid::Uuid>,
    String,
    OffsetDateTime,
);

fn chat_message_from_row(row: ChatMessageRow) -> ChatMessageV1 {
    let (
        thread_key,
        message,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        sent_by_self_perspective_memory_id,
        parent_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        sent_at,
    ) = row;
    ChatMessageV1 {
        thread_key,
        message,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        sent_by_self_perspective_memory_id,
        parent_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        sent_at,
    }
}

async fn chat_load_message(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<ChatMessageV1>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<ChatMessageRow> = sqlx::query_as(
        "SELECT q.thread_key, q.message, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.sent_by_self_perspective_memory_id,
                q.parent_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.sent_at
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(chat_message_from_row))
}

async fn chat_parent_thread_key(
    pool: &PgPool,
    owner: &Owner,
    parent_memory_id: MemoryId,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT parent.thread_key
           FROM (
                 SELECT q.thread_key
                   FROM proxima_core.chat_message_v1 q
                   JOIN proxima_core.memories m USING (memory_id)
                  WHERE q.memory_id = $1
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4
                 UNION ALL
                 SELECT r.thread_key
                   FROM proxima_core.chat_reply_v1 r
                   JOIN proxima_core.memories m USING (memory_id)
                  WHERE r.memory_id = $1
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4
                ) parent
          LIMIT 1",
    )
    .bind(parent_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(thread_key,)| thread_key))
}

async fn chat_load_end_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<ChatEndRequestedV1>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT r.thread_key, r.target_personality_instance_id,
                r.target_self_perspective_memory_id,
                r.requested_by_self_perspective_memory_id, r.reason,
                r.idempotency_key, r.requested_at
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(request_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(
        |(
            thread_key,
            target_personality_instance_id,
            target_self_perspective_memory_id,
            requested_by_self_perspective_memory_id,
            reason,
            idempotency_key,
            requested_at,
        )| ChatEndRequestedV1 {
            thread_key,
            target_personality_instance_id,
            target_self_perspective_memory_id,
            requested_by_self_perspective_memory_id,
            reason,
            idempotency_key,
            requested_at,
        },
    ))
}

async fn chat_policy_fact_target(
    pool: &PgPool,
    owner: &Owner,
    policy_memory_id: uuid::Uuid,
) -> Result<Option<Option<uuid::Uuid>>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT p.target_memory_id
           FROM proxima_core.approval_policy_v1 p
           JOIN proxima_core.memories m USING (memory_id)
          WHERE p.memory_id = $1
            AND p.target_kind = 'fact'
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(policy_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

async fn chat_policy_id_for_vote_or_decision(
    pool: &PgPool,
    owner: &Owner,
    memory_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT policy_memory_id
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT policy_memory_id
           FROM proxima_core.approval_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          LIMIT 1",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

async fn chat_thread_key_for_memory(
    pool: &PgPool,
    owner: &Owner,
    memory_id: uuid::Uuid,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT s.thread_key
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT q.thread_key
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT a.thread_key
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT r.thread_key
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT e.thread_key
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT s.thread_key
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT c.thread_key
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          LIMIT 1",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

async fn chat_thread_started(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
) -> Result<Option<LoadedStarted>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT s.memory_id, s.thread_key, s.started_by_self_perspective_memory_id,
                s.target_personality_instance_id, s.target_self_perspective_memory_id,
                s.title, s.idempotency_key, s.started_at
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY s.started_at ASC, s.memory_id ASC
          LIMIT 1",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(
        |(
            memory_id,
            thread_key,
            started_by_self_perspective_memory_id,
            target_personality_instance_id,
            target_self_perspective_memory_id,
            title,
            idempotency_key,
            started_at,
        )| LoadedStarted {
            memory_id: MemoryId::new(memory_id),
            payload: ChatStartedV1 {
                thread_key,
                started_by_self_perspective_memory_id,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                title,
                idempotency_key,
                started_at,
            },
        },
    ))
}

async fn chat_thread_messages(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedMessage>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<uuid::Uuid>,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT q.memory_id, q.thread_key, q.message, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.sent_by_self_perspective_memory_id,
                q.parent_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.sent_at
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY q.sent_at ASC, q.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                message,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                sent_by_self_perspective_memory_id,
                parent_memory_id,
                context_memory_ids,
                context_goal_ids,
                idempotency_key,
                sent_at,
            )| LoadedMessage {
                memory_id: MemoryId::new(memory_id),
                payload: chat_message_from_row((
                    thread_key,
                    message,
                    target_personality_instance_id,
                    target_self_perspective_memory_id,
                    sent_by_self_perspective_memory_id,
                    parent_memory_id,
                    context_memory_ids,
                    context_goal_ids,
                    idempotency_key,
                    sent_at,
                )),
            },
        )
        .collect())
}

async fn chat_thread_replies(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedReply>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        String,
        String,
        uuid::Uuid,
        uuid::Uuid,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT a.memory_id, a.message_memory_id, a.thread_key, a.reply,
                a.replied_by_personality_instance_id,
                a.replied_by_self_perspective_memory_id,
                a.context_memory_ids_used, a.idempotency_key, a.replied_at
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY a.replied_at ASC, a.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                message_memory_id,
                thread_key,
                reply,
                replied_by_personality_instance_id,
                replied_by_self_perspective_memory_id,
                context_memory_ids_used,
                idempotency_key,
                replied_at,
            )| LoadedReply {
                memory_id: MemoryId::new(memory_id),
                payload: ChatReplyV1 {
                    message_memory_id,
                    thread_key,
                    reply,
                    replied_by_personality_instance_id,
                    replied_by_self_perspective_memory_id,
                    context_memory_ids_used,
                    idempotency_key,
                    replied_at,
                },
            },
        )
        .collect())
}

async fn chat_thread_end_requests(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedEndRequest>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT r.memory_id, r.thread_key, r.target_personality_instance_id,
                r.target_self_perspective_memory_id,
                r.requested_by_self_perspective_memory_id, r.reason,
                r.idempotency_key, r.requested_at
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY r.requested_at ASC, r.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                requested_by_self_perspective_memory_id,
                reason,
                idempotency_key,
                requested_at,
            )| LoadedEndRequest {
                memory_id: MemoryId::new(memory_id),
                payload: ChatEndRequestedV1 {
                    thread_key,
                    target_personality_instance_id,
                    target_self_perspective_memory_id,
                    requested_by_self_perspective_memory_id,
                    reason,
                    idempotency_key,
                    requested_at,
                },
            },
        )
        .collect())
}

async fn chat_thread_ended(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
) -> Result<Option<LoadedEnded>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT e.memory_id, e.thread_key, e.request_memory_id,
                e.ended_by_personality_instance_id,
                e.ended_by_self_perspective_memory_id, e.summary_memory_id,
                e.idempotency_key, e.ended_at
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY e.ended_at DESC, e.memory_id DESC
          LIMIT 1",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(
        |(
            memory_id,
            thread_key,
            request_memory_id,
            ended_by_personality_instance_id,
            ended_by_self_perspective_memory_id,
            summary_memory_id,
            idempotency_key,
            ended_at,
        )| LoadedEnded {
            memory_id: MemoryId::new(memory_id),
            payload: ChatEndedV1 {
                thread_key,
                request_memory_id,
                ended_by_personality_instance_id,
                ended_by_self_perspective_memory_id,
                summary_memory_id,
                idempotency_key,
                ended_at,
            },
        },
    ))
}

async fn chat_thread_compactions(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedCompaction>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        String,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT c.memory_id, c.thread_key, c.compacted_by_personality_instance_id,
                c.compacted_by_self_perspective_memory_id, c.summary,
                c.included_memory_ids, c.context_memory_ids_used,
                c.idempotency_key, c.compacted_at
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY c.compacted_at ASC, c.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                compacted_by_personality_instance_id,
                compacted_by_self_perspective_memory_id,
                summary,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                compacted_at,
            )| LoadedCompaction {
                memory_id: MemoryId::new(memory_id),
                payload: ChatCompactionV1 {
                    thread_key,
                    compacted_by_personality_instance_id,
                    compacted_by_self_perspective_memory_id,
                    summary,
                    included_memory_ids,
                    context_memory_ids_used,
                    idempotency_key,
                    compacted_at,
                },
            },
        )
        .collect())
}

async fn chat_thread_summaries(
    pool: &PgPool,
    owner: &Owner,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedSummary>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        String,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT s.memory_id, s.thread_key, s.request_memory_id, s.ended_memory_id,
                s.summarized_by_personality_instance_id,
                s.summarized_by_self_perspective_memory_id, s.summary,
                s.included_memory_ids, s.context_memory_ids_used,
                s.idempotency_key, s.summarized_at
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY s.summarized_at ASC, s.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                request_memory_id,
                ended_memory_id,
                summarized_by_personality_instance_id,
                summarized_by_self_perspective_memory_id,
                summary,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                summarized_at,
            )| LoadedSummary {
                memory_id: MemoryId::new(memory_id),
                payload: ChatSummaryV1 {
                    thread_key,
                    request_memory_id,
                    ended_memory_id,
                    summarized_by_personality_instance_id,
                    summarized_by_self_perspective_memory_id,
                    summary,
                    included_memory_ids,
                    context_memory_ids_used,
                    idempotency_key,
                    summarized_at,
                },
            },
        )
        .collect())
}

async fn chat_thread_approval_policies(
    pool: &PgPool,
    owner: &Owner,
    target_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalPolicy>, StorageError> {
    if target_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        ApprovalTargetKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        String,
        String,
        serde_json::Value,
        serde_json::Value,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT p.memory_id, p.target_kind, p.target_memory_id, p.target_goal_id,
                p.title, p.summary, p.eligible_voters_json, p.requirements_json,
                p.idempotency_key, p.created_at
           FROM proxima_core.approval_policy_v1 p
           JOIN proxima_core.memories m USING (memory_id)
          WHERE p.target_kind = 'fact'
            AND p.target_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY p.created_at ASC, p.memory_id ASC
          LIMIT $5",
    )
    .bind(target_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    rows.into_iter()
        .map(
            |(
                memory_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                title,
                summary,
                eligible_voters_json,
                requirements_json,
                idempotency_key,
                created_at,
            )| {
                let eligible_voters: Vec<ApprovalEligibleVoter> =
                    serde_json::from_value(eligible_voters_json).map_err(|err| {
                        StorageError::Internal(format!("decode eligible voters: {err}"))
                    })?;
                let requirements: Vec<ApprovalRequirement> =
                    serde_json::from_value(requirements_json).map_err(|err| {
                        StorageError::Internal(format!("decode approval requirements: {err}"))
                    })?;
                Ok(LoadedApprovalPolicy {
                    memory_id: MemoryId::new(memory_id),
                    target_kind,
                    target_memory_id,
                    target_goal_id,
                    title,
                    summary,
                    eligible_voters,
                    requirements,
                    idempotency_key,
                    created_at,
                })
            },
        )
        .collect()
}

async fn chat_thread_approval_votes(
    pool: &PgPool,
    owner: &Owner,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalVote>, StorageError> {
    if policy_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        String,
        ApprovalVoterKind,
        Option<String>,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        ApprovalVoteVerdict,
        String,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT v.memory_id, v.policy_memory_id, v.voter_key, v.voter_kind,
                v.role, v.personality_instance_id, v.self_perspective_memory_id,
                v.master_token_id, v.verdict, v.rationale, v.idempotency_key, v.voted_at
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.policy_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY v.voted_at ASC, v.memory_id ASC
          LIMIT $5",
    )
    .bind(policy_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                policy_memory_id,
                voter_key,
                voter_kind,
                role,
                personality_instance_id,
                self_perspective_memory_id,
                master_token_id,
                verdict,
                rationale,
                idempotency_key,
                voted_at,
            )| LoadedApprovalVote {
                memory_id: MemoryId::new(memory_id),
                policy_memory_id,
                voter_key,
                voter_kind,
                role,
                personality_instance_id,
                self_perspective_memory_id,
                master_token_id,
                verdict,
                rationale,
                idempotency_key,
                voted_at,
            },
        )
        .collect())
}

async fn chat_thread_approval_decisions(
    pool: &PgPool,
    owner: &Owner,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalDecision>, StorageError> {
    if policy_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        ApprovalTargetKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        ApprovalDecision,
        String,
        serde_json::Value,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT d.memory_id, d.policy_memory_id, d.target_kind, d.target_memory_id,
                d.target_goal_id, d.decision, d.reason, d.counted_votes_json,
                d.idempotency_key, d.decided_at
           FROM proxima_core.approval_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.policy_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY d.decided_at ASC, d.memory_id ASC
          LIMIT $5",
    )
    .bind(policy_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    rows.into_iter()
        .map(
            |(
                memory_id,
                policy_memory_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                decision,
                reason,
                counted_votes_json,
                idempotency_key,
                decided_at,
            )| {
                let counted_votes: Vec<ThreadApprovalCountedVoteRaw> =
                    serde_json::from_value(counted_votes_json).map_err(|err| {
                        StorageError::Internal(format!("decode counted votes: {err}"))
                    })?;
                Ok(LoadedApprovalDecision {
                    memory_id: MemoryId::new(memory_id),
                    policy_memory_id,
                    target_kind,
                    target_memory_id,
                    target_goal_id,
                    decision,
                    reason,
                    counted_votes,
                    idempotency_key,
                    decided_at,
                })
            },
        )
        .collect()
}

fn endpoint_in_thread(endpoint: Option<uuid::Uuid>, thread_memory_ids: &[uuid::Uuid]) -> bool {
    endpoint.is_some_and(|id| thread_memory_ids.contains(&id))
}

async fn chat_thread_edges(
    pool: &PgPool,
    owner: &Owner,
    thread_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedThreadEdge>, StorageError> {
    if thread_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        EntityKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        EntityKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        EdgeAuthorshipKind,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT edge_id, relation, source_kind, source_memory_id, source_goal_id,
                target_kind, target_memory_id, target_goal_id, authorship_kind, created_at
           FROM proxima_core.edges
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND relation = ANY($4::text[])
            AND (source_memory_id = ANY($5::uuid[])
                 OR target_memory_id = ANY($5::uuid[]))
          ORDER BY created_at ASC, edge_id ASC
          LIMIT $6",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind([
        CORE_RECEIVES_CHAT_MESSAGE_RELATION,
        CORE_REPLIES_TO_MESSAGE_RELATION,
        CORE_HAS_APPROVAL_POLICY_RELATION,
        CORE_VOTES_ON_RELATION,
        CORE_HAS_APPROVAL_DECISION_RELATION,
        CORE_DERIVED_FROM_RELATION,
        CORE_RECEIVES_CHAT_END_REQUEST_RELATION,
    ])
    .bind(thread_memory_ids)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter(
            |(_, relation, _, source_memory_id, _, _, target_memory_id, _, _, _)| {
                if relation == CORE_RECEIVES_CHAT_MESSAGE_RELATION
                    || relation == CORE_RECEIVES_CHAT_END_REQUEST_RELATION
                {
                    endpoint_in_thread(*target_memory_id, thread_memory_ids)
                } else {
                    endpoint_in_thread(*source_memory_id, thread_memory_ids)
                        && endpoint_in_thread(*target_memory_id, thread_memory_ids)
                }
            },
        )
        .map(
            |(
                edge_id,
                relation,
                source_kind,
                source_memory_id,
                source_goal_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                authorship_kind,
                created_at,
            )| LoadedThreadEdge {
                edge_id: EdgeId::new(edge_id),
                relation,
                source_kind,
                source_memory_id,
                source_goal_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                authorship_kind,
                created_at,
            },
        )
        .collect())
}

// ---------------------------------------------------------------------------
// Write verbs
// ---------------------------------------------------------------------------

/// Append one memory-to-memory edge inside `tx`, returning the new edge id.
#[allow(clippy::too_many_arguments)]
async fn append_edge(
    tx: &mut Transaction<'_, Postgres>,
    registry: &FlavorRegistryFrozen,
    relation_id: &str,
    owner: &Owner,
    source_kind: EntityKind,
    source_memory_id: uuid::Uuid,
    target_kind: EntityKind,
    target_memory_id: uuid::Uuid,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner: uuid::Uuid,
) -> Result<uuid::Uuid, StorageError> {
    let relation = registry
        .resolve_relation(relation_id)
        .ok_or_else(|| StorageError::Internal(format!("relation {relation_id} not registered")))?;
    let edge_id = uuid::Uuid::now_v7();
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id,
            relation,
            source_kind,
            source_memory_id: Some(source_memory_id),
            source_goal_id: None,
            target_kind,
            target_memory_id: Some(target_memory_id),
            target_goal_id: None,
            authorship_kind,
            authorship_owner_memory_id: Some(authorship_owner),
            owner,
        },
        None,
    )
    .await?;
    Ok(edge_id)
}

async fn insert_started_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatStartedV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_message_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatMessageV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_reply_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatReplyV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_end_requested_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatEndRequestedV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_ended_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatEndedV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_compaction_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatCompactionV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

async fn insert_summary_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ChatSummaryV1,
) -> Result<(), StorageError> {
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
    .map_err(map_err)?;
    Ok(())
}

/// Materialize a chat-compaction Abstraction memory + sidecar + `change_event`.
/// Returns `false` on idempotent replay (the `memory_id` already existed).
async fn insert_chat_compaction_abstraction(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    model_id: &str,
    memory_id: uuid::Uuid,
    payload: &ChatCompactionV1,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
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
    .bind(model_id)
    .bind("core/compact_chat_thread-v1")
    .bind(payload.compacted_by_personality_instance_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
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
    .map_err(map_err)?;
    Ok(true)
}

/// Materialize a chat-summary Abstraction memory + sidecar + `change_event`.
/// Returns `false` on idempotent replay (the `memory_id` already existed).
async fn insert_chat_summary_abstraction(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    model_id: &str,
    memory_id: uuid::Uuid,
    payload: &ChatSummaryV1,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
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
    .bind(model_id)
    .bind("core/end_chat-v1")
    .bind(payload.summarized_by_personality_instance_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
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
    .map_err(map_err)?;
    Ok(true)
}

async fn start_chat_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &StartChatInput,
) -> Result<StartChatEmitOutcome, StorageError> {
    let started_draft = chat_fact_event_draft(&input.owner, &input.started)?;
    let message_draft = chat_fact_event_draft(&input.owner, &input.message)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let started_outcome = ingest_event_in_tx(&mut tx, &started_draft).await?;
    let message_outcome = ingest_event_in_tx(&mut tx, &message_draft).await?;
    let idempotent_replay = started_outcome.idempotent_replay || message_outcome.idempotent_replay;
    let message_edge_id = if idempotent_replay {
        None
    } else {
        insert_started_sidecar(&mut tx, started_outcome.memory_id, &input.started).await?;
        insert_message_sidecar(&mut tx, message_outcome.memory_id, &input.message).await?;
        Some(
            append_edge(
                &mut tx,
                registry,
                CORE_RECEIVES_CHAT_MESSAGE_RELATION,
                &input.owner,
                EntityKind::Perspective,
                input.message.target_self_perspective_memory_id,
                EntityKind::Fact,
                message_outcome.memory_id.into_inner(),
                input.edge_authorship,
                input.caller_self.into_inner(),
            )
            .await?,
        )
    };
    tx.commit().await.map_err(map_err)?;
    Ok(StartChatEmitOutcome {
        started_memory_id: started_outcome.memory_id,
        message_memory_id: message_outcome.memory_id,
        message_edge_id,
        idempotent_replay,
    })
}

async fn emit_chat_message_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitChatMessageInput,
) -> Result<ChatFactEmitOutcome, StorageError> {
    let draft = chat_fact_event_draft(&input.owner, &input.message)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let edge_id = if outcome.idempotent_replay {
        None
    } else {
        insert_message_sidecar(&mut tx, outcome.memory_id, &input.message).await?;
        Some(
            append_edge(
                &mut tx,
                registry,
                CORE_RECEIVES_CHAT_MESSAGE_RELATION,
                &input.owner,
                EntityKind::Perspective,
                input.message.target_self_perspective_memory_id,
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                input.edge_authorship,
                input.caller_self.into_inner(),
            )
            .await?,
        )
    };
    tx.commit().await.map_err(map_err)?;
    Ok(ChatFactEmitOutcome {
        memory_id: outcome.memory_id,
        edge_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn emit_chat_reply_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitChatReplyInput,
) -> Result<ChatFactEmitOutcome, StorageError> {
    let draft = chat_fact_event_draft(&input.owner, &input.reply)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let edge_id = if outcome.idempotent_replay {
        None
    } else {
        insert_reply_sidecar(&mut tx, outcome.memory_id, &input.reply).await?;
        Some(
            append_edge(
                &mut tx,
                registry,
                CORE_REPLIES_TO_MESSAGE_RELATION,
                &input.owner,
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                EntityKind::Fact,
                input.message_memory_id.into_inner(),
                input.edge_authorship,
                input.caller_self.into_inner(),
            )
            .await?,
        )
    };
    tx.commit().await.map_err(map_err)?;
    Ok(ChatFactEmitOutcome {
        memory_id: outcome.memory_id,
        edge_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn request_end_chat_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &RequestEndChatInput,
) -> Result<ChatFactEmitOutcome, StorageError> {
    let draft = chat_fact_event_draft(&input.owner, &input.request)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let edge_id = if outcome.idempotent_replay {
        None
    } else {
        insert_end_requested_sidecar(&mut tx, outcome.memory_id, &input.request).await?;
        Some(
            append_edge(
                &mut tx,
                registry,
                CORE_RECEIVES_CHAT_END_REQUEST_RELATION,
                &input.owner,
                EntityKind::Perspective,
                input.request.target_self_perspective_memory_id,
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                input.edge_authorship,
                input.caller_self.into_inner(),
            )
            .await?,
        )
    };
    tx.commit().await.map_err(map_err)?;
    Ok(ChatFactEmitOutcome {
        memory_id: outcome.memory_id,
        edge_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn compact_chat_thread_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &CompactChatThreadInput,
) -> Result<CompactChatThreadEmitOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let inserted = insert_chat_compaction_abstraction(
        &mut tx,
        &input.owner,
        &input.model_id,
        input.compaction_memory_id,
        &input.payload,
    )
    .await?;
    let mut edge_ids = Vec::new();
    if inserted {
        for (source, target_kind) in &input.classified_sources {
            edge_ids.push(
                append_edge(
                    &mut tx,
                    registry,
                    CORE_DERIVED_FROM_RELATION,
                    &input.owner,
                    EntityKind::Abstraction,
                    input.compaction_memory_id,
                    *target_kind,
                    *source,
                    EdgeAuthorshipKind::ExternalAgent,
                    input.caller_self.into_inner(),
                )
                .await?,
            );
        }
    }
    tx.commit().await.map_err(map_err)?;
    Ok(CompactChatThreadEmitOutcome { inserted, edge_ids })
}

async fn end_chat_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EndChatInput,
) -> Result<EndChatEmitOutcome, StorageError> {
    let ended_draft = chat_fact_event_draft(&input.owner, &input.ended)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let ended_outcome = ingest_event_in_tx(&mut tx, &ended_draft).await?;
    let mut summary_inserted = false;
    let mut edge_ids = Vec::new();
    if !ended_outcome.idempotent_replay {
        let ended_memory_id = ended_outcome.memory_id.into_inner();
        insert_ended_sidecar(&mut tx, ended_outcome.memory_id, &input.ended).await?;
        // The chat-ended Fact id is only known after ingest; the tool
        // passes a nil placeholder for it on the summary payload.
        let mut summary = input.summary.clone();
        summary.ended_memory_id = ended_memory_id;
        summary_inserted = insert_chat_summary_abstraction(
            &mut tx,
            &input.owner,
            &input.model_id,
            input.summary_memory_id,
            &summary,
        )
        .await?;
        if summary_inserted {
            let class_map: HashMap<uuid::Uuid, EntityKind> =
                input.classified_sources.iter().copied().collect();
            let mut sources = input.summary.included_memory_ids.clone();
            sources.push(input.ended.request_memory_id);
            sources.push(ended_memory_id);
            sources.sort_unstable();
            sources.dedup();
            for source in sources {
                let target_kind = if source == ended_memory_id {
                    EntityKind::Fact
                } else {
                    *class_map.get(&source).ok_or_else(|| {
                        StorageError::Internal(format!(
                            "chat provenance memory class not found: {source}"
                        ))
                    })?
                };
                edge_ids.push(
                    append_edge(
                        &mut tx,
                        registry,
                        CORE_DERIVED_FROM_RELATION,
                        &input.owner,
                        EntityKind::Abstraction,
                        input.summary_memory_id,
                        target_kind,
                        source,
                        EdgeAuthorshipKind::ExternalAgent,
                        input.caller_self.into_inner(),
                    )
                    .await?,
                );
            }
        }
    }
    tx.commit().await.map_err(map_err)?;
    Ok(EndChatEmitOutcome {
        ended_memory_id: ended_outcome.memory_id,
        ended_idempotent_replay: ended_outcome.idempotent_replay,
        summary_inserted,
        edge_ids,
    })
}
