use crate::{
    FactPayload, PayloadKeyBuilder, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField,
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
        Some("proxima_core.agent_note_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["note_id"]
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
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
        })
    }
}
