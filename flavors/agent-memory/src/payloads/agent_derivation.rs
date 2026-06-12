use proxima_core::{AbstractionPayload, PerspectivePayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentDerivationV1 {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub idempotency_key: Option<String>,
    pub source_memory_ids: Vec<uuid::Uuid>,
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
}

impl AbstractionPayload for AgentDerivationV1 {
    const SCHEMA_ID: &'static str = "proxima-agent-memory/agent-derivation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_agent_memory.agent_derivation_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AgentDerivationV1 schema serializes"),
        )
    }
}

impl PerspectivePayload for AgentDerivationV1 {
    const SCHEMA_ID: &'static str = "proxima-agent-memory/agent-derivation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_agent_memory.agent_derivation_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AgentDerivationV1 schema serializes"),
        )
    }
}
