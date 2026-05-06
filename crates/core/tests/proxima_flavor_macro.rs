use async_trait::async_trait;
use proxima_core::operators::{
    A2PContext, A2PContextSpec, A2POperator, F2AContext, F2AOperator, NewAbstraction,
    NewPerspective, OperatorError, PersonalitySnapshot,
};
use proxima_core::personality::PersonalityContext;
use proxima_core::{
    FactPayload, FlavorRegistry, PersonalityFlavor, PersonalityId, SchemaId, proxima_schema_id,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DummyFact {
    v: u32,
}

impl FactPayload for DummyFact {
    const SCHEMA_ID: &'static str = proxima_schema_id!("dummy-fact-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.dummy_fact_v1"
    }

    fn render(&self) -> String {
        self.v.to_string()
    }
}

#[derive(Debug, Default)]
struct DemoF2A;

#[async_trait]
impl F2AOperator for DemoF2A {
    fn operator_id(&self) -> &'static str {
        "proxima-core/f2a-demo"
    }

    fn output_schema_id(&self) -> &'static str {
        "proxima-core/dummy-fact-v1"
    }

    fn output_schema_version(&self) -> u32 {
        1
    }

    fn prompt_version(&self) -> &'static str {
        "v1"
    }

    fn consumes(&self, _: &SchemaId) -> bool {
        true
    }

    async fn run(&self, _: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
struct DemoA2P;

#[async_trait]
impl A2POperator for DemoA2P {
    fn operator_id(&self) -> &'static str {
        "proxima-core/a2p-demo"
    }

    fn output_schema_id(&self) -> &'static str {
        "proxima-core/dummy-fact-v1"
    }

    fn output_schema_version(&self) -> u32 {
        1
    }

    fn prompt_version(&self) -> &'static str {
        "v1"
    }

    fn consumes(&self, _: &SchemaId) -> bool {
        true
    }

    fn context(&self) -> A2PContextSpec {
        A2PContextSpec {
            kind: "on_ingest".into(),
            key: "k".into(),
            label: "l".into(),
        }
    }

    async fn run(&self, _: A2PContext<'_>) -> Result<Vec<NewPerspective>, OperatorError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
struct DemoPersonality;

#[async_trait]
impl PersonalityFlavor for DemoPersonality {
    fn personality_id(&self) -> &'static str {
        "proxima-core/personality-demo"
    }

    async fn snapshot(
        &self,
        _: &PersonalityContext<'_>,
    ) -> Result<PersonalitySnapshot, proxima_core::ProtocolError> {
        Ok(PersonalitySnapshot {
            personality_id: PersonalityId::new("proxima-core/personality-demo"),
            captured_at: OffsetDateTime::now_utc(),
        })
    }
}

proxima_core::proxima_flavor! {
    name = "proxima-core",
    fact_schemas = [DummyFact],
    personalities = [DemoPersonality],
    f2a_operators = [DemoF2A],
    a2p_operators = [DemoA2P],
}

#[test]
fn macro_registers_operators_via_proxima_flavor() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();
    assert_eq!(frozen.list_f2a_operators().len(), 1);
    assert_eq!(frozen.list_a2p_operators().len(), 1);
}
