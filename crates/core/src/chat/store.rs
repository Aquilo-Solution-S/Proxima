//! `ChatStore` — Postgres data-access contract for the chat tools.
//!
//! Like `ApprovalStore` and `InterventionStore`, `ChatStore` is a focused
//! capability trait declared as a supertrait of [`crate::Storage`]. The
//! chat tools in `proxima-core` reach storage through it; the `PgStorage`
//! impl lives in `proxima-storage-pg::verbs::chat`.
//!
//! Read verbs return the `Loaded*` projections below. Write verbs are
//! composite atomic operations: each opens one transaction so the Fact
//! materialization, the typed sidecar row, and the provenance edges land
//! together. All raw SQL stays out of `proxima-core`.

use async_trait::async_trait;
use time::OffsetDateTime;

use super::payloads::{
    CHAT_SOURCE_ID, ChatCompactionV1, ChatEndRequestedV1, ChatEndedV1, ChatMessageV1, ChatReplyV1,
    ChatStartedV1, ChatSummaryV1, END_REQUESTED_OBJECT_SCHEMA, END_REQUESTED_WHOLE_SCHEMA,
    ENDED_OBJECT_SCHEMA, ENDED_WHOLE_SCHEMA, MESSAGE_OBJECT_SCHEMA, MESSAGE_WHOLE_SCHEMA,
    REPLY_OBJECT_SCHEMA, REPLY_WHOLE_SCHEMA, STARTED_OBJECT_SCHEMA, STARTED_WHOLE_SCHEMA,
};
use crate::approval::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalTargetKind,
    ApprovalVoteVerdict, ApprovalVoterKind,
};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, FlavorRegistryFrozen, MemoryId, Owner,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};

// ---------------------------------------------------------------------------
// Read-side projections
// ---------------------------------------------------------------------------

/// A `core/chat-started-v1` Fact with its memory id.
#[derive(Clone)]
pub struct LoadedStarted {
    pub memory_id: MemoryId,
    pub payload: ChatStartedV1,
}

/// A `core/chat-message-v1` Fact with its memory id.
#[derive(Clone)]
pub struct LoadedMessage {
    pub memory_id: MemoryId,
    pub payload: ChatMessageV1,
}

/// A `core/chat-reply-v1` Fact with its memory id.
#[derive(Clone)]
pub struct LoadedReply {
    pub memory_id: MemoryId,
    pub payload: ChatReplyV1,
}

/// A `core/chat-end-requested-v1` Fact with its memory id.
#[derive(Clone)]
pub struct LoadedEndRequest {
    pub memory_id: MemoryId,
    pub payload: ChatEndRequestedV1,
}

/// A `core/chat-ended-v1` Fact with its memory id.
#[derive(Clone)]
pub struct LoadedEnded {
    pub memory_id: MemoryId,
    pub payload: ChatEndedV1,
}

/// A `core/chat-compaction-v1` Abstraction with its memory id.
#[derive(Clone)]
pub struct LoadedCompaction {
    pub memory_id: MemoryId,
    pub payload: ChatCompactionV1,
}

/// A `core/chat-summary-v1` Abstraction with its memory id.
#[derive(Clone)]
pub struct LoadedSummary {
    pub memory_id: MemoryId,
    pub payload: ChatSummaryV1,
}

/// An `approval_policy_v1` row projected for chat-thread rendering.
#[derive(Clone)]
pub struct LoadedApprovalPolicy {
    pub memory_id: MemoryId,
    pub target_kind: ApprovalTargetKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub title: String,
    pub summary: String,
    pub eligible_voters: Vec<ApprovalEligibleVoter>,
    pub requirements: Vec<ApprovalRequirement>,
    pub idempotency_key: String,
    pub created_at: OffsetDateTime,
}

