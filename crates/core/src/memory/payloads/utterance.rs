use crate::FactPayload;
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

    fn render(&self) -> String {
        format!(
            "[{}] conversation:{}\n{}",
            self.speaker.as_str(),
            self.conversation_id,
            self.text
        )
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.utterance_v1")
    }
}
