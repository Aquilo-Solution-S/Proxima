use async_trait::async_trait;
use proxima_core::{
    ModelTier, Owner, PersonalityFlavor, PersonalitySelfDraft, PerspectivePayload, ProtocolError,
    SchemaId, SchemaVersion, WakeFilter, WakeFilterCtx, WakeFilterKind,
};
use schemars::JsonSchema;
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

    fn default_wake_filters(&self) -> Vec<WakeFilter> {
        Vec::new()
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Standard
    }
}

#[derive(Debug, Default)]
struct DemoWakeFilterKind;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DemoWakeParams {
    tag: String,
}

#[async_trait]
impl WakeFilterKind for DemoWakeFilterKind {
    fn kind_id(&self) -> &'static str {
        "proxima-test/demo-filter"
    }

    fn version(&self) -> u16 {
        1
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(DemoWakeParams)).unwrap()
    }

    async fn matches(
        &self,
        _ctx: &mut dyn WakeFilterCtx,
        _params: &serde_json::Value,
        _event: &proxima_core::ChangeEvent,
    ) -> Result<bool, ProtocolError> {
        Ok(false)
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
    wake_filter_kinds = [
        DemoWakeFilterKind,
    ],
}

#[test]
fn macro_registers_personalities_and_wake_filter_kinds() {
    let mut registry = proxima_core::FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();

    assert_eq!(frozen.list_personalities().len(), 1);
    assert_eq!(
        frozen.list_personalities()[0].personality_type_id(),
        "proxima-test/personality-v1"
    );
    let kind_ids: std::collections::HashSet<_> = frozen
        .list_wake_filter_kinds()
        .iter()
        .map(|kind| kind.kind_id())
        .collect();
    assert!(kind_ids.contains("core/on-memory"));
    assert!(kind_ids.contains("core/on-edge"));
    assert!(kind_ids.contains("proxima-test/demo-filter"));
}
