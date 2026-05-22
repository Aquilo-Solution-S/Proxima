//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::personality::workspace::WorkspaceRunner;
use crate::verbs::schema::{
    FlavorRegistryFrozen, MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
    PayloadValidator, PayloadValidatorEntry, SchemaInfo,
};
use crate::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, DependencySatisfactionRule,
    EdgePayload, FactPayload, GoalPayload, McpCallFn, McpTool, McpToolDescriptor, McpToolError,
    PerspectivePayload, RelationDescriptor, SchemaId, SchemaVersion, core_relation_descriptors,
};

use std::sync::Arc;

/// Structured per-flavor metadata. Populated by `proxima_flavor!` at
/// macro-expansion time so the `package_version` and `author` fields
/// reflect the calling crate's `Cargo.toml`.
///
/// One descriptor per `proxima_flavor!` invocation.
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
    /// `Builtin`; other variants are wire-compatible reserved values,
    /// not a runtime registration contract.
    pub provenance: FlavorProvenance,
}

/// Where the flavor came from. Reserved cases are out-of-scope for
/// v1 and do not imply dynamic flavor loading.
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
    search_projections: Vec<MemorySearchProjection>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    flavors: Vec<FlavorDescriptor>,
    /// Per-flavor workspace runner. Populated by
    /// `proxima_flavor! { workspace_runner = ... }`. Frozen into
    /// `FlavorRegistryFrozen.workspace_runners` and looked up by
    /// `wake/fire.rs` at fire time.
    workspace_runners: Vec<(String, Arc<dyn WorkspaceRunner>)>,
    /// Workspace-eligible trigger schemas. Core treats them as opaque
    /// flavor-qualified schema ids; flavor runners interpret payloads.
    workspace_triggers: Vec<String>,
    dependency_satisfaction_rules: Vec<(String, Arc<dyn DependencySatisfactionRule>)>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            search_projections: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            flavors: Vec::new(),
            workspace_runners: Vec::new(),
            workspace_triggers: Vec::new(),
            dependency_satisfaction_rules: Vec::new(),
        };
        // Substrate-shipped Fact schema for MCP-CRUD audit.
        registry.add_fact_schema::<crate::mcp::core_tools::PersonalityConfigChangedV1>();
        registry.add_fact_schema::<crate::approval::ApprovalPolicyV1>();
        registry.add_fact_schema::<crate::approval::ApprovalVoteV1>();
        registry.add_fact_schema::<crate::approval::ApprovalDecisionV1>();
        registry.add_fact_schema::<crate::intervention::InterventionRequestedV1>();
        registry.add_fact_schema::<crate::intervention::InterventionDecisionV1>();
        registry.add_fact_schema::<crate::chat::ChatStartedV1>();
        registry.add_fact_schema::<crate::chat::ChatMessageV1>();
        registry.add_fact_schema::<crate::chat::ChatReplyV1>();
        registry.add_fact_schema::<crate::chat::ChatEndRequestedV1>();
        registry.add_fact_schema::<crate::chat::ChatEndedV1>();
        registry.add_abstraction_schema::<crate::chat::ChatCompactionV1>();
        registry.add_abstraction_schema::<crate::chat::ChatSummaryV1>();
        registry.add_cited_object_schema::<crate::citations::UploadedBlobPayload>();
        registry.add_fact_schema::<crate::workspace_run::CoreWorkspaceRunV1>();
        // Workspace-run citation nodes: a run cites its output blob
        // (object) and the structural mapping (whole). Both are
        // content-addressed and have no Rust payload type — opaque
        // by construction.
        registry.add_opaque_schema(
            SchemaId::new(crate::workspace_run::CORE_WORKSPACE_RUN_OBJECT_SCHEMA.to_string()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        );
        registry.add_opaque_schema(
            SchemaId::new(crate::workspace_run::CORE_WORKSPACE_RUN_WHOLE_SCHEMA.to_string()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        );
        registry.add_fact_schema::<crate::wake::trace::WakeTracePayload>();
        registry.add_cited_object_schema::<crate::wake::trace::WakeTraceJsonlPayload>();
        registry.add_citation_mapping_schema::<crate::wake::trace::WakeTraceCitationPayload>();
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
    /// optional search projection, the `SchemaInfo`, and the payload
    /// validator entry. Callers build the kind-specific `SchemaInfo`;
    /// `schema_id` / `schema_version` / `kind` for the validator entry
    /// are read back off it so they cannot drift from the schema.
    fn register_schema(
        &mut self,
        schema_info: SchemaInfo,
        search_projection: Option<crate::SearchProjection>,
        validate: PayloadValidator,
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
        self.validators.push(PayloadValidatorEntry {
            schema_id,
            schema_version,
            kind,
            validate,
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
            },
            F::search_projection(),
            validate_payload_type::<F>,
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
                cbor_encoder: Some(encode_payload_cbor::<A>),
            },
            A::search_projection(),
            validate_payload_type::<A>,
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
                cbor_encoder: Some(encode_payload_cbor::<P>),
            },
            P::search_projection(),
            validate_payload_type::<P>,
            P::json_schema(),
        );
    }

    pub fn add_goal_schema<G: GoalPayload>(&mut self) {
        self.register_schema(
            SchemaInfo {
                schema_id: G::schema_id(),
                schema_version: SchemaVersion::new(G::SCHEMA_VERSION),
                kind: PayloadKind::Goal,
                filter_keys: vec![],
                sidecar_table: Some(G::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: Some(encode_payload_cbor::<G>),
            },
            None,
            validate_payload_type::<G>,
            G::json_schema(),
        );
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
                cbor_encoder: Some(encode_payload_cbor::<E>),
            },
            None,
            validate_payload_type::<E>,
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
                cbor_encoder: Some(encode_payload_cbor::<C>),
            },
            None,
            validate_payload_type::<C>,
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
                sidecar_table: Some(M::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: Some(encode_payload_cbor::<M>),
            },
            None,
            validate_payload_type::<M>,
            M::json_schema(),
        );
    }

    /// Register an *opaque* schema — one with no Rust payload type.
    /// The blessed path for content-addressed `CitedObject`s and
    /// structural `CitationMapping`s whose payload is an opaque blob;
    /// it carries no validator, no CBOR encoder, and no JSON schema, so
    /// `validate_payload` accepts any object payload for it.
    ///
    /// This is the *only* sanctioned way to register an untyped schema.
    /// `freeze()` asserts every other schema is fully typed (matching
    /// `cbor_encoder` and validator), so a validator dropped by mistake
    /// fails the build rather than silently disabling validation.
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

    /// Register a flavor's workspace runner. Called by
    /// `proxima_flavor!` once per flavor (at most one runner per
    /// flavor). Duplicate registration for the same flavor_id
    /// panics at freeze time.
    pub fn add_workspace_runner(
        &mut self,
        flavor_id: impl Into<String>,
        runner: Arc<dyn WorkspaceRunner>,
    ) {
        self.workspace_runners.push((flavor_id.into(), runner));
    }

    pub fn replace_workspace_runner(
        &mut self,
        flavor_id: impl Into<String>,
        runner: Arc<dyn WorkspaceRunner>,
    ) {
        let flavor_id = flavor_id.into();
        self.workspace_runners
            .retain(|(existing, _)| existing != &flavor_id);
        self.workspace_runners.push((flavor_id, runner));
    }

    pub fn add_workspace_trigger(&mut self, schema_id: impl Into<String>) {
        self.workspace_triggers.push(schema_id.into());
    }

    pub fn add_dependency_satisfaction_rule(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) {
        self.dependency_satisfaction_rules
            .push((schema_id.into(), rule));
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

    pub fn add_mcp_tool<T: McpTool>(&mut self, expected_prefix: &str) {
        let prefix = format!("{expected_prefix}/");
        assert!(
            T::NAME.starts_with(&prefix),
            "McpTool::NAME {:?} must start with prefix {:?}",
            T::NAME,
            prefix,
        );
        let schema = schemars::schema_for!(T::Args);
        let mut args_schema = serde_json::to_value(schema).expect("JsonSchema must serialize");
        inline_local_schema_refs(&mut args_schema);
        describe_generated_schema_fields(&mut args_schema);
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
            produces_schema_ids: T::PRODUCES_SCHEMA_IDS,
            args_schema,
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
        self.assert_flavor_descriptors();
        // Every schema is either typed (a `cbor_encoder` and a matching
        // validator) or opaque (neither). A typed schema whose validator
        // was dropped would make `validate_payload` silently accept any
        // payload — catch the drift here, not at first write.
        for schema in &self.schemas {
            let has_validator = self.validators.iter().any(|v| {
                v.schema_id == schema.schema_id
                    && v.schema_version == schema.schema_version
                    && v.kind == schema.kind
            });
            assert!(
                schema.cbor_encoder.is_some() == has_validator,
                "schema {:?} v{:?} {:?}: a typed schema needs both a \
                 cbor_encoder and a validator, an opaque schema neither \
                 — found cbor_encoder={}, validator={}",
                schema.schema_id.as_str(),
                schema.schema_version.into_inner(),
                schema.kind,
                schema.cbor_encoder.is_some(),
                has_validator,
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
        // At most one workspace runner per flavor.
        let mut seen_runner_flavors: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (flavor_id, _) in &self.workspace_runners {
            assert!(
                seen_runner_flavors.insert(flavor_id.as_str()),
                "duplicate workspace_runner registration for flavor {flavor_id:?}",
            );
        }
        let mut seen_workspace_triggers: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for schema_id in &self.workspace_triggers {
            assert!(
                seen_workspace_triggers.insert(schema_id.as_str()),
                "duplicate workspace_trigger registration for schema {schema_id:?}",
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
        FlavorRegistryFrozen::with_schemas_relations_validators(
            self.schemas,
            self.search_projections,
            self.relations,
            self.validators,
            self.mcp_tools,
            self.flavors,
            self.workspace_runners,
            self.workspace_triggers,
            self.dependency_satisfaction_rules,
        )
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
    });
}

fn inline_local_schema_refs(schema: &mut serde_json::Value) {
    let defs = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    if defs.is_empty() {
        return;
    }
    inline_local_schema_refs_inner(schema, &defs, 0);
    if let Some(object) = schema.as_object_mut() {
        object.remove("$defs");
    }
}

fn inline_local_schema_refs_inner(
    schema: &mut serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    depth: u8,
) {
    if depth > 32 {
        return;
    }
    match schema {
        serde_json::Value::Object(object) => {
            if let Some(ref_key) = object
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .and_then(local_def_ref_key)
                .map(str::to_string)
                && let Some(def) = defs.get(&ref_key)
            {
                *schema = def.clone();
                inline_local_schema_refs_inner(schema, defs, depth + 1);
                return;
            }
            for (key, value) in object {
                if key != "$defs" {
                    inline_local_schema_refs_inner(value, defs, depth + 1);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                inline_local_schema_refs_inner(item, defs, depth + 1);
            }
        }
        _ => {}
    }
}

fn local_def_ref_key(reference: &str) -> Option<&str> {
    let key = reference.strip_prefix("#/$defs/")?;
    if key.contains("~0") || key.contains("~1") {
        return None;
    }
    Some(key)
}

fn describe_generated_schema_fields(schema: &mut serde_json::Value) {
    let serde_json::Value::Object(object) = schema else {
        return;
    };

    if let Some(properties) = object
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (property_name, property_schema) in properties {
            if property_schema
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|description| description.trim().is_empty())
            {
                let description = match property_name.as_str() {
                    "schema_id" => Some(
                        "Schema discriminator id selecting the typed payload variant for this object.",
                    ),
                    "body" => Some("Typed payload body for the selected schema_id variant."),
                    _ => None,
                };
                if let (Some(description), Some(object)) =
                    (description, property_schema.as_object_mut())
                {
                    object.insert(
                        "description".to_string(),
                        serde_json::Value::String(description.to_string()),
                    );
                }
            }
            describe_generated_schema_fields(property_schema);
        }
    }

    for container_key in ["$defs", "definitions"] {
        if let Some(defs) = object
            .get_mut(container_key)
            .and_then(serde_json::Value::as_object_mut)
        {
            for value in defs.values_mut() {
                describe_generated_schema_fields(value);
            }
        }
    }

    for object_key in ["items", "additionalProperties"] {
        if let Some(value) = object.get_mut(object_key) {
            describe_generated_schema_fields(value);
        }
    }

    for array_key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(values) = object
            .get_mut(array_key)
            .and_then(serde_json::Value::as_array_mut)
        {
            for value in values {
                describe_generated_schema_fields(value);
            }
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
    fn workspace_runner_round_trips_through_freeze() {
        use crate::personality::workspace::{
            WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun,
            WorkspaceRunRecord, WorkspaceRunner, WorkspaceRunnerError,
        };
        use std::sync::Arc;

        #[derive(Debug, Default)]
        struct Probe;
        #[async_trait::async_trait]
        impl WorkspaceRunner for Probe {
            async fn prepare(
                &self,
                _input: WorkspacePrepareInput<'_>,
            ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
                Err(WorkspaceRunnerError::Unimplemented)
            }
            async fn finalize(
                &self,
                _input: WorkspaceFinalizeInput<'_>,
            ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
                Err(WorkspaceRunnerError::Unimplemented)
            }
        }

        let mut registry = FlavorRegistry::new();
        registry.add_workspace_runner("probe-flavor", Arc::new(Probe));
        let frozen = registry.freeze();

        assert!(
            frozen.workspace_runner("probe-flavor").is_some(),
            "freeze should preserve registered runner",
        );
        assert!(
            frozen.workspace_runner("missing-flavor").is_none(),
            "missing flavor returns None",
        );
    }

    #[test]
    fn proxima_flavor_macro_registers_workspace_runner() {
        // Inline macro invocation under a fixture module. No schemas
        // means no per-schema prefix checks fire -- minimal test
        // surface that exercises only the workspace_runner arm.
        mod fixture {
            use crate::personality::workspace::{
                WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun,
                WorkspaceRunRecord, WorkspaceRunner, WorkspaceRunnerError,
            };

            #[derive(Debug, Default)]
            struct StubRunner;
            #[async_trait::async_trait]
            impl WorkspaceRunner for StubRunner {
                async fn prepare(
                    &self,
                    _input: WorkspacePrepareInput<'_>,
                ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
                    Err(WorkspaceRunnerError::Unimplemented)
                }
                async fn finalize(
                    &self,
                    _input: WorkspaceFinalizeInput<'_>,
                ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
                    Err(WorkspaceRunnerError::Unimplemented)
                }
            }

            crate::proxima_flavor! {
                name = "macro-test-flavor",
                workspace_runner = StubRunner,
            }
        }

        let mut registry = FlavorRegistry::new();
        fixture::register(&mut registry);
        let frozen = registry.freeze();

        assert!(frozen.workspace_runner("macro-test-flavor").is_some());
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
    fn default_registry_includes_all_41_substrate_mcp_tools() {
        let frozen = FlavorRegistry::new().freeze();
        let names: std::collections::HashSet<_> =
            frozen.list_mcp_tools().iter().map(|d| d.name).collect();
        let expected = [
            "core/list_personalities",
            "core/get_personality",
            "core/get_graph",
            "core/instantiate_personality",
            "core/tombstone_personality",
            "core/list_wake_entries",
            "core/list_wake_invocations",
            "core/set_wake_entries",
            "core/list_read_scope",
            "core/set_read_scope",
            "core/add_wake_entry",
            "core/update_wake_entry",
            "core/remove_wake_entry",
            "core/replay_wake_events",
            "core/list_inference_targets",
            "core/list_inference_tier_bindings",
            "core/register_inference_target",
            "core/remove_inference_target",
            "core/bind_inference_tier",
            "core/list_embedding_models",
            "core/get_embedding_active",
            "core/register_embedding_model",
            "core/delete_embedding_model",
            "core/set_embedding_active",
            "core/clear_embedding_active",
            "core/list_substrate_tools",
            "core/list_workspace_tools",
            "core/list_schemas",
            "core/list_edge_types",
            "core/emit_approval_policy",
            "core/emit_approval_vote",
            "core/try_emit_approval_decision",
            "core/emit_intervention_decision",
            "core/list_chat_targets",
            "core/get_chat_thread",
            "core/start_chat",
            "core/emit_chat_message",
            "core/emit_chat_reply",
            "core/compact_chat_thread",
            "core/request_end_chat",
            "core/end_chat",
        ];
        for name in expected {
            assert!(names.contains(name), "missing tool {name}");
        }
        assert!(
            !names.contains("core/emit_budget_decision"),
            "old intervention tool name must not remain registered"
        );
        assert_eq!(names.len(), 41, "exactly 41 substrate tools registered");
    }
}