/// An `approval_vote_v1` row projected for chat-thread rendering.
#[derive(Clone)]
pub struct LoadedApprovalVote {
    pub memory_id: MemoryId,
    pub policy_memory_id: uuid::Uuid,
    pub voter_key: String,
    pub voter_kind: ApprovalVoterKind,
    pub role: Option<String>,
    pub personality_instance_id: Option<uuid::Uuid>,
    pub self_perspective_memory_id: Option<uuid::Uuid>,
    pub master_token_id: Option<uuid::Uuid>,
    pub verdict: ApprovalVoteVerdict,
    pub rationale: String,
    pub idempotency_key: String,
    pub voted_at: OffsetDateTime,
}

/// An `approval_decision_v1` row projected for chat-thread rendering.
#[derive(Clone)]
pub struct LoadedApprovalDecision {
    pub memory_id: MemoryId,
    pub policy_memory_id: uuid::Uuid,
    pub target_kind: ApprovalTargetKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub decision: ApprovalDecision,
    pub reason: String,
    pub counted_votes: Vec<ThreadApprovalCountedVoteRaw>,
    pub idempotency_key: String,
    pub decided_at: OffsetDateTime,
}

/// One counted vote inside an approval decision's `counted_votes_json`.
#[derive(Clone, serde::Deserialize)]
pub struct ThreadApprovalCountedVoteRaw {
    pub vote_memory_id: uuid::Uuid,
    pub voter_key: String,
    pub verdict: ApprovalVoteVerdict,
}

/// An `edges` row projected for chat-thread rendering.
#[derive(Clone)]
pub struct LoadedThreadEdge {
    pub edge_id: EdgeId,
    pub relation: String,
    pub source_kind: EntityKind,
    pub source_memory_id: Option<uuid::Uuid>,
    pub source_goal_id: Option<uuid::Uuid>,
    pub target_kind: EntityKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub authorship_kind: EdgeAuthorshipKind,
    pub created_at: OffsetDateTime,
}

/// The `core/chat-ended-v1` / `core/chat-summary-v1` pair already recorded
/// for an end-chat request — the idempotent-replay short-circuit for
/// `core/end_chat`.
#[derive(Clone, Copy)]
pub struct ExistingChatEnd {
    pub ended_memory_id: MemoryId,
    pub summary_memory_id: MemoryId,
}

// ---------------------------------------------------------------------------
// Write-side inputs and outcomes
// ---------------------------------------------------------------------------

/// Fully-resolved input for `core/start_chat`.
pub struct StartChatInput {
    pub owner: Owner,
    pub started: ChatStartedV1,
    pub message: ChatMessageV1,
    pub edge_authorship: EdgeAuthorshipKind,
    pub caller_self: MemoryId,
}

/// Result of `start_chat_atomic`.
pub struct StartChatEmitOutcome {
    pub started_memory_id: MemoryId,
    pub message_memory_id: MemoryId,
    pub message_edge_id: Option<uuid::Uuid>,
    pub idempotent_replay: bool,
}

/// Fully-resolved input for `core/emit_chat_message`.
pub struct EmitChatMessageInput {
    pub owner: Owner,
    pub message: ChatMessageV1,
    pub edge_authorship: EdgeAuthorshipKind,
    pub caller_self: MemoryId,
}

/// Fully-resolved input for `core/emit_chat_reply`.
pub struct EmitChatReplyInput {
    pub owner: Owner,
    pub reply: ChatReplyV1,
    pub message_memory_id: MemoryId,
    pub edge_authorship: EdgeAuthorshipKind,
    pub caller_self: MemoryId,
}

/// Fully-resolved input for `core/request_end_chat`.
pub struct RequestEndChatInput {
    pub owner: Owner,
    pub request: ChatEndRequestedV1,
    pub edge_authorship: EdgeAuthorshipKind,
    pub caller_self: MemoryId,
}

