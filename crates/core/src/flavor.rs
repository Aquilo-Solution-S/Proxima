//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::authz::{AuthorizationHook, OwnerResolver};
use crate::mcp::schema::mcp_tool_schema;
use crate::mcp::validate_action_args;
use crate::verbs::schema::{
    FlavorRegistryFrozen, MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
    ProtocolPayload, ProtocolPayloadIngress, ProtocolPayloadIngressEntry, SchemaCapabilityTags,
    SchemaInfo,
};
use crate::{
    AbstractionPayload, CapabilityTag, CitationMappingPayload, CitedObjectPayload,
    DependencySatisfactionRule, EdgePayload, FactPayload, GoalPayload, McpCallFn, McpTool,
    McpToolDescriptor, McpToolError, McpToolOrigin, PerspectivePayload, RelationDescriptor,
    RequestBehavior, SchemaId, SchemaVersion, ScopeGateBehavior, SidecarPayload, Tool,
    core_relation_descriptors,
};

use std::collections::BTreeSet;
use std::sync::Arc;

/// Structured per-flavor metadata. Populated by `proxima_flavor!` at
/// macro-expansion time so the `package_version` and `author` fields
/// reflect the calling crate's `Cargo.toml`.
///
/// One descriptor per `proxima_flavor!` invocation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// `Builtin`; the other variants are inert forward-compat
    /// placeholders, not a runtime registration contract.
    pub provenance: FlavorProvenance,
}

/// Where the flavor came from. Reserved cases are out-of-scope for
/// v1 and do not imply dynamic flavor loading.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlavorProvenance {
    Builtin,
    Marketplace { source_url: String },
    Local { workspace_path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    DuplicateRelation {
        relation: String,
    },
    DuplicateTool {
        name: &'static str,
    },
    DuplicateFlavor {
        flavor_id: String,
    },
    DuplicateDependencyRule {
        schema_id: String,
    },
    InvalidRelationDescriptor {
        relation: String,
        message: String,
    },
    InvalidCapabilityTag {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        tag: String,
        message: String,
    },
    InvalidToolName {
        name: &'static str,
        expected_prefix: String,
        message: String,
    },
    DuplicateOwnerResolver,
    UnregisteredRelationPayload {
        relation: String,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    SchemaIngressMismatch {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    UnregisteredSchemaCapabilityTags {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    UnsatisfiableRelationTags {
        relation: String,
        side: &'static str,
    },
}

impl std::fmt::Display for FlavorRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSchema {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "duplicate schema registered: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::DuplicateRelation { relation } => {
                write!(f, "duplicate relation descriptor registered: {relation}")
            }
            Self::DuplicateTool { name } => {
                write!(f, "duplicate tool name registered: {name}")
            }
            Self::DuplicateFlavor { flavor_id } => {
                write!(f, "duplicate flavor descriptor registered: {flavor_id}")
            }
            Self::DuplicateDependencyRule { schema_id } => {
                write!(
                    f,
                    "duplicate dependency satisfaction rule for schema {schema_id}"
                )
            }
            Self::InvalidRelationDescriptor { relation, message } => {
                write!(f, "relation descriptor {relation} is invalid: {message}")
            }
            Self::InvalidCapabilityTag {
                schema_id,
                schema_version,
                kind,
                tag,
                message,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has invalid capability tag {tag:?}: {message}"
            ),
            Self::InvalidToolName {
                name,
                expected_prefix,
                message,
            } => write!(
                f,
                "tool name {name:?} is invalid for prefix {expected_prefix:?}: {message}"
            ),
            Self::DuplicateOwnerResolver => f.write_str("duplicate owner resolver registered"),
            Self::UnregisteredRelationPayload {
                relation,
                schema_id,
                schema_version,
            } => write!(
                f,
                "relation descriptor {relation} references unregistered EdgePayload schema {schema_id} v{schema_version}"
            ),
            Self::SchemaIngressMismatch {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has mismatched typed-ingress registration"
            ),
            Self::UnregisteredSchemaCapabilityTags {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema capability tags reference unregistered schema: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::UnsatisfiableRelationTags { relation, side } => write!(
                f,
                "relation descriptor {relation} has unsatisfiable {side} required capability tags"
            ),
        }
    }
}

impl std::error::Error for FlavorRegistryError {}

/// Mutable build-time registry. Flavors push into it during their
/// `register()` call; `freeze` consumes it whole into a
/// `FlavorRegistryFrozen` via `FlavorRegistryFrozen::from_registry`.
/// Fields are `pub(crate)` so that constructor can destructure them.
#[derive(Debug)]
pub struct FlavorRegistry {
    pub(crate) schemas: Vec<SchemaInfo>,
    pub(crate) schema_capability_tags: Vec<SchemaCapabilityTags>,
    pub(crate) search_projections: Vec<MemorySearchProjection>,
    pub(crate) relations: Vec<RelationDescriptor>,
    pub(crate) protocol_ingress: Vec<ProtocolPayloadIngressEntry>,
    pub(crate) mcp_tools: Vec<McpToolDescriptor>,
    pub(crate) request_behaviors: Vec<Arc<dyn RequestBehavior>>,
    pub(crate) flavors: Vec<FlavorDescriptor>,
    pub(crate) dependency_satisfaction_rules: Vec<(String, Arc<dyn DependencySatisfactionRule>)>,
    pub(crate) owner_resolver: Option<Arc<dyn OwnerResolver>>,
    pub(crate) authorization_hooks: Vec<Arc<dyn AuthorizationHook>>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            schema_capability_tags: Vec::new(),
            search_projections: Vec::new(),
            relations: core_relation_descriptors(),
            protocol_ingress: Vec::new(),
            mcp_tools: Vec::new(),
            request_behaviors: vec![Arc::new(ScopeGateBehavior)],
            flavors: Vec::new(),
            dependency_satisfaction_rules: Vec::new(),
            owner_resolver: None,
            authorization_hooks: Vec::new(),
        };
        registry
            .try_add_cited_object_schema::<crate::citations::UploadedBlobPayload>()
            .expect("built-in cited-object schema registration must be valid");
        registry
            .try_add_fact_schema::<crate::verbs::persist_mcp_call::McpCallLoggedV1>()
            .expect("built-in MCP call fact schema registration must be valid");
        registry
            .try_add_cited_object_schema::<crate::verbs::persist_mcp_call::McpCallIoV1>()
            .expect("built-in MCP call cited-object schema registration must be valid");
        registry
            .try_add_citation_mapping_schema::<crate::verbs::persist_mcp_call::McpCallIoCitationV1>(
            )
            .expect("built-in MCP call citation-mapping schema registration must be valid");
        crate::memory::register_all(&mut registry)
            .expect("built-in memory registration must be valid");
        crate::goal::register_all(&mut registry).expect("built-in goal registration must be valid");
        crate::mcp::core_tools::register_all(&mut registry)
            .expect("built-in MCP tool registration must be valid");
        registry
    }
}

