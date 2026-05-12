//! Wake-trace schemas. See the harness design spec
//! §"Observability: three layers".

pub mod emit;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{CitationMappingPayload, CitedObjectPayload, FactPayload, SchemaId, proxima_schema_id};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTracePayload {
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub model_target_ref: String,
    pub model_id: String,
    #[schemars(with = "String")]
    pub started_at: OffsetDateTime,
    #[schemars(with = "String")]
    pub finished_at: OffsetDateTime,
    pub outcome_kind: String,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub finish_reason: Option<String>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    pub jsonl_truncated: bool,
}

impl FactPayload for WakeTracePayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!(
            "Wake {} {} ({} rounds)",
            self.invocation_id, self.outcome_kind, self.rounds_used
        )
    }

    fn sidecar_table() -> &'static str {
        "proxima_core.wake_trace_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTraceJsonlPayload {
    pub content_hash: [u8; 32],
    pub byte_len: u64,
    pub line_count: u64,
    pub truncated: bool,
}

impl CitedObjectPayload for WakeTraceJsonlPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-jsonl-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_wake_trace_jsonl_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WakeTraceCitationPayload {
    pub byte_range_start: Option<u64>,
    pub byte_range_end: Option<u64>,
}

impl CitationMappingPayload for WakeTraceCitationPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-citation-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.citation_wake_trace_v1"
    }

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(proxima_schema_id!("wake-trace-jsonl-v1").to_string())
    }
}
