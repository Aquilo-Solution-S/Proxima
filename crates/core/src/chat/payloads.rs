use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AbstractionPayload, FactPayload, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField,
};

pub const CHAT_STARTED_SCHEMA_ID: &str = "core/chat-started-v1";
pub const CHAT_MESSAGE_SCHEMA_ID: &str = "core/chat-message-v1";
pub const CHAT_REPLY_SCHEMA_ID: &str = "core/chat-reply-v1";
pub const CHAT_END_REQUESTED_SCHEMA_ID: &str = "core/chat-end-requested-v1";
pub const CHAT_ENDED_SCHEMA_ID: &str = "core/chat-ended-v1";
pub const CHAT_COMPACTION_SCHEMA_ID: &str = "core/chat-compaction-v1";
pub const CHAT_SUMMARY_SCHEMA_ID: &str = "core/chat-summary-v1";

pub(super) const CHAT_SOURCE_ID: &str = "core/chat";
pub(super) const STARTED_OBJECT_SCHEMA: &str = "core/chat-started-object-v1";
pub(super) const STARTED_WHOLE_SCHEMA: &str = "core/chat-started-whole-v1";
pub(super) const MESSAGE_OBJECT_SCHEMA: &str = "core/chat-message-object-v1";
pub(super) const MESSAGE_WHOLE_SCHEMA: &str = "core/chat-message-whole-v1";
pub(super) const REPLY_OBJECT_SCHEMA: &str = "core/chat-reply-object-v1";
pub(super) const REPLY_WHOLE_SCHEMA: &str = "core/chat-reply-whole-v1";
pub(super) const END_REQUESTED_OBJECT_SCHEMA: &str = "core/chat-end-requested-object-v1";
pub(super) const END_REQUESTED_WHOLE_SCHEMA: &str = "core/chat-end-requested-whole-v1";
pub(super) const ENDED_OBJECT_SCHEMA: &str = "core/chat-ended-object-v1";
pub(super) const ENDED_WHOLE_SCHEMA: &str = "core/chat-ended-whole-v1";
pub(super) const CHAT_COMPACTION_DERIVED_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x83, 0xde, 0x1b, 0xaf, 0x92, 0x35, 0x47, 0x65, 0xa0, 0xe6, 0xd3, 0x14, 0x8a, 0x13, 0x68, 0x4f,
]);
pub(super) const CHAT_SUMMARY_DERIVED_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0xcf, 0xc2, 0x1e, 0xf4, 0x3b, 0xa5, 0x41, 0x7b, 0x9b, 0x90, 0x61, 0x4f, 0x72, 0xe3, 0x92, 0x11,
]);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ChatStartedV1 {
    pub thread_key: String,
    pub started_by_self_perspective_memory_id: uuid::Uuid,
    pub target_personality_instance_id: uuid::Uuid,
    pub target_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub title: Option<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

impl FactPayload for ChatStartedV1 {
    const SCHEMA_ID: &'static str = CHAT_STARTED_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_started_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
        })
    }

    fn render(&self) -> String {
        match self.title.as_deref() {
            Some(title) => format!("Chat started: {title}"),
            None => format!("Chat started: {}", self.thread_key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ChatMessageV1 {
    pub thread_key: String,
    pub message: String,
    pub target_personality_instance_id: uuid::Uuid,
    pub target_self_perspective_memory_id: uuid::Uuid,
    pub sent_by_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub parent_memory_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub context_memory_ids: Vec<uuid::Uuid>,
    #[serde(default)]
    pub context_goal_ids: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}

impl FactPayload for ChatMessageV1 {
    const SCHEMA_ID: &'static str = CHAT_MESSAGE_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_message_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "message",
                kind: SearchProjectionColumnKind::Text,
            }],
        })
    }

    fn render(&self) -> String {
        format!("Chat message: {}", self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ChatReplyV1 {
    pub message_memory_id: uuid::Uuid,
    pub thread_key: String,
    pub reply: String,
    pub replied_by_personality_instance_id: uuid::Uuid,
    pub replied_by_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub context_memory_ids_used: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub replied_at: OffsetDateTime,
}

impl FactPayload for ChatReplyV1 {
    const SCHEMA_ID: &'static str = CHAT_REPLY_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_reply_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "reply",
                kind: SearchProjectionColumnKind::Text,
            }],
        })
    }

    fn render(&self) -> String {
        "Chat reply".into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ChatEndRequestedV1 {
    pub thread_key: String,
    pub target_personality_instance_id: uuid::Uuid,
    pub target_self_perspective_memory_id: uuid::Uuid,
    pub requested_by_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub reason: Option<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

impl FactPayload for ChatEndRequestedV1 {
    const SCHEMA_ID: &'static str = CHAT_END_REQUESTED_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_end_requested_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "reason",
                kind: SearchProjectionColumnKind::Text,
            }],
        })
    }

    fn render(&self) -> String {
        format!("Chat end requested: {}", self.thread_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ChatEndedV1 {
    pub thread_key: String,
    pub request_memory_id: uuid::Uuid,
    pub ended_by_personality_instance_id: uuid::Uuid,
    pub ended_by_self_perspective_memory_id: uuid::Uuid,
    pub summary_memory_id: uuid::Uuid,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub ended_at: OffsetDateTime,
}

impl FactPayload for ChatEndedV1 {
    const SCHEMA_ID: &'static str = CHAT_ENDED_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_ended_v1"
    }

    fn render(&self) -> String {
        format!("Chat ended: {}", self.thread_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ChatCompactionV1 {
    pub thread_key: String,
    pub compacted_by_personality_instance_id: uuid::Uuid,
    pub compacted_by_self_perspective_memory_id: uuid::Uuid,
    pub summary: String,
    #[serde(default)]
    pub included_memory_ids: Vec<uuid::Uuid>,
    #[serde(default)]
    pub context_memory_ids_used: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[schemars(with = "String")]
    #[serde(with = "time::serde::rfc3339")]
    pub compacted_at: OffsetDateTime,
}

impl AbstractionPayload for ChatCompactionV1 {
    const SCHEMA_ID: &'static str = CHAT_COMPACTION_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_compaction_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("ChatCompactionV1 schema serializes"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ChatSummaryV1 {
    pub thread_key: String,
    pub request_memory_id: uuid::Uuid,
    pub ended_memory_id: uuid::Uuid,
    pub summarized_by_personality_instance_id: uuid::Uuid,
    pub summarized_by_self_perspective_memory_id: uuid::Uuid,
    pub summary: String,
    #[serde(default)]
    pub included_memory_ids: Vec<uuid::Uuid>,
    #[serde(default)]
    pub context_memory_ids_used: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[schemars(with = "String")]
    #[serde(with = "time::serde::rfc3339")]
    pub summarized_at: OffsetDateTime,
}

impl AbstractionPayload for ChatSummaryV1 {
    const SCHEMA_ID: &'static str = CHAT_SUMMARY_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.chat_summary_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("ChatSummaryV1 schema serializes"),
        )
    }
}