/// Result of `emit_chat_message_atomic`, `emit_chat_reply_atomic`, and
/// `request_end_chat_atomic` — a single Fact plus its addressing edge.
pub struct ChatFactEmitOutcome {
    pub memory_id: MemoryId,
    pub edge_id: Option<uuid::Uuid>,
    pub idempotent_replay: bool,
}

/// Fully-resolved input for `core/compact_chat_thread`.
///
/// `classified_sources` is `payload.included_memory_ids` paired with each
/// memory's `EntityKind`, in order; Perspective sources are rejected by the
/// tool before this input is built.
pub struct CompactChatThreadInput {
    pub owner: Owner,
    pub model_id: String,
    pub compaction_memory_id: uuid::Uuid,
    pub payload: ChatCompactionV1,
    pub classified_sources: Vec<(uuid::Uuid, EntityKind)>,
    pub caller_self: MemoryId,
}

/// Result of `compact_chat_thread_atomic`.
pub struct CompactChatThreadEmitOutcome {
    pub inserted: bool,
    pub edge_ids: Vec<uuid::Uuid>,
}

/// Fully-resolved input for `core/end_chat`.
///
/// `classified_sources` covers `summary.included_memory_ids` and
/// `ended.request_memory_id` (every provenance source except the
/// chat-ended Fact, whose id is only known after ingest).
pub struct EndChatInput {
    pub owner: Owner,
    pub model_id: String,
    pub summary_memory_id: uuid::Uuid,
    pub ended: ChatEndedV1,
    pub summary: ChatSummaryV1,
    pub classified_sources: Vec<(uuid::Uuid, EntityKind)>,
    pub caller_self: MemoryId,
}

/// Result of `end_chat_atomic`.
pub struct EndChatEmitOutcome {
    pub ended_memory_id: MemoryId,
    pub ended_idempotent_replay: bool,
    pub summary_inserted: bool,
    pub edge_ids: Vec<uuid::Uuid>,
}

// ---------------------------------------------------------------------------
// Pure helper
// ---------------------------------------------------------------------------

fn chat_store_unimplemented(verb: &str) -> StorageError {
    StorageError::Internal(format!("ChatStore::{verb} has no storage backend"))
}

