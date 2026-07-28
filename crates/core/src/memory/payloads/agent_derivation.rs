use crate::{
    AbstractionPayload, PerspectivePayload, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField,
};
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
    const SCHEMA_ID: &'static str = "core/agent-derivation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.agent_derivation_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(agent_derivation_search_projection())
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AgentDerivationV1 schema serializes"),
        )
    }
}

impl PerspectivePayload for AgentDerivationV1 {
    const SCHEMA_ID: &'static str = "core/agent-derivation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.agent_derivation_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(agent_derivation_search_projection())
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AgentDerivationV1 schema serializes"),
        )
    }
}

fn agent_derivation_search_projection() -> SearchProjection {
    SearchProjection {
        fields: &[
            SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            },
            SearchProjectionField {
                column: "body",
                kind: SearchProjectionColumnKind::Text,
            },
            SearchProjectionField {
                column: "tags",
                kind: SearchProjectionColumnKind::TextArray,
            },
        ],
        tag_column: Some("tags".to_string()),
        tsv_column: Some("search_tsv"),
        language_column: Some("lexical_language"),
    }
}
