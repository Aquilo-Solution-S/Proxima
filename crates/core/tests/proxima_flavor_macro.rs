use async_trait::async_trait;
use proxima_core::{
    ModelTier, Owner, PersonalityFlavor, PersonalitySelfDraft, PerspectivePayload, ProtocolError,
    SchemaId, SchemaVersion,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct DemoSelfPayload {
    display_name: String,
}

impl PerspectivePayload for DemoSelfPayload {
    const SCHEMA_ID: &'static str = "proxima-test/self-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.self_v1"
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DemoOutputPayload {
    summary: String,
}

impl PerspectivePayload for DemoOutputPayload {
    const SCHEMA_ID: &'static str = "proxima-test/out-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.out_v1"
    }
}

#[derive(Debug, Default)]
struct DemoPersonality;

#[async_trait]
impl PersonalityFlavor for DemoPersonality {
    fn personality_type_id(&self) -> &'static str {
        "proxima-test/personality-v1"
    }

    fn self_schema(&self) -> SchemaId {
        SchemaId::new(DemoSelfPayload::SCHEMA_ID.to_string())
    }

    fn default_self_payload(
        &self,
        _owner: &Owner,
        _payload_overrides: Option<&serde_json::Value>,
    ) -> Result<PersonalitySelfDraft, ProtocolError> {
        Ok(PersonalitySelfDraft {
            schema_id: self.self_schema(),
            schema_version: SchemaVersion::new(1),
            text: "Demo".into(),
            typed_payload: serde_json::json!({ "display_name": "Demo" }),
        })
    }

    fn system_prompt(&self) -> &'static str {
        "demo"
    }

    fn writeable_schemas(&self) -> &'static [&'static str] {
        &[DemoOutputPayload::SCHEMA_ID]
    }

    fn writeable_relations(&self) -> &'static [&'static str] {
        &[]
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Standard
    }
}

proxima_core::proxima_flavor! {
    name = "proxima-test",
    perspective_schemas = [
        DemoSelfPayload,
        DemoOutputPayload,
    ],
    personalities = [
        DemoPersonality,
    ],
}

#[test]
fn macro_registers_personalities() {
    let mut registry = proxima_core::FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();

    assert_eq!(frozen.list_personalities().len(), 1);
    assert_eq!(
        frozen.list_personalities()[0].personality_type_id(),
        "proxima-test/personality-v1"
    );
}
