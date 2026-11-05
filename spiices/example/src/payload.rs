//! The example spiice's payloads: a stateful note Fact, a digest
//! Abstraction, a stamp Fact that follows its memory on transfer, and a
//! sealed stamp Fact that never transfers.

use proxiima::spiice::{
    AbstractionPayload, DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay,
    PayloadKeyBuilder, PayloadProjection, ScopeKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The notebook scope every example payload names.
pub const NOTEBOOK_SCOPE: ScopeKind = ScopeKind::new("notebook");

/// Schema id of [`NoteV1`].
pub const NOTE_SCHEMA_ID: &str = "proxiima-example/note-v1";
/// Schema id of [`DigestV1`].
pub const DIGEST_SCHEMA_ID: &str = "proxiima-example/digest-v1";
/// Schema id of [`StampV1`].
pub const STAMP_SCHEMA_ID: &str = "proxiima-example/stamp-v1";
/// Schema id of [`SealedStampV1`].
pub const SEALED_STAMP_SCHEMA_ID: &str = "proxiima-example/sealed-stamp-v1";

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
    /// When the note was written: the payload's own declared clock, which
    /// retention can age a series by instead of its admission time.
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub written_at: time::OffsetDateTime,
}

impl FactPayload for NoteV1 {
    const SCHEMA_ID: &'static str = NOTE_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;
    const SCOPE_KIND: Option<ScopeKind> = Some(NOTEBOOK_SCOPE);

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("notebook_id", self.notebook_id);
        key.field_str("key", &self.key);
        key.field_str("body", &self.body);
        key.finish()
    }

    fn render(&self) -> String {
        self.body.clone()
    }

    fn scope_id(&self) -> Option<Uuid> {
        Some(self.notebook_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_example.note_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("NoteV1 schema serializes"))
    }
}

impl PayloadProjection for NoteV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[
        KeyField::uuid("notebook_id", |value| value.notebook_id),
        KeyField::text("key", |value| &value.key),
    ];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            body: Some(self.body.clone()),
            sketch: Some(self.body.clone()),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("notebook_id", &self.notebook_id)?
            .field("key", &self.key)?
            .field("body", &self.body)?
            .field(
                "written_at",
                &self
                    .written_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| error.to_string())?,
            )?;
        Ok(fields.finish())
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
        "proxiima_example.digest_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("DigestV1 schema serializes"))
    }
}

impl PayloadProjection for DigestV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            body: Some(self.summary.clone()),
            sketch: Some(self.summary.clone()),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("notebook_id", &self.notebook_id)?
            .field("summary", &self.summary)?;
        Ok(fields.finish())
    }
}

proxiima::spiice::pg_sidecar! {
    payload: NoteV1,
    row: NoteRow,
    kinds: [Fact],
    table: "proxiima_example.note_v1",
    key: t,
    fields: {
        notebook_id => notebook_id: (uuid),
        key => key: (text),
        body => body: (text),
        written_at => written_at: (timestamptz),
    },
}

proxiima::spiice::pg_sidecar! {
    payload: DigestV1,
    row: DigestRow,
    kinds: [Abstraction],
    table: "proxiima_example.digest_v1",
    key: t,
    fields: {
        notebook_id => notebook_id: (uuid),
        summary => summary: (text),
    },
}

/// A stamp on a memory that carries its own `owner_id` and follows the
/// memory on transfer (`owner: follows`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StampV1 {
    /// Opaque mark text.
    pub mark: String,
}

impl FactPayload for StampV1 {
    const SCHEMA_ID: &'static str = STAMP_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("mark", &self.mark);
        key.finish()
    }

    fn render(&self) -> String {
        self.mark.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_example.stamp_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("StampV1 schema serializes"))
    }
}

impl PayloadProjection for StampV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            body: Some(self.mark.clone()),
            sketch: Some(self.mark.clone()),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields.field("mark", &self.mark)?;
        Ok(fields.finish())
    }
}

proxiima::spiice::pg_sidecar! {
    payload: StampV1,
    row: StampRow,
    kinds: [Fact],
    table: "proxiima_example.stamp_v1",
    key: t,
    owner: follows,
    fields: {
        mark => mark: (text),
    },
}

/// A stamp on a memory that stays with its owner: it shares the stamp
/// sidecar, whose rows would follow a transfer, and its schema declares
/// `TransferRule::NotTransferable`, so a transfer of it is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SealedStampV1 {
    /// Opaque mark text.
    pub mark: String,
}

impl FactPayload for SealedStampV1 {
    const SCHEMA_ID: &'static str = SEALED_STAMP_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("mark", &self.mark);
        key.finish()
    }

    fn render(&self) -> String {
        self.mark.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_example.stamp_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("SealedStampV1 schema serializes"),
        )
    }
}

impl PayloadProjection for SealedStampV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            body: Some(self.mark.clone()),
            sketch: Some(self.mark.clone()),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields.field("mark", &self.mark)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn durable_examples_match_current_wire_encoding() {
        let note = NoteV1 {
            notebook_id: Uuid::nil(),
            key: "key".into(),
            body: "body".into(),
            written_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let digest = DigestV1 {
            notebook_id: Uuid::nil(),
            summary: "summary".into(),
        };
        let stamp = StampV1 {
            mark: "mark".into(),
        };
        let sealed = SealedStampV1 {
            mark: "sealed".into(),
        };
        for (durable, wire) in [
            (
                note.durable().expect("note durable"),
                serde_json::to_value(&note).expect("note wire"),
            ),
            (
                digest.durable().expect("digest durable"),
                serde_json::to_value(&digest).expect("digest wire"),
            ),
            (
                stamp.durable().expect("stamp durable"),
                serde_json::to_value(&stamp).expect("stamp wire"),
            ),
            (
                sealed.durable().expect("sealed durable"),
                serde_json::to_value(&sealed).expect("sealed wire"),
            ),
        ] {
            assert_eq!(serde_json::to_value(&durable).expect("durable json"), wire);
        }
    }
}

proxiima::spiice::pg_sidecar! {
    payload: SealedStampV1,
    row: SealedStampRow,
    kinds: [Fact],
    table: "proxiima_example.stamp_v1",
    key: t,
    owner: follows,
    fields: {
        mark => mark: (text),
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: &str, body: &str) -> NoteV1 {
        NoteV1 {
            notebook_id: Uuid::from_u128(1),
            key: key.to_owned(),
            body: body.to_owned(),
            written_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Notes whose key and body only differ in where a NUL byte sits are different notes.
    #[test]
    fn notes_that_move_a_nul_between_key_and_body_have_different_receipt_keys() {
        assert_ne!(
            note("a\0b", "x").receipt_key(),
            note("a", "b\0x").receipt_key()
        );
        assert_eq!(note("a", "x").receipt_key(), note("a", "x").receipt_key());
    }
}
