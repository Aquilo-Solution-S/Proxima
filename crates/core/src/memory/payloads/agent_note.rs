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
        let Self {
            note_id,
            title,
            body,
            tags,
            idempotency_key,
        } = self;
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("note_id", note_id)?
            .field("title", title)?
            .field("body", body)?
            .field("tags", tags)?
            .field("idempotency_key", idempotency_key)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn durable_matches_the_explicit_contract() {
        let value = AgentNoteV1 {
            note_id: uuid::Uuid::nil(),
            title: "title".into(),
            body: "body".into(),
            tags: vec!["tag".into()],
            idempotency_key: Some("request".into()),
        };
        let expected = serde_json::json!({
            "body": "body",
            "idempotency_key": "request",
            "note_id": uuid::Uuid::nil(),
            "tags": ["tag"],
            "title": "title",
        });
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&expected)
        );
    }
}
