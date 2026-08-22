use crate::{FactPayload, PayloadKeyBuilder};
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
        Some("proxima_core.write_act_v1")
    }
}
