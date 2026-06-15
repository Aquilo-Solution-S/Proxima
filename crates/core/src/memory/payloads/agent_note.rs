use crate::{FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField};
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

    fn render(&self) -> String {
        format!("{}\n\n{}", self.title, self.body)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.agent_note_v1")
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
        })
    }
}
