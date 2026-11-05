use crate::{
    DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay, PayloadKeyBuilder,
    PayloadProjection,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    User,
    Agent,
}

impl Speaker {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UtteranceV1 {
    pub speaker: Speaker,
    pub conversation_id: String,
    pub text: String,
}

impl FactPayload for UtteranceV1 {
    const SCHEMA_ID: &'static str = "core/utterance-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("speaker", self.speaker.as_str());
        key.field_str("conversation_id", &self.conversation_id);
        key.field_str("text", &self.text);
        key.finish()
    }

    fn render(&self) -> String {
        format!(
            "[{}] conversation:{}\n{}",
            self.speaker.as_str(),
            self.conversation_id,
            self.text
        )
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_core.utterance_v1")
    }
}

impl PayloadProjection for UtteranceV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            title: Some(self.conversation_id.clone()),
            body: Some(self.text.clone()),
            sketch: Some(self.text.clone()),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("speaker", &self.speaker.as_str())?
            .field("conversation_id", &self.conversation_id)?
            .field("text", &self.text)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn durable_matches_current_wire_encoding() {
        let value = UtteranceV1 {
            speaker: Speaker::Agent,
            conversation_id: "conversation".into(),
            text: "text".into(),
        };
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&serde_json::to_value(&value).expect("wire"))
        );
    }
}
