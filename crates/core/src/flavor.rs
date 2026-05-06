//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::operators::{A2POperator, F2AOperator};
use crate::verbs::schema::{FlavorRegistryFrozen, PayloadKind, PayloadValidatorEntry, SchemaInfo};
use crate::{
    AbstractionPayload, EdgePayload, FactPayload, GoalPayload, McpCallFn, McpTool,
    McpToolDescriptor, McpToolError, PersonalityFlavor, PerspectivePayload, RelationDescriptor,
    SchemaVersion, core_relation_descriptors,
};

use std::sync::Arc;

#[derive(Debug)]
pub struct FlavorRegistry {
    schemas: Vec<SchemaInfo>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    personalities: Vec<Arc<dyn PersonalityFlavor>>,
    f2a_operators: Vec<Arc<dyn F2AOperator>>,
    a2p_operators: Vec<Arc<dyn A2POperator>>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        Self {
            schemas: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            personalities: Vec::new(),
            f2a_operators: Vec::new(),
            a2p_operators: Vec::new(),
        }
    }
}

impl FlavorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_fact_schema<F: FactPayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: F::schema_id(),
            schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: Some(F::sidecar_table().to_string()),
            natural_key_columns: F::natural_key_columns()
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            cbor_encoder: Some(encode_payload_cbor::<F>),
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: F::schema_id(),
            schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
            kind: PayloadKind::Fact,
            validate: validate_payload_type::<F>,
        });
    }

    pub fn add_abstraction_schema<A: AbstractionPayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: A::schema_id(),
            schema_version: SchemaVersion::new(A::SCHEMA_VERSION),
            kind: PayloadKind::Abstraction,
            filter_keys: vec![],
            sidecar_table: Some(A::sidecar_table().to_string()),
            natural_key_columns: vec![],
            cbor_encoder: Some(encode_payload_cbor::<A>),
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: A::schema_id(),
            schema_version: SchemaVersion::new(A::SCHEMA_VERSION),
            kind: PayloadKind::Abstraction,
            validate: validate_payload_type::<A>,
        });
    }

    pub fn add_perspective_schema<P: PerspectivePayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            kind: PayloadKind::Perspective,
            filter_keys: vec![],
            sidecar_table: Some(P::sidecar_table().to_string()),
            natural_key_columns: vec![],
            cbor_encoder: Some(encode_payload_cbor::<P>),
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            kind: PayloadKind::Perspective,
            validate: validate_payload_type::<P>,
        });
    }

    pub fn add_goal_schema<G: GoalPayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: G::schema_id(),
            schema_version: SchemaVersion::new(G::SCHEMA_VERSION),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: Some(G::sidecar_table().to_string()),
            natural_key_columns: vec![],
            cbor_encoder: Some(encode_payload_cbor::<G>),
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: G::schema_id(),
            schema_version: SchemaVersion::new(G::SCHEMA_VERSION),
            kind: PayloadKind::Goal,
            validate: validate_payload_type::<G>,
        });
    }

    /// Register a typed `EdgePayload` schema. The descriptor that
    /// references this schema must be registered separately via
    /// `add_relation`; the substrate cross-checks the linkage at
    /// `freeze()` time.
    pub fn add_edge_schema<E: EdgePayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: E::schema_id(),
            schema_version: SchemaVersion::new(E::SCHEMA_VERSION),
            kind: PayloadKind::Edge,
            filter_keys: vec![],
            sidecar_table: Some(E::sidecar_table().to_string()),
            natural_key_columns: vec![],
            cbor_encoder: Some(encode_payload_cbor::<E>),
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: E::schema_id(),
            schema_version: SchemaVersion::new(E::SCHEMA_VERSION),
            kind: PayloadKind::Edge,
            validate: validate_payload_type::<E>,
        });
    }

    /// Register a relation. Substrate-only relations carry no
    /// `payload_schema`; typed relations point at a registered
    /// `EdgePayload` schema.
    pub fn add_relation(&mut self, descriptor: RelationDescriptor) {
        self.relations.push(descriptor);
    }

    pub fn add_personality<P: PersonalityFlavor + 'static>(&mut self, personality: P) {
        self.personalities.push(Arc::new(personality));
    }

    pub fn add_f2a_operator<O: F2AOperator + 'static>(&mut self, op: O) {
        self.f2a_operators.push(Arc::new(op));
    }

    pub fn add_a2p_operator<O: A2POperator + 'static>(&mut self, op: O) {
        self.a2p_operators.push(Arc::new(op));
    }

    #[must_use]
    pub fn list_personalities(&self) -> &[Arc<dyn PersonalityFlavor>] {
        &self.personalities
    }

    pub fn add_mcp_tool<T: McpTool>(&mut self, expected_prefix: &str) {
        let prefix = format!("{expected_prefix}/");
        assert!(
            T::NAME.starts_with(&prefix),
            "McpTool::NAME {:?} must start with flavor prefix {:?}",
            T::NAME,
            prefix,
        );
        let schema = schemars::schema_for!(T::Args);
        let args_schema = serde_json::to_value(schema).expect("JsonSchema must serialize");
        let call: McpCallFn = |ctx, args| {
            Box::pin(async move {
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = T::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        };
        self.mcp_tools.push(McpToolDescriptor {
            name: T::NAME,
            description: T::DESCRIPTION,
            args_schema,
            call,
        });
    }

    #[must_use]
    pub fn freeze(self) -> FlavorRegistryFrozen {
        // Cross-check: every typed relation's payload_schema must
        // point at a registered Edge schema with the matching
        // RelationClass. Catches authoring drift at startup, not
        // at first edge-write.
        for rel in &self.relations {
            if let Some(payload_schema) = &rel.payload_schema {
                let info = self
                    .schemas
                    .iter()
                    .find(|s| {
                        s.kind == PayloadKind::Edge
                            && s.schema_id == payload_schema.schema_id
                            && s.schema_version == payload_schema.schema_version
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "RelationDescriptor {:?} references unregistered EdgePayload schema {:?} v{:?}",
                            rel.relation,
                            payload_schema.schema_id.as_str(),
                            payload_schema.schema_version.into_inner(),
                        )
                    });
                let _ = info;
            }
        }
        if !self.a2p_operators.is_empty() && self.personalities.is_empty() {
            let names: Vec<&str> = self
                .a2p_operators
                .iter()
                .map(|op| op.operator_id())
                .collect();
            panic!(
                "A2POperator(s) {names:?} registered but no PersonalityFlavor; \
                 every A2P operator requires at least one PersonalityFlavor in \
                 the same FlavorRegistry. Add a `personalities = [...]` entry \
                 in proxima_flavor!"
            );
        }
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            assert!(
                seen_tools.insert(tool.name),
                "duplicate McpTool name registered: {}",
                tool.name,
            );
        }
        FlavorRegistryFrozen::with_schemas_relations_validators(
            self.schemas,
            self.relations,
            self.validators,
            self.mcp_tools,
            self.personalities,
            self.f2a_operators,
            self.a2p_operators,
        )
    }
}

