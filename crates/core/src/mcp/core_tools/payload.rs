//! Payload type for the `core/personality_config_changed_v1` Fact
//! memory emitted alongside every MCP-CRUD mutation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{FactPayload, PayloadKeyBuilder};

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

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("verb", self.verb.as_str());
        self.subject.add_to_key(&mut key, "subject");
        self.caller.add_to_key(&mut key, "caller");
        add_snapshot_to_key(&mut key, "before", self.before.as_ref());
        add_snapshot_to_key(&mut key, "after", self.after.as_ref());
        key.finish()
    }

    fn render(&self) -> String {
        format!("{:?} {:?}", self.verb, self.subject)
    }

    // No fact sidecar: the typed snapshot is persisted as this Fact's
    // citation cited-object (see `audit::write_fact`), so `sidecar_table`
    // inherits the trait default of `None`. The former
    // `proxima_core.personality_config_changed_v1` sidecar was always
    // empty and has been dropped from the schema.
}

impl PersonalityConfigChangedVerb {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Instantiate => "instantiate",
            Self::Tombstone => "tombstone",
            Self::SetWakeEntries => "set_wake_entries",
            Self::AddWakeEntry => "add_wake_entry",
            Self::UpdateWakeEntry => "update_wake_entry",
            Self::RemoveWakeEntry => "remove_wake_entry",
            Self::SetReadScope => "set_read_scope",
        }
    }
}

impl PersonalityConfigChangedSubject {
    fn add_to_key(&self, key: &mut PayloadKeyBuilder, prefix: &str) {
        match self {
            Self::Personality(id) => {
                key.field_str(&format!("{prefix}.kind"), "personality");
                key.field_uuid(&format!("{prefix}.id"), *id);
            }
            Self::WakeEntry(id) => {
                key.field_str(&format!("{prefix}.kind"), "wake_entry");
                key.field_uuid(&format!("{prefix}.id"), *id);
            }
        }
    }
}

impl PersonalityConfigChangedCaller {
    fn add_to_key(&self, key: &mut PayloadKeyBuilder, prefix: &str) {
        match self {
            Self::WakePersonality {
                personality_instance_id,
            } => {
                key.field_str(&format!("{prefix}.kind"), "wake_personality");
                key.field_uuid(
                    &format!("{prefix}.personality_instance_id"),
                    *personality_instance_id,
                );
            }
            Self::MasterToken {
                personality_instance_id,
            } => {
                key.field_str(&format!("{prefix}.kind"), "master_token");
                key.field_uuid(
                    &format!("{prefix}.personality_instance_id"),
                    *personality_instance_id,
                );
            }
        }
    }
}

fn add_snapshot_to_key(
    key: &mut PayloadKeyBuilder,
    prefix: &str,
    snapshot: Option<&PersonalityConfigChangeSnapshot>,
) {
    let Some(snapshot) = snapshot else {
        key.field_bool(&format!("{prefix}.present"), false);
        return;
    };
    key.field_bool(&format!("{prefix}.present"), true);
    match snapshot {
        PersonalityConfigChangeSnapshot::Personality {
            personality_instance_id,
            display_name,
            status,
            wake_entry_count,
        } => {
            key.field_str(&format!("{prefix}.kind"), "personality");
            key.field_option_uuid(
                &format!("{prefix}.personality_instance_id"),
                *personality_instance_id,
            );
            key.field_option_str(&format!("{prefix}.display_name"), display_name.as_deref());
            key.field_option_str(&format!("{prefix}.status"), status.as_deref());
            if let Some(count) = wake_entry_count {
                key.field_bool(&format!("{prefix}.wake_entry_count.present"), true);
                key.field_usize(&format!("{prefix}.wake_entry_count"), *count);
            } else {
                key.field_bool(&format!("{prefix}.wake_entry_count.present"), false);
            }
        }
        PersonalityConfigChangeSnapshot::WakeEntry {
            wake_entry_id,
            patch_applied,
        } => {
            key.field_str(&format!("{prefix}.kind"), "wake_entry");
            key.field_uuid(&format!("{prefix}.wake_entry_id"), *wake_entry_id);
            key.field_option_bool(&format!("{prefix}.patch_applied"), *patch_applied);
        }
        PersonalityConfigChangeSnapshot::WakeEntries {
            wake_entry_count,
            wake_entry_ids,
        } => {
            key.field_str(&format!("{prefix}.kind"), "wake_entries");
            key.field_usize(&format!("{prefix}.wake_entry_count"), *wake_entry_count);
            key.field_uuid_list(&format!("{prefix}.wake_entry_ids"), wake_entry_ids);
        }
        PersonalityConfigChangeSnapshot::ReadScope {
            readable_personality_instance_ids,
        } => {
            key.field_str(&format!("{prefix}.kind"), "read_scope");
            key.field_uuid_list(
                &format!("{prefix}.readable_personality_instance_ids"),
                readable_personality_instance_ids,
            );
        }
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
