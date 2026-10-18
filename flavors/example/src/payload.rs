//! The example flavor's two payloads: a stateful note Fact and a digest
//! Abstraction, both scoped to a notebook. Test and tutorial fixtures.

use proxima_core::{AbstractionPayload, FactPayload, ScopeKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The notebook scope every example payload names.
pub const NOTEBOOK_SCOPE: ScopeKind = ScopeKind::new("notebook");

/// Schema id of [`NoteV1`].
pub const NOTE_SCHEMA_ID: &str = "proxima-example/note-v1";
/// Schema id of [`DigestV1`].
pub const DIGEST_SCHEMA_ID: &str = "proxima-example/digest-v1";

/// One note in a notebook. Stateful: a newer note with the same
/// `(notebook_id, key)` supersedes the older one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NoteV1 {
    /// The notebook the note belongs to (its scope).
    pub notebook_id: Uuid,
    /// The note's key inside the notebook.
    pub key: String,
    /// The note's text.
    pub body: String,
}

impl FactPayload for NoteV1 {
    const SCHEMA_ID: &'static str = NOTE_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;
    const SCOPE_KIND: Option<ScopeKind> = Some(NOTEBOOK_SCOPE);

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = self.notebook_id.as_bytes().to_vec();
        key.extend_from_slice(self.key.as_bytes());
        key.push(0);
        key.extend_from_slice(self.body.as_bytes());
        key
    }

    fn render(&self) -> String {
        self.body.clone()
    }

    fn scope_id(&self) -> Option<Uuid> {
        Some(self.notebook_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_example.note_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["notebook_id", "key"]
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("NoteV1 schema serializes"))
    }
}

/// A digest derived from notes of one notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DigestV1 {
    /// The notebook the digest summarizes (its scope).
    pub notebook_id: Uuid,
    /// The digest text.
    pub summary: String,
}

impl AbstractionPayload for DigestV1 {
    const SCHEMA_ID: &'static str = DIGEST_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;
    const SCOPE_KIND: Option<ScopeKind> = Some(NOTEBOOK_SCOPE);

    fn scope_id(&self) -> Option<Uuid> {
        Some(self.notebook_id)
    }

    fn sidecar_table() -> &'static str {
        "proxima_example.digest_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("DigestV1 schema serializes"))
    }
}

proxima::flavor::pg_sidecar! {
    payload: NoteV1,
    row: NoteRow,
    kinds: [Fact],
    table: "proxima_example.note_v1",
    key: t,
    fields: {
        notebook_id => notebook_id: (uuid),
        key => key: (text),
        body => body: (text),
    },
}

proxima::flavor::pg_sidecar! {
    payload: DigestV1,
    row: DigestRow,
    kinds: [Abstraction],
    table: "proxima_example.digest_v1",
    key: t,
    fields: {
        notebook_id => notebook_id: (uuid),
        summary => summary: (text),
    },
}
