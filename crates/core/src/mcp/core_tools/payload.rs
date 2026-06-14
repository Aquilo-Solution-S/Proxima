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
    SetReadScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PersonalityConfigChangedSubject {
    Personality(uuid::Uuid),
    WakeEntry(uuid::Uuid),
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
    /// Typed snapshot of relevant prior state. `None` on create-style verbs.
    pub before: Option<PersonalityConfigChangeSnapshot>,
    /// Typed snapshot of relevant new state. `None` on tombstone-style verbs.
    pub after: Option<PersonalityConfigChangeSnapshot>,
    pub subject: PersonalityConfigChangedSubject,
    pub caller: PersonalityConfigChangedCaller,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PersonalityConfigChangeSnapshot {
    Personality {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personality_instance_id: Option<uuid::Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_entry_count: Option<usize>,
    },
    WakeEntry {
        wake_entry_id: uuid::Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch_applied: Option<bool>,
    },
    WakeEntries {
        wake_entry_count: usize,
        wake_entry_ids: Vec<uuid::Uuid>,
    },
    ReadScope {
        readable_personality_instance_ids: Vec<uuid::Uuid>,
    },
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
            after: Some(PersonalityConfigChangeSnapshot::Personality {
                personality_instance_id: None,
                display_name: Some("Engineer".into()),
                purpose: None,
                status: None,
                wake_entry_count: None,
            }),
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
