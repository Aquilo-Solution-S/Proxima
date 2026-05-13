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
    // RFC 3339 explicit so JSON round-trips through Postgres
    // `row_to_json(sidecar)` (which emits e.g. `2026-05-13T12:55:00+00:00`).
    // Without this, the default deserializer (enabled by time's
    // `serde-human-readable` feature) expects time's own default format
    // (`YYYY-MM-DD HH:MM:SS +HH:MM:SS`) and rejects the 'T' separator with
    // "a character literal was not valid", failing the whole Query verb.
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
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

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for the Atlas "CBOR payload encode failed: a character
    // literal was not valid" loop: storage projects `wake_trace_v1` via
    // `row_to_json(sidecar)::text`, producing RFC-3339-style timestamp strings
    // like `2026-05-13T12:55:00.728+00:00`. The Query verb then runs
    // `serde_json::from_value::<WakeTracePayload>(...)`. If the time fields
    // lack the `time::serde::rfc3339` attribute, time's default
    // human-readable deserializer rejects the 'T' separator and the entire
    // Query batch fails — which sent the frontend hydration loop into a
    // 20Hz retry storm.
    #[test]
    fn deserializes_pg_row_to_json_timestamps() {
        let pg_projection = serde_json::json!({
            "invocation_id": "019e2167-a375-7fe0-8379-7f3424a2dae4",
            "wake_entry_id": "019e1376-efc9-75e0-bd16-8a633d987216",
            "personality_instance_id": "019e1375-7596-7d43-ab37-6fc2e9bcce70",
            "model_target_ref": "GPT-5.5",
            "model_id": "gpt-5.5",
            "started_at": "2026-05-13T12:55:00.72832+00:00",
            "finished_at": "2026-05-13T12:55:00.728334+00:00",
            "outcome_kind": "failed",
            "failure_reason": "provider_not_yet_supported:ChatGPTCodex",
            "rounds_used": 0,
            "finish_reason": "stop",
            "total_prompt_tokens": null,
            "total_completion_tokens": null,
            "tool_call_count": 0,
            "jsonl_truncated": false,
        });
        serde_json::from_value::<WakeTracePayload>(pg_projection)
            .expect("PG row_to_json projection must round-trip into WakeTracePayload");
    }
}
