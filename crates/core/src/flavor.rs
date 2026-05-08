//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::verbs::schema::{FlavorRegistryFrozen, PayloadKind, PayloadValidatorEntry, SchemaInfo};
use crate::{
    AbstractionPayload, EdgePayload, FactPayload, GoalPayload, McpCallFn, McpTool,
    McpToolDescriptor, McpToolError, PersonalityFlavor, PerspectivePayload, RelationDescriptor,
    SchemaVersion, core_relation_descriptors,
};

use std::path::PathBuf;
use std::sync::Arc;

/// Structured per-flavor metadata. Populated by `proxima_flavor!` at
/// macro-expansion time so the `package_version` and `author` fields
/// reflect the calling crate's `Cargo.toml`.
///
/// One descriptor per `proxima_flavor!` invocation; the registry
/// cross-checks at `freeze()` time that every registered personality's
/// `personality_type_id` prefix matches a registered `flavor_id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FlavorDescriptor {
    /// Crate prefix used by `proxima_flavor! { name = ... }` —
    /// e.g. `"proxima-code"`. Schemas, relations, personalities, and
    /// MCP tools registered through the macro all start with this
    /// prefix followed by `/`.
    pub flavor_id: String,
    /// Human-friendly name shown in the UI. Defaults to `flavor_id`
    /// when the macro caller omits `display_name`.
    pub display_name: String,
    /// `CARGO_PKG_VERSION` of the flavor crate at build time.
    pub package_version: String,
    /// First author from `CARGO_PKG_AUTHORS` (split on `:` per Cargo
    /// convention, trimmed). `None` when the crate's `authors` field
    /// is empty.
    pub author: Option<String>,
    /// How this flavor was loaded into the binary. v1 is always
    /// `Builtin`; marketplace and local-dev variants are reserved for
    /// when out-of-process loading lands.
    pub provenance: FlavorProvenance,
}

/// Where the flavor came from. Reserved cases are out-of-scope for
/// v1 but kept on the wire so post-v1 flavors don't need a contract
/// change.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlavorProvenance {
    Builtin,
    Marketplace { source_url: String },
    Local { workspace_path: String },
}

#[derive(Debug)]
pub struct FlavorRegistry {
    schemas: Vec<SchemaInfo>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    personalities: Vec<Arc<dyn PersonalityFlavor>>,
    flavors: Vec<FlavorDescriptor>,
    /// Bundled recipe paths registered by `proxima_flavor! { recipes = [ ... ] }`.
    /// Slug shape is `<flavor_id>/<filename_without_ext>`.
    bundled_recipes: Vec<(String, PathBuf)>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        Self {
            schemas: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            personalities: Vec::new(),
            flavors: Vec::new(),
            bundled_recipes: Vec::new(),
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
            tombstone: F::tombstone().map(|t| crate::verbs::schema::SchemaTombstone {
                column: t.column.to_string(),
                value: t.value.to_string(),
            }),
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
            tombstone: None,
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
            tombstone: None,
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
            tombstone: None,
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
            tombstone: None,
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

    /// Record a bundled recipe under a unique slug. Slug shape is
    /// `<flavor_id>/<filename_without_ext>`. Panics on duplicate slug
    /// to catch flavor-author mistakes at registration time.
    pub fn add_bundled_recipe(&mut self, slug: String, path: PathBuf) {
        assert!(
            !self
                .bundled_recipes
                .iter()
                .any(|(existing, _)| existing == &slug),
            "duplicate bundled recipe slug {slug:?}"
        );
        self.bundled_recipes.push((slug, path));
    }

    /// Register a `FlavorDescriptor`. Called once per
    /// `proxima_flavor!` invocation; freeze panics if the same
    /// `flavor_id` is added twice.
    pub fn add_flavor(&mut self, descriptor: FlavorDescriptor) {
        self.flavors.push(descriptor);
    }

    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
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
        self.assert_flavor_descriptors();
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
            self.flavors,
            self.bundled_recipes,
        )
    }

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique, and
    /// every registered personality's `personality_type_id` prefix
    /// matches a registered flavor. Promotes the implicit prefix
    /// invariant from `proxima_flavor!`'s expansion-time asserts to a
    /// runtime registry guard so freestanding `add_personality` calls
    /// (test fixtures, etc.) cannot bypass it.
    fn assert_flavor_descriptors(&self) {
        let mut seen_ids = std::collections::HashSet::new();
        for flavor in &self.flavors {
            assert!(
                seen_ids.insert(flavor.flavor_id.as_str()),
                "duplicate FlavorDescriptor flavor_id registered: {}",
                flavor.flavor_id,
            );
        }
        for personality in &self.personalities {
            let type_id = personality.personality_type_id();
            let matched = self.flavors.iter().any(|f| {
                let prefix = format!("{}/", f.flavor_id);
                type_id.starts_with(&prefix)
            });
            assert!(
                matched,
                "PersonalityFlavor {type_id} has no matching FlavorDescriptor — \
                 register the flavor via `proxima_flavor!` or `FlavorRegistry::add_flavor`",
            );
        }
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
    fn bundled_recipe_round_trip_through_freeze() {
        let mut registry = FlavorRegistry::new();
        registry.add_flavor(FlavorDescriptor {
            flavor_id: "test-flavor".to_string(),
            display_name: "Test".to_string(),
            package_version: "0.0.0".to_string(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        });
        let path = PathBuf::from("/tmp/test-flavor/recipes/foo.yaml");
        registry.add_bundled_recipe("test-flavor/foo".to_string(), path.clone());
        let frozen = registry.freeze();
        assert_eq!(frozen.bundled_recipe_path("test-flavor/foo"), Some(path));
        assert_eq!(frozen.bundled_recipe_path("test-flavor/missing"), None);
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct SelfPayload {
        display_name: String,
    }

    impl PerspectivePayload for SelfPayload {
        const SCHEMA_ID: &'static str = "proxima-test/self";
        const SCHEMA_VERSION: u32 = 1;

        fn sidecar_table() -> &'static str {
            "proxima_test.self_v1"
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct OutPayload {
        summary: String,
    }

    impl PerspectivePayload for OutPayload {
        const SCHEMA_ID: &'static str = "proxima-test/out";
        const SCHEMA_VERSION: u32 = 1;

        fn sidecar_table() -> &'static str {
            "proxima_test.out_v1"
        }
    }

    #[derive(Debug)]
    struct DemoPersonality;

    impl PersonalityFlavor for DemoPersonality {
        fn personality_type_id(&self) -> &'static str {
            "proxima-test/personality"
        }

        fn default_display_name(&self) -> &'static str {
            "Demo"
        }

        fn default_purpose(&self) -> &'static str {
            "Demo personality used by FlavorRegistry tests"
        }
    }

    fn registry_with_personality() -> FlavorRegistry {
        let mut registry = FlavorRegistry::new();
        registry.add_flavor(FlavorDescriptor {
            flavor_id: "proxima-test".to_string(),
            display_name: "Proxima Test".to_string(),
            package_version: "0.0.0".to_string(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        });
        registry.add_perspective_schema::<SelfPayload>();
        registry.add_perspective_schema::<OutPayload>();
        registry.add_personality(DemoPersonality);
        registry
    }

    #[test]
    fn registers_and_lists_personalities() {
        let frozen = registry_with_personality().freeze();
        assert_eq!(frozen.list_personalities().len(), 1);
        assert_eq!(
            frozen.list_personalities()[0].personality_type_id(),
            "proxima-test/personality"
        );
    }
}
