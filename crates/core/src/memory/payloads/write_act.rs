use crate::{
    DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay, PayloadKeyBuilder,
    PayloadProjection,
};
use serde::{Deserialize, Serialize};

/// Protocol write-act Fact: one episode token. Members pin this `t`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteActV1 {
    pub episode_id: uuid::Uuid,
}

impl FactPayload for WriteActV1 {
    const SCHEMA_ID: &'static str = "core/write-act-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("episode_id", self.episode_id);
        key.finish()
    }

    fn render(&self) -> String {
        format!("write-act {}", self.episode_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_core.write_act_v1")
    }
}

impl PayloadProjection for WriteActV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            sketch: Some(format!("write-act {}", self.episode_id)),
            ..PayloadDisplay::default()
        }
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields.field("episode_id", &self.episode_id)?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn durable_matches_current_wire_encoding() {
        let value = WriteActV1 {
            episode_id: uuid::Uuid::nil(),
        };
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&serde_json::to_value(&value).expect("wire"))
        );
    }
}
