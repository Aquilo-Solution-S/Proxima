//! Payload type for the `core/personality_config_changed_v1` Fact
//! memory emitted alongside every MCP-CRUD mutation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FactPayload;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersonalityConfigChangedVerb {
    Instantiate,
    Tombstone,
    SetWakeEntries,
    AddWakeEntry,
    UpdateWakeEntry,
    RemoveWakeEntry,
    RegisterInferenceTarget,
    RemoveInferenceTarget,
    BindInferenceTier,
    SetReadScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PersonalityConfigChangedSubject {
    Personality(uuid::Uuid),
    WakeEntry(uuid::Uuid),
    InferenceTarget(String),
    TierBinding(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PersonalityConfigChangedCaller {
    WakePersonality { personality_instance_id: uuid::Uuid },
    MasterToken { personality_instance_id: uuid::Uuid },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersonalityConfigChangedV1 {
    pub verb: PersonalityConfigChangedVerb,
    /// Opaque snapshot of relevant prior state. `None` on create-style verbs.
    pub before: Option<serde_json::Value>,
    /// Opaque snapshot of relevant new state. `None` on tombstone-style verbs.
    pub after: Option<serde_json::Value>,
    pub subject: PersonalityConfigChangedSubject,
    pub caller: PersonalityConfigChangedCaller,
}

impl FactPayload for PersonalityConfigChangedV1 {
    const SCHEMA_ID: &'static str = "core/personality_config_changed_v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("{:?} {:?}", self.verb, self.subject)
    }

    fn sidecar_table() -> &'static str {
        "proxima_core.personality_config_changed_v1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_through_json() {
        let payload = PersonalityConfigChangedV1 {
            verb: PersonalityConfigChangedVerb::Instantiate,
            subject: PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            before: None,
            after: Some(serde_json::json!({ "display_name": "Engineer" })),
            caller: PersonalityConfigChangedCaller::MasterToken {
                personality_instance_id: uuid::Uuid::now_v7(),
            },
        };
        let value = serde_json::to_value(&payload).expect("serialize");
        let back: PersonalityConfigChangedV1 = serde_json::from_value(value).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn schema_id_is_stable() {
        assert_eq!(
            PersonalityConfigChangedV1::SCHEMA_ID,
            "core/personality_config_changed_v1"
        );
        assert_eq!(PersonalityConfigChangedV1::SCHEMA_VERSION, 1);
    }
}
