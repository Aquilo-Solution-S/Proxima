use crate::{
    AbstractionPayload, DurablePayloadBuilder, KeyField, PayloadDisplay, PayloadProjection,
    PerspectivePayload,
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
        "proxiima_core.agent_derivation_v1"
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
        "proxiima_core.agent_derivation_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AgentDerivationV1 schema serializes"),
        )
    }
}

impl PayloadProjection for AgentDerivationV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            title: Some(self.title.clone()),
            body: Some(self.body.clone()),
            sketch: Some(self.title.clone()),
            tags: self.tags.clone(),
        }
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("title", &self.title)?
            .field("body", &self.body)?
            .field("tags", &self.tags)?
            .field("idempotency_key", &self.idempotency_key)?
            .field("source_memory_ids", &self.source_memory_ids)?
            .field("model_id", &self.model_id)?
            .field("client_name", &self.client_name)?
            .field("client_version", &self.client_version)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn typed_projection_preserves_display_and_v1_encoding() {
        let value = AgentDerivationV1 {
            title: "title".into(),
            body: "body".into(),
            tags: vec!["tag".into()],
            idempotency_key: Some("request".into()),
            source_memory_ids: vec![uuid::Uuid::nil()],
            model_id: "model".into(),
            client_name: "client".into(),
            client_version: "1".into(),
        };
        assert_eq!(value.display().sketch.as_deref(), Some("title"));
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&serde_json::to_value(&value).expect("wire"))
        );
    }
}