impl FlavorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared tail for the typed `add_*_schema` methods: records the
    /// optional search projection, the `SchemaInfo`, and the protocol
    /// ingress entry. Callers build the kind-specific `SchemaInfo`;
    /// `schema_id` / `schema_version` / `kind` for the ingress entry
    /// are read back off it so they cannot drift from the schema.
    fn register_schema(
        &mut self,
        schema_info: SchemaInfo,
        search_projection: Option<crate::SearchProjection>,
        ingress: ProtocolPayloadIngress,
        json_schema: Option<serde_json::Value>,
    ) {
        maybe_add_search_projection(
            &mut self.search_projections,
            &schema_info,
            search_projection,
        );
        let schema_id = schema_info.schema_id.clone();
        let schema_version = schema_info.schema_version;
        let kind = schema_info.kind;
        self.schemas.push(schema_info);
        self.protocol_ingress.push(ProtocolPayloadIngressEntry {
            schema_id,
            schema_version,
            kind,
            ingress,
            json_schema,
        });
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_fact_schema<F: FactPayload>(&mut self) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: F::schema_id(),
                schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
                kind: PayloadKind::Fact,
                filter_keys: vec![],
                sidecar_table: F::sidecar_table().map(std::string::ToString::to_string),
                natural_key_columns: F::natural_key_columns()
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                tombstone: F::tombstone().map(|t| crate::verbs::schema::SchemaTombstone {
                    column: t.column.to_string(),
                    value: t.value.to_string(),
                }),
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            F::search_projection(),
            ingest_fact_payload::<F>,
            F::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_fact_schema_or_panic_for_tests<F: FactPayload>(&mut self) {
        self.try_add_fact_schema::<F>()
            .expect("fact schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_abstraction_schema<A: AbstractionPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: A::schema_id(),
                schema_version: SchemaVersion::new(A::SCHEMA_VERSION),
                kind: PayloadKind::Abstraction,
                filter_keys: vec![],
                sidecar_table: Some(A::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            A::search_projection(),
            ingest_abstraction_payload::<A>,
            A::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_abstraction_schema_or_panic_for_tests<A: AbstractionPayload>(&mut self) {
        self.try_add_abstraction_schema::<A>()
            .expect("abstraction schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_perspective_schema<P: PerspectivePayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: P::schema_id(),
                schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
                kind: PayloadKind::Perspective,
                filter_keys: vec![],
                sidecar_table: Some(P::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            P::search_projection(),
            ingest_perspective_payload::<P>,
            P::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_perspective_schema_or_panic_for_tests<P: PerspectivePayload>(&mut self) {
        self.try_add_perspective_schema::<P>()
            .expect("perspective schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_goal_schema<G: GoalPayload>(&mut self) -> Result<(), FlavorRegistryError> {
        let sidecar_table = G::sidecar_table();
        self.register_schema(
            SchemaInfo {
                schema_id: G::schema_id(),
                schema_version: SchemaVersion::new(G::SCHEMA_VERSION),
                kind: PayloadKind::Goal,
                filter_keys: vec![],
                sidecar_table: sidecar_table.map(std::string::ToString::to_string),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            None,
            ingest_goal_payload::<G>,
            G::json_schema(),
        );
        let _ = sidecar_table;
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_goal_schema_or_panic_for_tests<G: GoalPayload>(&mut self) {
        self.try_add_goal_schema::<G>()
            .expect("goal schema registration must be valid");
    }

    /// Register a typed `EdgePayload` schema. The descriptor that
    /// references this schema must be registered separately via
    /// `add_relation`; the substrate cross-checks the linkage at
    /// `freeze()` time.
    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_edge_schema<E: EdgePayload>(&mut self) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: E::schema_id(),
                schema_version: SchemaVersion::new(E::SCHEMA_VERSION),
                kind: PayloadKind::Edge,
                filter_keys: vec![],
                sidecar_table: Some(E::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            None,
            ingest_edge_payload::<E>,
            E::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_edge_schema_or_panic_for_tests<E: EdgePayload>(&mut self) {
        self.try_add_edge_schema::<E>()
            .expect("edge schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_cited_object_schema<C: CitedObjectPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: C::schema_id(),
                schema_version: SchemaVersion::new(C::SCHEMA_VERSION),
                kind: PayloadKind::CitedObject,
                filter_keys: vec![],
                sidecar_table: Some(C::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
            },
            None,
            ingest_cited_object_payload::<C>,
            C::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_cited_object_schema_or_panic_for_tests<C: CitedObjectPayload>(&mut self) {
        self.try_add_cited_object_schema::<C>()
            .expect("cited-object schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_citation_mapping_schema<M: CitationMappingPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: M::schema_id(),
                schema_version: SchemaVersion::new(M::SCHEMA_VERSION),
                kind: PayloadKind::CitationMapping,
                filter_keys: vec![],
                sidecar_table: M::sidecar_table().map(std::string::ToString::to_string),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: Some(M::cited_object_schema()),
            },
            None,
            ingest_citation_mapping_payload::<M>,
            M::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_citation_mapping_schema_or_panic_for_tests<M: CitationMappingPayload>(&mut self) {
        self.try_add_citation_mapping_schema::<M>()
            .expect("citation-mapping schema registration must be valid");
    }

    /// Register an *opaque* schema — one with no Rust payload type.
    /// The blessed path for content-addressed `CitedObject`s and
    /// structural `CitationMapping`s whose payload is an opaque blob;
    /// it carries no typed ingress parser and no JSON schema, so
    /// `ingest_protocol_payload` accepts any object payload for it.
    ///
    /// This is the *only* sanctioned way to register an untyped schema.
    /// `freeze()` asserts every other schema has a typed ingress parser,
    /// so a dropped parser fails the build rather than silently disabling
    /// validation and typed sidecar construction.
    /// # Errors
    ///
    /// Currently infallible; returns a registry error if opaque schema
    /// admission adds validation.
    pub fn try_add_opaque_schema(
        &mut self,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) -> Result<(), FlavorRegistryError> {
        self.schemas
            .push(SchemaInfo::opaque(schema_id, schema_version, kind));
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_opaque_schema_or_panic_for_tests(
        &mut self,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) {
        self.try_add_opaque_schema(schema_id, schema_version, kind)
            .expect("opaque schema registration must be valid");
    }

    /// Register a relation. Substrate-only relations carry no
    /// `payload_schema`; typed relations point at a registered
    /// `EdgePayload` schema.
    /// # Errors
    ///
    /// Returns `InvalidRelationDescriptor` when descriptor-local masks are
    /// invalid.
    pub fn try_add_relation(
        &mut self,
        descriptor: RelationDescriptor,
    ) -> Result<(), FlavorRegistryError> {
        if let Err(message) = descriptor.validate_descriptor() {
            return Err(FlavorRegistryError::InvalidRelationDescriptor {
                relation: descriptor.relation,
                message,
            });
        }
        self.relations.push(descriptor);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_relation_or_panic_for_tests(&mut self, descriptor: RelationDescriptor) {
        self.try_add_relation(descriptor)
            .expect("relation descriptor registration must be valid");
    }

    /// Attach opaque capability tags to a registered payload schema.
    ///
    /// # Panics
    ///
    /// Panics if any tag fails [`CapabilityTag::parse`]. The schema
    /// existence check runs at [`Self::freeze`], after every flavor has
    /// registered its schemas.
    /// # Errors
    ///
    /// Returns `InvalidCapabilityTag` when any tag fails capability syntax.
    pub fn try_add_schema_capability_tags<'a>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        version: SchemaVersion,
        tags: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), FlavorRegistryError> {
        let mut parsed = BTreeSet::new();
        for tag in tags {
            parsed.insert(CapabilityTag::parse(tag).map_err(|err| {
                FlavorRegistryError::InvalidCapabilityTag {
                    schema_id: schema_id.clone(),
                    schema_version: version,
                    kind,
                    tag: tag.to_string(),
                    message: err.to_string(),
                }
            })?);
        }
        self.schema_capability_tags.push(SchemaCapabilityTags {
            schema_id,
            schema_version: version,
            kind,
            tags: parsed,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_schema_capability_tags_or_panic_for_tests<'a>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        version: SchemaVersion,
        tags: impl IntoIterator<Item = &'a str>,
    ) {
        self.try_add_schema_capability_tags(kind, schema_id, version, tags)
            .expect("schema capability tags must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; duplicate rule ids are checked by
    /// [`Self::try_freeze`].
    pub fn try_add_dependency_satisfaction_rule(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) -> Result<(), FlavorRegistryError> {
        self.dependency_satisfaction_rules
            .push((schema_id.into(), rule));
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_dependency_satisfaction_rule_or_panic_for_tests(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) {
        self.try_add_dependency_satisfaction_rule(schema_id, rule)
            .expect("dependency satisfaction rule registration must be valid");
    }

    /// Register the composed app's owner resolver.
    /// # Errors
    ///
    /// Returns `DuplicateOwnerResolver` when a resolver is already registered.
    pub fn try_set_owner_resolver(
        &mut self,
        resolver: Arc<dyn OwnerResolver>,
    ) -> Result<(), FlavorRegistryError> {
        if self.owner_resolver.is_some() {
            return Err(FlavorRegistryError::DuplicateOwnerResolver);
        }
        self.owner_resolver = Some(resolver);
        Ok(())
    }

    #[doc(hidden)]
    pub fn set_owner_resolver_or_panic_for_tests(&mut self, resolver: Arc<dyn OwnerResolver>) {
        self.try_set_owner_resolver(resolver)
            .expect("owner resolver registration must be valid");
    }

    pub fn add_authorization_hook(&mut self, hook: Arc<dyn AuthorizationHook>) {
        self.authorization_hooks.push(hook);
    }

    pub fn add_request_behavior(&mut self, behavior: impl RequestBehavior + 'static) {
        self.request_behaviors.push(Arc::new(behavior));
    }

    /// # Errors
    ///
    /// Currently infallible; duplicate flavor ids are checked by
    /// [`Self::try_freeze`].
    pub fn try_add_flavor(
        &mut self,
        descriptor: FlavorDescriptor,
    ) -> Result<(), FlavorRegistryError> {
        self.flavors.push(descriptor);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_flavor_or_panic_for_tests(&mut self, descriptor: FlavorDescriptor) {
        self.try_add_flavor(descriptor)
            .expect("flavor descriptor registration must be valid");
    }

    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
    }

    /// # Errors
    ///
    /// Returns `InvalidToolName` when the tool name does not match the expected
    /// prefix or provider-safe form.
    pub fn try_add_tool<T: Tool>(
        &mut self,
        expected_prefix: &str,
    ) -> Result<(), FlavorRegistryError> {
        let slash = format!("{expected_prefix}/");
        let under = format!("{expected_prefix}_");
        validate_tool_name(T::NAME, expected_prefix, &slash, &under)?;
        let args_schema = mcp_tool_schema::<T::Args>();
        let call: McpCallFn = |ctx, args| {
            Box::pin(async move {
                validate_action_args(T::NAME, &[], &args)?;
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = <T as McpTool>::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        };
        self.mcp_tools.push(McpToolDescriptor {
            name: T::NAME,
            description: T::DESCRIPTION,
            origin: if expected_prefix == "core" {
                McpToolOrigin::Substrate
            } else {
                McpToolOrigin::Flavor(expected_prefix.to_string())
            },
            produces_schema_ids: T::PRODUCES_SCHEMA_IDS,
            args_schema,
            action_arg_specs: &[],
            call,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_tool_or_panic_for_tests<T: Tool>(&mut self, expected_prefix: &str) {
        self.try_add_tool::<T>(expected_prefix)
            .expect("tool registration must be valid");
    }

    /// Register a flavor-shipped MCP tool under `expected_prefix`.
    #[doc(hidden)]
    pub fn try_add_mcp_tool<T: McpTool>(
        &mut self,
        expected_prefix: &str,
    ) -> Result<(), FlavorRegistryError> {
        let slash = format!("{expected_prefix}/");
        let under = format!("{expected_prefix}_");
        validate_tool_name(T::NAME, expected_prefix, &slash, &under)?;
        let args_schema = mcp_tool_schema::<T::Args>();
        let call: McpCallFn = |ctx, args| {
            Box::pin(async move {
                validate_action_args(T::NAME, T::ACTION_ARG_SPECS, &args)?;
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = T::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        };
        self.mcp_tools.push(McpToolDescriptor {
            name: T::NAME,
            description: T::DESCRIPTION,
            origin: if expected_prefix == "core" {
                McpToolOrigin::Substrate
            } else {
                McpToolOrigin::Flavor(expected_prefix.to_string())
            },
            produces_schema_ids: T::PRODUCES_SCHEMA_IDS,
            args_schema,
            action_arg_specs: T::ACTION_ARG_SPECS,
            call,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_mcp_tool_or_panic_for_tests<T: McpTool>(&mut self, expected_prefix: &str) {
        self.try_add_mcp_tool::<T>(expected_prefix)
            .expect("MCP tool registration must be valid");
    }

    /// Validate the registry and seal it for runtime use.
    /// # Errors
    ///
    /// Returns typed registry errors for invalid descriptors, unregistered
    /// references, ingress mismatches, unsatisfiable tags, and duplicate ids.
    pub fn try_freeze(self) -> Result<FlavorRegistryFrozen, FlavorRegistryError> {
        for rel in &self.relations {
            if let Err(message) = rel.validate_descriptor() {
                return Err(FlavorRegistryError::InvalidRelationDescriptor {
                    relation: rel.relation.clone(),
                    message,
                });
            }
            if let Some(payload_schema) = &rel.payload_schema
                && !self.schemas.iter().any(|s| {
                    s.kind == PayloadKind::Edge
                        && s.schema_id == payload_schema.schema_id
                        && s.schema_version == payload_schema.schema_version
                })
            {
                return Err(FlavorRegistryError::UnregisteredRelationPayload {
                    relation: rel.relation.clone(),
                    schema_id: payload_schema.schema_id.clone(),
                    schema_version: payload_schema.schema_version,
                });
            }
        }
        self.validate_schema_capability_tags_resolve()?;
        self.validate_required_relation_tags_satisfiable()?;
        self.validate_flavor_descriptors()?;
        // Every schema is either typed (a protocol-ingress parser) or
        // opaque. A typed schema whose ingress parser was dropped would
        // make `ingest_protocol_payload` silently accept any payload —
        // catch the drift here, not at first write.
        for schema in &self.schemas {
            let has_ingress = self.protocol_ingress.iter().any(|v| {
                v.schema_id == schema.schema_id
                    && v.schema_version == schema.schema_version
                    && v.kind == schema.kind
            });
            if schema.has_typed_ingress != has_ingress {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        let mut seen_schemas = std::collections::HashSet::new();
        for schema in &self.schemas {
            if !seen_schemas.insert((schema.schema_id.clone(), schema.schema_version, schema.kind))
            {
                return Err(FlavorRegistryError::DuplicateSchema {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        let mut seen_relations = std::collections::HashSet::new();
        for rel in &self.relations {
            if !seen_relations.insert(rel.relation.clone()) {
                return Err(FlavorRegistryError::DuplicateRelation {
                    relation: rel.relation.clone(),
                });
            }
        }
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            if !seen_tools.insert(tool.name) {
                return Err(FlavorRegistryError::DuplicateTool { name: tool.name });
            }
        }
        let mut seen_dependency_rules: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (schema_id, _) in &self.dependency_satisfaction_rules {
            if !seen_dependency_rules.insert(schema_id.as_str()) {
                return Err(FlavorRegistryError::DuplicateDependencyRule {
                    schema_id: schema_id.clone(),
                });
            }
        }
        Ok(FlavorRegistryFrozen::from_registry(self))
    }

    #[must_use]
    #[doc(hidden)]
    pub fn freeze_or_panic_for_tests(self) -> FlavorRegistryFrozen {
        self.try_freeze()
            .expect("flavor registry must be valid before freeze")
    }

    fn validate_schema_capability_tags_resolve(&self) -> Result<(), FlavorRegistryError> {
        for binding in &self.schema_capability_tags {
            if !self.schemas.iter().any(|schema| {
                schema.schema_id == binding.schema_id
                    && schema.schema_version == binding.schema_version
                    && schema.kind == binding.kind
            }) {
                return Err(FlavorRegistryError::UnregisteredSchemaCapabilityTags {
                    schema_id: binding.schema_id.clone(),
                    schema_version: binding.schema_version,
                    kind: binding.kind,
                });
            }
        }
        Ok(())
    }

    fn validate_required_relation_tags_satisfiable(&self) -> Result<(), FlavorRegistryError> {
        let declared = schema_capability_map(&self.schema_capability_tags);
        for relation in &self.relations {
            self.validate_relation_side_tags_satisfiable(
                relation,
                "source",
                relation.source_kind_mask,
                &relation.source_required_tags,
                &declared,
            )?;
            self.validate_relation_side_tags_satisfiable(
                relation,
                "target",
                relation.target_kind_mask,
                &relation.target_required_tags,
                &declared,
            )?;
        }
        Ok(())
    }

    fn validate_relation_side_tags_satisfiable(
        &self,
        relation: &RelationDescriptor,
        side: &'static str,
        kind_mask: crate::EntityKindMask,
        required_tags: &BTreeSet<CapabilityTag>,
        declared: &std::collections::HashMap<
            (SchemaId, SchemaVersion, PayloadKind),
            BTreeSet<CapabilityTag>,
        >,
    ) -> Result<(), FlavorRegistryError> {
        if required_tags.is_empty() {
            return Ok(());
        }
        let admitted = self.schemas.iter().any(|schema| {
            payload_kind_admitted_by_mask(schema.kind, kind_mask)
                && declared
                    .get(&(schema.schema_id.clone(), schema.schema_version, schema.kind))
                    .is_some_and(|tags| required_tags.is_subset(tags))
        });
        if !admitted {
            return Err(FlavorRegistryError::UnsatisfiableRelationTags {
                relation: relation.relation.clone(),
                side,
            });
        }
        Ok(())
    }

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique.
    fn validate_flavor_descriptors(&self) -> Result<(), FlavorRegistryError> {
        let mut seen_ids = std::collections::HashSet::new();
        for flavor in &self.flavors {
            if !seen_ids.insert(flavor.flavor_id.as_str()) {
                return Err(FlavorRegistryError::DuplicateFlavor {
                    flavor_id: flavor.flavor_id.clone(),
                });
            }
        }
        Ok(())
    }
}

fn validate_tool_name(
    name: &'static str,
    expected_prefix: &str,
    slash: &str,
    under: &str,
) -> Result<(), FlavorRegistryError> {
    if !(name.starts_with(slash) || name.starts_with(under)) {
        return Err(FlavorRegistryError::InvalidToolName {
            name,
            expected_prefix: expected_prefix.to_string(),
            message: format!("expected prefix {slash:?} or {under:?}"),
        });
    }
    let provider_safe = crate::mcp::provider_safe_tool_name(name);
    if name != provider_safe {
        return Err(FlavorRegistryError::InvalidToolName {
            name,
            expected_prefix: expected_prefix.to_string(),
            message: format!(
                "tool name must be provider-safe; normalized form is {provider_safe:?}"
            ),
        });
    }
    Ok(())
}

/// `const fn` byte-wise `str::starts_with` — used by `proxima_flavor!`
/// to compile-check schema / tool / trigger prefixes. `str::starts_with`
/// is not `const`, so the comparison is spelled out. See docs/08
/// §Schema namespacing: prefix violations reachable from associated
/// `const`s or literals are now caught at build time, not at `register`.
#[must_use]
pub const fn schema_id_has_prefix(id: &str, prefix: &str) -> bool {
    let (id, prefix) = (id.as_bytes(), prefix.as_bytes());
    if prefix.len() > id.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if id[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn maybe_add_search_projection(
    out: &mut Vec<MemorySearchProjection>,
    schema_info: &SchemaInfo,
    projection: Option<crate::SearchProjection>,
) {
    let Some(projection) = projection else {
        return;
    };
    if projection.fields.is_empty() {
        return;
    }
    let Some(sidecar_table) = schema_info.sidecar_table.clone() else {
        return;
    };
    out.push(MemorySearchProjection {
        schema_id: schema_info.schema_id.clone(),
        schema_version: schema_info.schema_version,
        kind: schema_info.kind,
        sidecar_table,
        fields: projection
            .fields
            .iter()
            .map(|field| MemorySearchProjectionField {
                column: field.column.to_string(),
                kind: field.kind,
            })
            .collect(),
        tag_column: projection.tag_column,
    });
}

pub(crate) fn schema_capability_map(
    bindings: &[SchemaCapabilityTags],
) -> std::collections::HashMap<(SchemaId, SchemaVersion, PayloadKind), BTreeSet<CapabilityTag>> {
    let mut out: std::collections::HashMap<_, BTreeSet<CapabilityTag>> =
        std::collections::HashMap::new();
    for binding in bindings {
        out.entry((
            binding.schema_id.clone(),
            binding.schema_version,
            binding.kind,
        ))
        .or_default()
        .extend(binding.tags.iter().cloned());
    }
    out
}

fn payload_kind_admitted_by_mask(kind: PayloadKind, mask: crate::EntityKindMask) -> bool {
    match kind {
        PayloadKind::Fact => mask.contains_str("Fact"),
        PayloadKind::Abstraction => mask.contains_str("Abstraction"),
        PayloadKind::Perspective => mask.contains_str("Perspective"),
        PayloadKind::Goal => mask.contains_str("Goal"),
        PayloadKind::Edge | PayloadKind::CitedObject | PayloadKind::CitationMapping => false,
    }
}

fn decode_protocol_payload<T>(value: &serde_json::Value) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(value.clone()).map_err(|e| e.to_string())
}

fn ingest_fact_payload<F>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    F: FactPayload + Send + Sync,
{
    let payload = decode_protocol_payload::<F>(value)?;
    let key_bytes = Some(payload.receipt_key());
    let rendered_text = Some(payload.render());
    Ok(ProtocolPayload {
        key_bytes,
        sidecar_payload: SidecarPayload::fact(payload),
        rendered_text,
        content_hash: None,
    })
}

fn ingest_abstraction_payload<A>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    A: AbstractionPayload + Send + Sync,
{
    let payload = decode_protocol_payload::<A>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::abstraction(payload),
        rendered_text: None,
        content_hash: None,
    })
}

fn ingest_perspective_payload<P>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    P: PerspectivePayload + Send + Sync,
{
    let payload = decode_protocol_payload::<P>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::perspective(payload),
        rendered_text: None,
        content_hash: None,
    })
}

fn ingest_goal_payload<G>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    G: GoalPayload,
{
    let payload = decode_protocol_payload::<G>(value)?;
    let key_bytes = Some(payload.goal_key());
    Ok(ProtocolPayload {
        key_bytes,
        sidecar_payload: SidecarPayload::goal(payload),
        rendered_text: None,
        content_hash: None,
    })
}

fn ingest_edge_payload<E>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    E: EdgePayload + Send + Sync,
{
    let payload = decode_protocol_payload::<E>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::edge(payload),
        rendered_text: None,
        content_hash: None,
    })
}

fn ingest_cited_object_payload<C>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    C: CitedObjectPayload,
{
    let payload = decode_protocol_payload::<C>(value)?;
    let content_hash = Some(payload.idempotency_key());
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::cited_object(payload),
        rendered_text: None,
        content_hash,
    })
}

fn ingest_citation_mapping_payload<M>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    M: CitationMappingPayload,
{
    let payload = decode_protocol_payload::<M>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::citation_mapping(payload),
        rendered_text: None,
        content_hash: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpToolCtx, McpToolError};

    #[test]
    fn schema_id_has_prefix_edge_cases() {
        // Normal prefix match — the common case.
        assert!(schema_id_has_prefix("proxima-code/commit", "proxima-code/"));
        // Empty prefix is satisfied by anything.
        assert!(schema_id_has_prefix("abc", ""));
        // Prefix equal to the whole id.
        assert!(schema_id_has_prefix("abc", "abc"));
        // Prefix longer than the id never matches.
        assert!(!schema_id_has_prefix("ab", "abc"));
        // Plain mismatch.
        assert!(!schema_id_has_prefix("wrong/x", "right/"));
        // Truncated prefix — id is a prefix of the prefix, not vice versa.
        assert!(!schema_id_has_prefix("proxima-cod", "proxima-code/"));
        // Multibyte UTF-8: byte-wise comparison must still hold.
        assert!(schema_id_has_prefix("schémä/x", "schémä/"));
        assert!(!schema_id_has_prefix("sch", "schémä/"));
    }

    #[derive(schemars::JsonSchema, serde::Deserialize)]
    struct EmptyDemoArgs {}

    struct Demo;

    impl McpTool for Demo {
        const NAME: &'static str = "proxima-test_demo";
        const DESCRIPTION: &'static str = "test";
        type Args = EmptyDemoArgs;
        type Output = ();

        fn call(
            _ctx: McpToolCtx,
            _args: EmptyDemoArgs,
        ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn add_mcp_tool_lists_descriptor() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
        let frozen = registry.freeze_or_panic_for_tests();
        let descriptors = frozen.list_mcp_tools();
        let names: Vec<_> = descriptors.iter().map(|d| d.name).collect();
        assert!(names.contains(&"proxima-test_demo"));
        let demo = descriptors
            .iter()
            .find(|d| d.name == "proxima-test_demo")
            .expect("demo descriptor");
        assert_eq!(
            demo.origin,
            McpToolOrigin::Flavor("proxima-test".to_string())
        );
    }

    #[test]
    fn freeze_rejects_duplicate_tool_names() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
        registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
        let err = registry.try_freeze().expect_err("duplicate tool must fail");
        assert!(matches!(err, FlavorRegistryError::DuplicateTool { .. }));
    }

    #[test]
    fn freeze_rejects_duplicate_schema_keys() {
        let mut registry = FlavorRegistry::new();
        let schema_id = SchemaId::new("proxima-test/duplicate".to_string());
        registry.add_opaque_schema_or_panic_for_tests(
            schema_id.clone(),
            SchemaVersion::new(1),
            PayloadKind::Fact,
        );
        registry.add_opaque_schema_or_panic_for_tests(
            schema_id,
            SchemaVersion::new(1),
            PayloadKind::Fact,
        );
        let err = registry
            .try_freeze()
            .expect_err("duplicate schema must fail");
        assert!(matches!(err, FlavorRegistryError::DuplicateSchema { .. }));
    }

    #[test]
    fn freeze_rejects_capability_tags_for_unregistered_schema() {
        let mut registry = FlavorRegistry::new();
        registry.add_schema_capability_tags_or_panic_for_tests(
            PayloadKind::Fact,
            SchemaId::new("proxima-test/missing".to_string()),
            SchemaVersion::new(1),
            ["actor"],
        );
        let err = registry
            .try_freeze()
            .expect_err("unregistered capability tag schema must fail");
        assert!(matches!(
            err,
            FlavorRegistryError::UnregisteredSchemaCapabilityTags { .. }
        ));
    }

    #[test]
    fn freeze_rejects_unsatisfiable_required_tag_relation() {
        let mut registry = FlavorRegistry::new();
        registry.add_opaque_schema_or_panic_for_tests(
            SchemaId::new("proxima-test/plain-fact".to_string()),
            SchemaVersion::new(1),
            PayloadKind::Fact,
        );
        registry.add_relation_or_panic_for_tests(
            RelationDescriptor::substrate(
                "proxima-test/requires-actor",
                crate::RelationClass::Structural,
                crate::EndpointBinding::Pin,
                crate::EndpointBinding::Pin,
                crate::EntityKindMask::fact(),
                crate::EntityKindMask::fact(),
                crate::AuthorshipKindMask::external_agent(),
            )
            .with_required_tags(&[], &["actor"]),
        );
        let err = registry
            .try_freeze()
            .expect_err("unsatisfiable required tags must fail");
        assert!(matches!(
            err,
            FlavorRegistryError::UnsatisfiableRelationTags { side: "target", .. }
        ));
    }

    #[test]
    fn freeze_rejects_duplicate_relation_names() {
        let mut registry = FlavorRegistry::new();
        let duplicate_core_relation = core_relation_descriptors()
            .into_iter()
            .next()
            .expect("core relation descriptors are seeded");
        registry.add_relation_or_panic_for_tests(duplicate_core_relation);
        let err = registry
            .try_freeze()
            .expect_err("duplicate relation must fail");
        assert!(matches!(err, FlavorRegistryError::DuplicateRelation { .. }));
    }

    #[test]
    fn add_mcp_tool_rejects_unprefixed_tool_name() {
        struct Bad;

        impl McpTool for Bad {
            const NAME: &'static str = "wrong/demo";
            const DESCRIPTION: &'static str = "x";
            type Args = EmptyDemoArgs;
            type Output = ();

            fn call(
                _ctx: McpToolCtx,
                _args: EmptyDemoArgs,
            ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
                Box::pin(async { Ok(()) })
            }
        }

        let mut registry = FlavorRegistry::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.add_mcp_tool_or_panic_for_tests::<Bad>("proxima-test");
        }));
        assert!(result.is_err(), "must panic on prefix mismatch");
    }

    #[test]
    fn default_registry_includes_all_9_substrate_mcp_tools() {
        let frozen = FlavorRegistry::new().freeze_or_panic_for_tests();
        let names: std::collections::HashSet<_> =
            frozen.list_mcp_tools().iter().map(|d| d.name).collect();
        let expected = [
            "core_search_memories",
            "core_memory_spaces",
            "core_remember",
            "core_record_utterance",
            "core_derive",
            "core_link",
            "core_goal",
            "core_fact",
            "core_membership",
        ];
        for name in expected {
            assert!(names.contains(name), "missing tool {name}");
        }
        assert!(
            !names.contains("core/emit_budget_decision"),
            "retired tool name must not remain registered"
        );
        assert_eq!(names.len(), 9, "exactly 9 substrate tools registered");
        for desc in frozen.list_mcp_tools() {
            assert!(
                matches!(desc.origin, McpToolOrigin::Substrate),
                "default tool {} must be substrate-origin",
                desc.name
            );
        }
    }
}
