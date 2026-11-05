use crate::{
    DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay, PayloadKeyBuilder,
    PayloadProjection,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNoteV1 {
    pub note_id: uuid::Uuid,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub idempotency_key: Option<String>,
}

impl FactPayload for AgentNoteV1 {
    const SCHEMA_ID: &'static str = "core/agent-note-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("note_id", self.note_id);
        key.field_str("title", &self.title);
        key.field_str("body", &self.body);
        key.field_str_list("tags", &self.tags);
        key.finish()
    }

    fn render(&self) -> String {
        format!("{}\n\n{}", self.title, self.body)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_core.agent_note_v1")
    }
}

impl PayloadProjection for AgentNoteV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] =
        &[KeyField::uuid("note_id", |value| value.note_id)];

    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            title: Some(self.title.clone()),
            body: Some(self.body.clone()),
            tags: self.tags.clone(),
            ..PayloadDisplay::default()
        }
    }

    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("note_id", &self.note_id)?
            .field("title", &self.title)?
            .field("body", &self.body)?
            .field("tags", &self.tags)?
            .field("idempotency_key", &self.idempotency_key)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn durable_matches_current_wire_encoding() {
        let value = AgentNoteV1 {
            note_id: uuid::Uuid::nil(),
            title: "title".into(),
            body: "body".into(),
            tags: vec!["tag".into()],
            idempotency_key: Some("request".into()),
        };
        let durable = value.durable().expect("durable").canonical_bytes();
        let wire = serde_json::to_value(&value).expect("wire");
        assert_eq!(durable, crate::canonical_json_bytes(&wire));
    }
}