fn validate_payload_type<T>(value: &serde_json::Value) -> Result<(), String>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn encode_payload_cbor<T>(value: &serde_json::Value) -> Result<Vec<u8>, String>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let typed = serde_json::from_value::<T>(value.clone()).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&typed, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod mcp_tool_registry_tests {
    use super::*;
    use crate::mcp::{McpToolCtx, McpToolError};

    struct Demo;

    impl McpTool for Demo {
        const NAME: &'static str = "proxima-test/demo";
        const DESCRIPTION: &'static str = "test";
        type Args = ();
        type Output = ();

        fn call(
            _ctx: McpToolCtx,
            _args: (),
        ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn add_mcp_tool_lists_descriptor() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool::<Demo>("proxima-test");
        let frozen = registry.freeze();
        let names: Vec<_> = frozen.list_mcp_tools().iter().map(|d| d.name).collect();
        assert!(names.contains(&"proxima-test/demo"));
    }

    #[test]
    fn freeze_rejects_duplicate_tool_names() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool::<Demo>("proxima-test");
        registry.add_mcp_tool::<Demo>("proxima-test");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.freeze()));
        assert!(result.is_err(), "freeze must panic on duplicate tool names");
    }

    #[test]
    fn add_mcp_tool_rejects_unprefixed_tool_name() {
        struct Bad;

        impl McpTool for Bad {
            const NAME: &'static str = "wrong/demo";
            const DESCRIPTION: &'static str = "x";
            type Args = ();
            type Output = ();

            fn call(
                _ctx: McpToolCtx,
                _args: (),
            ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
                Box::pin(async { Ok(()) })
            }
        }

        let mut registry = FlavorRegistry::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.add_mcp_tool::<Bad>("proxima-test");
        }));
        assert!(result.is_err(), "must panic on prefix mismatch");
    }

    #[test]
    fn registers_and_lists_f2a_and_a2p_operators() {
        use crate::operators::{
            A2PContext, A2PContextSpec, A2POperator, F2AContext, F2AOperator, NewAbstraction,
            NewPerspective, OperatorError, PersonalitySnapshot,
        };
        use crate::personality::PersonalityContext;
        use crate::{PersonalityId, SchemaId};
        use async_trait::async_trait;
        use time::OffsetDateTime;

        #[derive(Debug)]
        struct DemoF2A;

        #[async_trait]
        impl F2AOperator for DemoF2A {
            fn operator_id(&self) -> &'static str {
                "proxima-test/f2a"
            }

            fn output_schema_id(&self) -> &'static str {
                "proxima-test/out"
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

        #[derive(Debug)]
        struct DemoA2P;

        #[async_trait]
        impl A2POperator for DemoA2P {
            fn operator_id(&self) -> &'static str {
                "proxima-test/a2p"
            }

            fn output_schema_id(&self) -> &'static str {
                "proxima-test/out"
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

        #[derive(Debug)]
        struct DemoPersonality;

        #[async_trait]
        impl PersonalityFlavor for DemoPersonality {
            fn personality_id(&self) -> &'static str {
                "proxima-test/personality"
            }

            async fn snapshot(
                &self,
                _: &PersonalityContext<'_>,
            ) -> Result<PersonalitySnapshot, crate::ProtocolError> {
                Ok(PersonalitySnapshot {
                    personality_id: PersonalityId::new("proxima-test/personality"),
                    captured_at: OffsetDateTime::now_utc(),
                })
            }
        }

        let mut registry = FlavorRegistry::new();
        registry.add_f2a_operator(DemoF2A);
        registry.add_a2p_operator(DemoA2P);
        registry.add_personality(DemoPersonality);
        let frozen = registry.freeze();
        assert_eq!(frozen.list_f2a_operators().len(), 1);
        assert_eq!(frozen.list_a2p_operators().len(), 1);
        assert_eq!(
            frozen.list_f2a_operators()[0].operator_id(),
            "proxima-test/f2a"
        );
        assert_eq!(
            frozen.list_a2p_operators()[0].operator_id(),
            "proxima-test/a2p"
        );
    }

    #[test]
    fn freeze_rejects_a2p_operator_without_personality() {
        use crate::SchemaId;
        use crate::operators::{
            A2PContext, A2PContextSpec, A2POperator, NewPerspective, OperatorError,
        };
        use async_trait::async_trait;

        #[derive(Debug)]
        struct DemoA2P;

        #[async_trait]
        impl A2POperator for DemoA2P {
            fn operator_id(&self) -> &'static str {
                "proxima-test/a2p"
            }

            fn output_schema_id(&self) -> &'static str {
                "proxima-test/out"
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

        let mut registry = FlavorRegistry::new();
        registry.add_a2p_operator(DemoA2P);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.freeze()));
        let err = result.expect_err("freeze must panic when an A2P operator has no personality");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&'static str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("proxima-test/a2p")
                && msg.contains("requires at least one PersonalityFlavor"),
            "panic message must name the offending operator and missing requirement: {msg}"
        );
    }
}
