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
    RequestBehavior, SchemaId, SchemaVersion, ScopeGateBehavior, SidecarPayload,
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
        // Substrate-shipped Fact schema for MCP-CRUD audit.
        registry.add_fact_schema::<crate::mcp::core_tools::PersonalityConfigChangedV1>();
        registry.add_cited_object_schema::<crate::citations::UploadedBlobPayload>();
        registry.add_fact_schema::<crate::verbs::persist_mcp_call::McpCallLoggedV1>();
        registry.add_cited_object_schema::<crate::verbs::persist_mcp_call::McpCallIoV1>();
        registry
            .add_citation_mapping_schema::<crate::verbs::persist_mcp_call::McpCallIoCitationV1>();
        crate::memory::register_all(&mut registry);
        crate::goal::register_all(&mut registry);
        crate::mcp::core_tools::register_all(&mut registry);
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

    pub fn add_fact_schema<F: FactPayload>(&mut self) {
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
    }

    pub fn add_abstraction_schema<A: AbstractionPayload>(&mut self) {
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
    }

    pub fn add_perspective_schema<P: PerspectivePayload>(&mut self) {
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
    }

    pub fn add_goal_schema<G: GoalPayload>(&mut self) {
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
    }

    /// Register a typed `EdgePayload` schema. The descriptor that
    /// references this schema must be registered separately via
    /// `add_relation`; the substrate cross-checks the linkage at
    /// `freeze()` time.
    pub fn add_edge_schema<E: EdgePayload>(&mut self) {
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
    }

    pub fn add_cited_object_schema<C: CitedObjectPayload>(&mut self) {
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
    }

    pub fn add_citation_mapping_schema<M: CitationMappingPayload>(&mut self) {
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
    pub fn add_opaque_schema(
        &mut self,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) {
        self.schemas
            .push(SchemaInfo::opaque(schema_id, schema_version, kind));
    }

    /// Register a relation. Substrate-only relations carry no
    /// `payload_schema`; typed relations point at a registered
    /// `EdgePayload` schema.
    pub fn add_relation(&mut self, descriptor: RelationDescriptor) {
        self.relations.push(descriptor);
    }

    /// Attach opaque capability tags to a registered payload schema.
    ///
    /// # Panics
    ///
    /// Panics if any tag fails [`CapabilityTag::parse`]. The schema
    /// existence check runs at [`Self::freeze`], after every flavor has
    /// registered its schemas.
    pub fn add_schema_capability_tags<'a>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        version: SchemaVersion,
        tags: impl IntoIterator<Item = &'a str>,
    ) {
        let tags = tags
            .into_iter()
            .map(|tag| {
                CapabilityTag::parse(tag).unwrap_or_else(|err| {
                    panic!(
                        "schema {} v{} {:?} has invalid capability tag: {err}",
                        schema_id.as_str(),
                        version.into_inner(),
                        kind,
                    )
                })
            })
            .collect();
        self.schema_capability_tags.push(SchemaCapabilityTags {
            schema_id,
            schema_version: version,
            kind,
            tags,
        });
    }

    pub fn add_dependency_satisfaction_rule(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) {
        self.dependency_satisfaction_rules
            .push((schema_id.into(), rule));
    }

    /// Register the composed app's owner resolver.
    ///
    /// # Panics
    ///
    /// Panics if a resolver is already registered. Composition permits at most
    /// one owner resolver.
    pub fn set_owner_resolver(&mut self, resolver: Arc<dyn OwnerResolver>) {
        assert!(
            self.owner_resolver.is_none(),
            "duplicate OwnerResolver registered"
        );
        self.owner_resolver = Some(resolver);
    }

    pub fn add_authorization_hook(&mut self, hook: Arc<dyn AuthorizationHook>) {
        self.authorization_hooks.push(hook);
    }

    pub fn add_request_behavior(&mut self, behavior: impl RequestBehavior + 'static) {
        self.request_behaviors.push(Arc::new(behavior));
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

    /// Register a flavor-shipped MCP tool under `expected_prefix`.
    ///
    /// # Panics
    ///
    /// Panics if `T::NAME` starts with neither `"<expected_prefix>/"` nor
    /// `"<expected_prefix>_"`. (Tool wire names may use `_` to stay valid under
    /// Anthropic's MCP tool-name rule; schema ids still require `/`.)
    pub fn add_mcp_tool<T: McpTool>(&mut self, expected_prefix: &str) {
        let slash = format!("{expected_prefix}/");
        let under = format!("{expected_prefix}_");
        assert!(
            T::NAME.starts_with(&slash) || T::NAME.starts_with(&under),
            "McpTool::NAME {:?} must start with prefix {:?} or {:?}",
            T::NAME,
            slash,
            under,
        );
        debug_assert_eq!(
            T::NAME,
            crate::mcp::provider_safe_tool_name(T::NAME),
            "McpTool::NAME {:?} must already be provider-safe",
            T::NAME,
        );
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
    }

    /// Register a substrate-shipped MCP tool. Asserts the name starts
    /// with `"core/"` (no flavor prefix). Used in `Default::default()`
    /// to wire the personality-config-CRUD tools into every composite
    /// binary. Same registration path as `add_mcp_tool`, pinned to the
    /// `core` prefix.
    pub(crate) fn add_substrate_mcp_tool<T: McpTool>(&mut self) {
        self.add_mcp_tool::<T>("core");
    }

    /// Validate the registry and seal it for runtime use.
    ///
    /// # Panics
    ///
    /// Panics on registration inconsistencies caught at startup: invalid
    /// relation masks, relations referencing an unregistered or
    /// wrong-class `EdgePayload` schema, and duplicate ids across
    /// registered flavors/schemas/tools.
    #[must_use]
    pub fn freeze(self) -> FlavorRegistryFrozen {
        // Cross-check: every typed relation's payload_schema must
        // point at a registered Edge schema with the matching
        // RelationClass. Catches authoring drift at startup, not
        // at first edge-write.
        for rel in &self.relations {
            rel.validate_descriptor().unwrap_or_else(|err| {
                panic!(
                    "RelationDescriptor {:?} has invalid masks: {err}",
                    rel.relation
                )
            });
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
        self.assert_schema_capability_tags_resolve();
        self.assert_required_relation_tags_satisfiable();
        self.assert_flavor_descriptors();
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
            assert!(
                schema.has_typed_ingress == has_ingress,
                "schema {:?} v{:?} {:?}: a typed schema needs a \
                 protocol-ingress parser, an opaque schema has none \
                 - found has_typed_ingress={}, ingress_entry={}",
                schema.schema_id.as_str(),
                schema.schema_version.into_inner(),
                schema.kind,
                schema.has_typed_ingress,
                has_ingress,
            );
        }
        let mut seen_schemas = std::collections::HashSet::new();
        for schema in &self.schemas {
            assert!(
                seen_schemas.insert((schema.schema_id.clone(), schema.schema_version, schema.kind)),
                "duplicate schema registered: {:?} v{:?} {:?}",
                schema.schema_id.as_str(),
                schema.schema_version.into_inner(),
                schema.kind,
            );
        }
        let mut seen_relations = std::collections::HashSet::new();
        for rel in &self.relations {
            assert!(
                seen_relations.insert(rel.relation.clone()),
                "duplicate RelationDescriptor registered: {:?}",
                rel.relation,
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
        let mut seen_dependency_rules: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (schema_id, _) in &self.dependency_satisfaction_rules {
            assert!(
                seen_dependency_rules.insert(schema_id.as_str()),
                "duplicate dependency satisfaction rule for schema {schema_id:?}",
            );
        }
        FlavorRegistryFrozen::from_registry(self)
    }

    fn assert_schema_capability_tags_resolve(&self) {
        for binding in &self.schema_capability_tags {
            assert!(
                self.schemas.iter().any(|schema| {
                    schema.schema_id == binding.schema_id
                        && schema.schema_version == binding.schema_version
                        && schema.kind == binding.kind
                }),
                "schema capability tags reference unregistered schema: {:?} v{:?} {:?}",
                binding.schema_id.as_str(),
                binding.schema_version.into_inner(),
                binding.kind,
            );
        }
    }

    fn assert_required_relation_tags_satisfiable(&self) {
        let declared = schema_capability_map(&self.schema_capability_tags);
        for relation in &self.relations {
            self.assert_relation_side_tags_satisfiable(
                relation,
                "source",
                relation.source_kind_mask,
                &relation.source_required_tags,
                &declared,
            );
            self.assert_relation_side_tags_satisfiable(
                relation,
                "target",
                relation.target_kind_mask,
                &relation.target_required_tags,
                &declared,
            );
        }
    }

    fn assert_relation_side_tags_satisfiable(
        &self,
        relation: &RelationDescriptor,
        side: &str,
        kind_mask: crate::EntityKindMask,
        required_tags: &BTreeSet<CapabilityTag>,
        declared: &std::collections::HashMap<
            (SchemaId, SchemaVersion, PayloadKind),
            BTreeSet<CapabilityTag>,
        >,
    ) {
        if required_tags.is_empty() {
            return;
        }
        let admitted = self.schemas.iter().any(|schema| {
            payload_kind_admitted_by_mask(schema.kind, kind_mask)
                && declared
                    .get(&(schema.schema_id.clone(), schema.schema_version, schema.kind))
                    .is_some_and(|tags| required_tags.is_subset(tags))
        });
        assert!(
            admitted,
            "RelationDescriptor {:?} has unsatisfiable {side} required capability tags",
            relation.relation,
        );
    }

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique.
    fn assert_flavor_descriptors(&self) {
        let mut seen_ids = std::collections::HashSet::new();
        for flavor in &self.flavors {
            assert!(
                seen_ids.insert(flavor.flavor_id.as_str()),
                "duplicate FlavorDescriptor flavor_id registered: {}",
                flavor.flavor_id,
            );
        }
    }
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
    let key_bytes = Some(payload.event_key());
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
        registry.add_mcp_tool::<Demo>("proxima-test");
        let frozen = registry.freeze();
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
        registry.add_mcp_tool::<Demo>("proxima-test");
        registry.add_mcp_tool::<Demo>("proxima-test");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.freeze()));
        assert!(result.is_err(), "freeze must panic on duplicate tool names");
    }

    #[test]
    #[should_panic(expected = "duplicate schema registered")]
    fn freeze_rejects_duplicate_schema_keys() {
        let mut registry = FlavorRegistry::new();
        let schema_id = SchemaId::new("proxima-test/duplicate".to_string());
        registry.add_opaque_schema(schema_id.clone(), SchemaVersion::new(1), PayloadKind::Fact);
        registry.add_opaque_schema(schema_id, SchemaVersion::new(1), PayloadKind::Fact);
        let _ = registry.freeze();
    }

    #[test]
    #[should_panic(expected = "schema capability tags reference unregistered schema")]
    fn freeze_rejects_capability_tags_for_unregistered_schema() {
        let mut registry = FlavorRegistry::new();
        registry.add_schema_capability_tags(
            PayloadKind::Fact,
            SchemaId::new("proxima-test/missing".to_string()),
            SchemaVersion::new(1),
            ["actor"],
        );
        let _ = registry.freeze();
    }

    #[test]
    #[should_panic(expected = "unsatisfiable target required capability tags")]
    fn freeze_rejects_unsatisfiable_required_tag_relation() {
        let mut registry = FlavorRegistry::new();
        registry.add_opaque_schema(
            SchemaId::new("proxima-test/plain-fact".to_string()),
            SchemaVersion::new(1),
            PayloadKind::Fact,
        );
        registry.add_relation(
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
        let _ = registry.freeze();
    }

    #[test]
    #[should_panic(expected = "duplicate RelationDescriptor registered")]
    fn freeze_rejects_duplicate_relation_names() {
        let mut registry = FlavorRegistry::new();
        let duplicate_core_relation = core_relation_descriptors()
            .into_iter()
            .next()
            .expect("core relation descriptors are seeded");
        registry.add_relation(duplicate_core_relation);
        let _ = registry.freeze();
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
            registry.add_mcp_tool::<Bad>("proxima-test");
        }));
        assert!(result.is_err(), "must panic on prefix mismatch");
    }

    #[test]
    fn default_registry_includes_personality_config_changed_schema() {
        let frozen = FlavorRegistry::new().freeze();
        let info = frozen.lookup(
            &crate::SchemaId::new("core/personality_config_changed_v1".into()),
            crate::SchemaVersion::new(1),
        );
        assert!(
            info.is_some(),
            "schema must be registered in default registry"
        );
        assert_eq!(info.unwrap().kind, PayloadKind::Fact);
    }

    #[test]
    fn default_registry_includes_all_14_substrate_mcp_tools() {
        let frozen = FlavorRegistry::new().freeze();
        let names: std::collections::HashSet<_> =
            frozen.list_mcp_tools().iter().map(|d| d.name).collect();
        let expected = [
            "core_search_memories",
            "core_memory_spaces",
            "core_publish_memory",
            "core_remember",
            "core_record_utterance",
            "core_derive",
            "core_link",
            "core_memory",
            "core_space",
            "core_marketplace",
            "core_goal",
            "core_wake",
            "core_personality",
            "core_fact",
        ];
        for name in expected {
            assert!(names.contains(name), "missing tool {name}");
        }
        assert!(
            !names.contains("core/emit_budget_decision"),
            "retired tool name must not remain registered"
        );
        assert_eq!(names.len(), 14, "exactly 14 substrate tools registered");
        for desc in frozen.list_mcp_tools() {
            assert!(
                matches!(desc.origin, McpToolOrigin::Substrate),
                "default tool {} must be substrate-origin",
                desc.name
            );
        }
    }
}