/// Build the [`EventDraft`] for a chat Fact payload — the storage-agnostic
/// half of chat-Fact ingest (CBOR encode, content hash, cited-object and
/// citation-mapping hints). The `PgStorage` write verbs call this, then
/// hand the draft to `ingest_event_in_tx`.
///
/// # Errors
///
/// Returns `StorageError::Internal` if the payload cannot be CBOR-encoded
/// or its `SCHEMA_ID` is not a known chat schema.
pub fn chat_fact_event_draft<F: FactPayload + serde::Serialize>(
    owner: &Owner,
    payload: &F,
) -> Result<EventDraft, StorageError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| StorageError::Internal(format!("serialize chat payload: {err}")))?;
    let content_hash = blake3::hash(&payload_bytes);
    let now = OffsetDateTime::now_utc();
    let (object_schema, whole_schema) = match F::SCHEMA_ID {
        ChatStartedV1::SCHEMA_ID => (STARTED_OBJECT_SCHEMA, STARTED_WHOLE_SCHEMA),
        ChatMessageV1::SCHEMA_ID => (MESSAGE_OBJECT_SCHEMA, MESSAGE_WHOLE_SCHEMA),
        ChatReplyV1::SCHEMA_ID => (REPLY_OBJECT_SCHEMA, REPLY_WHOLE_SCHEMA),
        ChatEndRequestedV1::SCHEMA_ID => (END_REQUESTED_OBJECT_SCHEMA, END_REQUESTED_WHOLE_SCHEMA),
        ChatEndedV1::SCHEMA_ID => (ENDED_OBJECT_SCHEMA, ENDED_WHOLE_SCHEMA),
        other => {
            return Err(StorageError::Internal(format!(
                "unsupported chat payload schema: {other}"
            )));
        }
    };
    Ok(EventDraft {
        source_id: SourceId::new(CHAT_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: owner.clone(),
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
    })
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Postgres data access for the `core/*_chat*` MCP tools.
///
/// Declared as a supertrait of [`crate::Storage`]; the default method
/// bodies (empty reads, `unimplemented` writes) let test fakes and
/// `NoopStorage` satisfy the bound without a Postgres backend.
#[async_trait]
pub trait ChatStore: Send + Sync {
    /// The chat-ended / chat-summary pair already recorded for `request`.
    async fn chat_existing_end_by_request(
        &self,
        _owner: &Owner,
        _request_memory_id: MemoryId,
    ) -> Result<Option<ExistingChatEnd>, StorageError> {
        Ok(None)
    }

    /// All chat-Fact and chat-Abstraction memory ids in `thread_key`.
    async fn chat_summary_source_memory_ids(
        &self,
        _owner: &Owner,
        _thread_key: &str,
    ) -> Result<Vec<uuid::Uuid>, StorageError> {
        Ok(Vec::new())
    }

    /// Count how many of `memory_ids` are visible memories for `owner`.
    async fn chat_count_visible_memories(
        &self,
        _owner: &Owner,
        _memory_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Count how many of `memory_ids` belong to chat thread `thread_key`.
    async fn chat_count_thread_source_memories(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _memory_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// `(memory_id, kind)` for each visible memory in `memory_ids`.
    async fn chat_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[uuid::Uuid],
    ) -> Result<Vec<(uuid::Uuid, String)>, StorageError> {
        Ok(Vec::new())
    }

    /// Count how many of `goal_ids` are visible goals for `owner`.
    async fn chat_count_visible_goals(
        &self,
        _owner: &Owner,
        _goal_ids: &[uuid::Uuid],
    ) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Load a `core/chat-message-v1` Fact payload by memory id.
    async fn chat_load_message(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
    ) -> Result<Option<ChatMessageV1>, StorageError> {
        Ok(None)
    }

    /// The thread key of a chat-message or chat-reply Fact.
    async fn chat_parent_thread_key(
        &self,
        _owner: &Owner,
        _parent_memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    /// Load a `core/chat-end-requested-v1` Fact payload by memory id.
    async fn chat_load_end_request(
        &self,
        _owner: &Owner,
        _request_memory_id: MemoryId,
    ) -> Result<Option<ChatEndRequestedV1>, StorageError> {
        Ok(None)
    }

    /// `target_memory_id` of a `fact`-kind approval policy.
    ///
    /// Outer `None` = no such policy; inner `None` = the policy exists but
    /// has a null `target_memory_id`.
    async fn chat_policy_fact_target(
        &self,
        _owner: &Owner,
        _policy_memory_id: uuid::Uuid,
    ) -> Result<Option<Option<uuid::Uuid>>, StorageError> {
        Ok(None)
    }

    /// The policy a vote or decision memory belongs to.
    async fn chat_policy_id_for_vote_or_decision(
        &self,
        _owner: &Owner,
        _memory_id: uuid::Uuid,
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        Ok(None)
    }

    /// The thread key of any chat Fact/Abstraction memory.
    async fn chat_thread_key_for_memory(
        &self,
        _owner: &Owner,
        _memory_id: uuid::Uuid,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    /// The earliest `core/chat-started-v1` Fact for `thread_key`.
    async fn chat_thread_started(
        &self,
        _owner: &Owner,
        _thread_key: &str,
    ) -> Result<Option<LoadedStarted>, StorageError> {
        Ok(None)
    }

    /// Chat-message Facts in `thread_key`, oldest first.
    async fn chat_thread_messages(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _limit: i64,
    ) -> Result<Vec<LoadedMessage>, StorageError> {
        Ok(Vec::new())
    }

    /// Chat-reply Facts in `thread_key`, oldest first.
    async fn chat_thread_replies(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _limit: i64,
    ) -> Result<Vec<LoadedReply>, StorageError> {
        Ok(Vec::new())
    }

    /// Chat-end-requested Facts in `thread_key`, oldest first.
    async fn chat_thread_end_requests(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _limit: i64,
    ) -> Result<Vec<LoadedEndRequest>, StorageError> {
        Ok(Vec::new())
    }

    /// The latest `core/chat-ended-v1` Fact for `thread_key`.
    async fn chat_thread_ended(
        &self,
        _owner: &Owner,
        _thread_key: &str,
    ) -> Result<Option<LoadedEnded>, StorageError> {
        Ok(None)
    }

    /// Chat-compaction Abstractions in `thread_key`, oldest first.
    async fn chat_thread_compactions(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _limit: i64,
    ) -> Result<Vec<LoadedCompaction>, StorageError> {
        Ok(Vec::new())
    }

    /// Chat-summary Abstractions in `thread_key`, oldest first.
    async fn chat_thread_summaries(
        &self,
        _owner: &Owner,
        _thread_key: &str,
        _limit: i64,
    ) -> Result<Vec<LoadedSummary>, StorageError> {
        Ok(Vec::new())
    }

    /// `fact`-target approval policies whose target is in `target_memory_ids`.
    async fn chat_thread_approval_policies(
        &self,
        _owner: &Owner,
        _target_memory_ids: &[uuid::Uuid],
        _limit: i64,
    ) -> Result<Vec<LoadedApprovalPolicy>, StorageError> {
        Ok(Vec::new())
    }

    /// Approval votes cast on any policy in `policy_memory_ids`.
    async fn chat_thread_approval_votes(
        &self,
        _owner: &Owner,
        _policy_memory_ids: &[uuid::Uuid],
        _limit: i64,
    ) -> Result<Vec<LoadedApprovalVote>, StorageError> {
        Ok(Vec::new())
    }

    /// Approval decisions made on any policy in `policy_memory_ids`.
    async fn chat_thread_approval_decisions(
        &self,
        _owner: &Owner,
        _policy_memory_ids: &[uuid::Uuid],
        _limit: i64,
    ) -> Result<Vec<LoadedApprovalDecision>, StorageError> {
        Ok(Vec::new())
    }

    /// Chat-relevant edges touching any memory in `thread_memory_ids`.
    async fn chat_thread_edges(
        &self,
        _owner: &Owner,
        _thread_memory_ids: &[uuid::Uuid],
        _limit: i64,
    ) -> Result<Vec<LoadedThreadEdge>, StorageError> {
        Ok(Vec::new())
    }

    /// Ingest the chat-started and first chat-message Facts, their typed
    /// sidecars, and the addressing edge in one transaction.
    async fn start_chat_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &StartChatInput,
    ) -> Result<StartChatEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("start_chat_atomic"))
    }

    /// Ingest a chat-message Fact, its sidecar, and the addressing edge.
    async fn emit_chat_message_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitChatMessageInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("emit_chat_message_atomic"))
    }

    /// Ingest a chat-reply Fact, its sidecar, and the reply edge.
    async fn emit_chat_reply_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitChatReplyInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("emit_chat_reply_atomic"))
    }

    /// Ingest a chat-end-requested Fact, its sidecar, and the addressing edge.
    async fn request_end_chat_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &RequestEndChatInput,
    ) -> Result<ChatFactEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("request_end_chat_atomic"))
    }

    /// Materialize a chat-compaction Abstraction and its derived-from edges.
    async fn compact_chat_thread_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &CompactChatThreadInput,
    ) -> Result<CompactChatThreadEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("compact_chat_thread_atomic"))
    }

    /// Ingest the chat-ended Fact, materialize the chat-summary Abstraction,
    /// and append the summary's derived-from edges in one transaction.
    async fn end_chat_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EndChatInput,
    ) -> Result<EndChatEmitOutcome, StorageError> {
        Err(chat_store_unimplemented("end_chat_atomic"))
    }
}
