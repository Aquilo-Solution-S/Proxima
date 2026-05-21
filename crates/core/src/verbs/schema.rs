//! Schema verb — registry introspection.
//!
//! See docs/14-protocol-surface.md §"Schema" and
//! docs/03-schema-registry.md.

use crate::{
    DependencySatisfactionRule, FlavorDescriptor, McpToolDescriptor, RegisteredRelation,
    RelationDescriptor, SchemaId, SchemaVersion,
};

pub type PayloadValidator = fn(&serde_json::Value) -> Result<(), String>;
pub type PayloadCborEncoder = fn(&serde_json::Value) -> Result<Vec<u8>, String>;

#[derive(Debug, Clone)]
pub(crate) struct PayloadValidatorEntry {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub validate: PayloadValidator,
    pub json_schema: Option<serde_json::Value>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub enum PayloadKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    /// Typed sidecar for an edge row, keyed on `edge_id`. See
    /// `EdgePayload` (docs/03 §EdgePayload) and the relation registry
    /// (docs/02 §"Relation registry").
    Edge,
    CitedObject,
    CitationMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaTombstone {
    pub column: String,
    pub value: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaInfo {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub filter_keys: Vec<String>,
    /// Sidecar table identifier (qualified, e.g. `proxima_code.code_chunk_v1`)
    /// when the payload trait declares one; `None` for `CitedObject` and
    /// `CitationMapping` payloads which don't participate in F/A/P queries.
    pub sidecar_table: Option<String>,
    /// Natural-key columns for stateful Fact schemas (docs/03 §Stateful
    /// Fact schemas). Empty for stateless / non-Fact schemas. Drives the
    /// head-by-natural-key SQL emission in `Query` heads-only mode.
    pub natural_key_columns: Vec<String>,
    /// Build-time tombstone discriminator for stateful Fact schemas.
    pub tombstone: Option<SchemaTombstone>,
    /// Build-time typed encoder for read-path CBOR projection.
    /// Function pointer is process-local only; not serialized on
    /// Schema responses.
    #[serde(skip)]
    #[specta(skip)]
    pub cbor_encoder: Option<PayloadCborEncoder>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaRequest;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaResponse {
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct FlavorRegistryFrozen {
    schemas: Vec<SchemaInfo>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    flavors: Vec<FlavorDescriptor>,
    workspace_runners: Vec<(
        String,
        std::sync::Arc<dyn crate::personality::workspace::WorkspaceRunner>,
    )>,
    workspace_triggers: Vec<String>,
    dependency_satisfaction_rules: Vec<(String, std::sync::Arc<dyn DependencySatisfactionRule>)>,
}

impl FlavorRegistryFrozen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build-time / test-time constructor. The struct stays
    /// immutable on the public surface (no `register` method)
    /// per AGENTS.md invariant 7.
    #[must_use]
    pub fn with_schemas(schemas: Vec<SchemaInfo>) -> Self {
        Self {
            schemas,
            relations: Vec::new(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            flavors: Vec::new(),
            workspace_runners: Vec::new(),
            workspace_triggers: Vec::new(),
            dependency_satisfaction_rules: Vec::new(),
        }
    }

    /// Build-time / test-time constructor that also seeds the
    /// relation registry. Used by `FlavorRegistry::freeze` once
    /// flavors have published their `RelationDescriptor`s.
    #[must_use]
    pub fn with_schemas_and_relations(
        schemas: Vec<SchemaInfo>,
        relations: Vec<RelationDescriptor>,
    ) -> Self {
        Self {
            schemas,
            relations,
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            flavors: Vec::new(),
            workspace_runners: Vec::new(),
            workspace_triggers: Vec::new(),
            dependency_satisfaction_rules: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_schemas_relations_validators(
        schemas: Vec<SchemaInfo>,
        relations: Vec<RelationDescriptor>,
        validators: Vec<PayloadValidatorEntry>,
        mcp_tools: Vec<McpToolDescriptor>,
        flavors: Vec<FlavorDescriptor>,
        workspace_runners: Vec<(
            String,
            std::sync::Arc<dyn crate::personality::workspace::WorkspaceRunner>,
        )>,
        workspace_triggers: Vec<String>,
        dependency_satisfaction_rules: Vec<(
            String,
            std::sync::Arc<dyn DependencySatisfactionRule>,
        )>,
    ) -> Self {
        Self {
            schemas,
            relations,
            validators,
            mcp_tools,
            flavors,
            workspace_runners,
            workspace_triggers,
            dependency_satisfaction_rules,
        }
    }

    #[must_use]
    pub fn with_additional_schemas(
        mut self,
        schemas: impl IntoIterator<Item = SchemaInfo>,
    ) -> Self {
        self.schemas.extend(schemas);
        self
    }

    #[must_use]
    pub fn list_mcp_tools(&self) -> &[McpToolDescriptor] {
        &self.mcp_tools
    }

    #[must_use]
    pub fn payload_json_schema(
        &self,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) -> Option<&serde_json::Value> {
        self.validators
            .iter()
            .find(|entry| {
                entry.schema_id == *schema_id
                    && entry.schema_version == schema_version
                    && entry.kind == kind
            })
            .and_then(|entry| entry.json_schema.as_ref())
    }

    #[must_use]
    pub fn mcp_tool_ids(&self) -> std::collections::HashSet<String> {
        self.mcp_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Lookup the workspace runner registered by the named flavor.
    /// Returns `None` if the flavor either was not linked into the
    /// composite binary or did not declare a workspace runner.
    #[must_use]
    pub fn workspace_runner(
        &self,
        flavor_id: &str,
    ) -> Option<std::sync::Arc<dyn crate::personality::workspace::WorkspaceRunner>> {
        self.workspace_runners
            .iter()
            .find(|(id, _)| id == flavor_id)
            .map(|(_, runner)| runner.clone())
    }

    #[must_use]
    pub fn is_workspace_trigger(&self, schema_id: &str) -> bool {
        self.workspace_triggers.iter().any(|id| id == schema_id)
    }

    #[must_use]
    pub fn dependency_satisfaction_rule(
        &self,
        schema_id: &str,
    ) -> Option<std::sync::Arc<dyn DependencySatisfactionRule>> {
        self.dependency_satisfaction_rules
            .iter()
            .find(|(id, _)| id == schema_id)
            .map(|(_, rule)| rule.clone())
    }

    /// All `FlavorDescriptor`s registered through `proxima_flavor!`.
    /// Order matches macro invocation order.
    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
    }

    /// Lookup a flavor descriptor by its `flavor_id`
    /// (e.g. `"proxima-code"`).
    #[must_use]
    pub fn flavor(&self, flavor_id: &str) -> Option<&FlavorDescriptor> {
        self.flavors.iter().find(|f| f.flavor_id == flavor_id)
    }

    pub fn list(&self) -> Vec<SchemaInfo> {
        self.schemas.clone()
    }

    /// All registered relations. Order matches the order flavors
    /// pushed them in.
    #[must_use]
    pub fn list_relations(&self) -> &[RelationDescriptor] {
        &self.relations
    }

    /// Lookup a `RelationDescriptor` by its flavor-qualified
    /// relation id (`"proxima-code/calls"`, etc.).
    #[must_use]
    pub fn lookup_relation(&self, relation: &str) -> Option<&RelationDescriptor> {
        self.relations.iter().find(|r| r.relation == relation)
    }

    /// Resolve a relation for an edge write. Typed relations also
    /// resolve their registered EdgePayload sidecar table; substrate
    /// relations return `payload_sidecar_table = None`.
    #[must_use]
    pub fn resolve_relation(&self, relation: &str) -> Option<RegisteredRelation<'_>> {
        let descriptor = self.lookup_relation(relation)?;
        let payload_sidecar_table = match &descriptor.payload_schema {
            Some(payload_schema) => Some(
                self.schemas
                    .iter()
                    .find(|s| {
                        s.kind == PayloadKind::Edge
                            && s.schema_id == payload_schema.schema_id
                            && s.schema_version == payload_schema.schema_version
                    })?
                    .sidecar_table
                    .as_deref()?,
            ),
            None => None,
        };
        Some(RegisteredRelation {
            descriptor,
            payload_sidecar_table,
        })
    }

    /// Lookup by `(schema_id, schema_version)`. Used by
    /// EventIngest / GoalWrite to validate incoming payloads.
    #[must_use]
    pub fn lookup(&self, schema_id: &SchemaId, version: SchemaVersion) -> Option<&SchemaInfo> {
        self.schemas
            .iter()
            .find(|s| s.schema_id == *schema_id && s.schema_version == version)
    }

    /// Validate a JSON payload against the build-time registered Rust
    /// payload type when the registry was produced by `FlavorRegistry`.
    /// Ad-hoc test registries may not carry validators; those still
    /// enforce the minimum F/A/P sidecar contract that payloads are
    /// JSON objects before storage casts them into sidecar rows.
    pub fn validate_payload(
        &self,
        schema_id: &SchemaId,
        version: SchemaVersion,
        kind: PayloadKind,
        payload: &serde_json::Value,
    ) -> Result<(), String> {
        if !payload.is_object() {
            return Err("typed payload must be a JSON object".into());
        }

        if let Some(validator) = self
            .validators
            .iter()
            .find(|v| v.schema_id == *schema_id && v.schema_version == version && v.kind == kind)
        {
            (validator.validate)(payload)?;
        }

        Ok(())
    }

    /// Resolve the head-by-natural-key filter for a stateful Fact
    /// schema. Returns `None` when the schema is unknown, is not a
    /// Fact, or has no natural-key columns (stateless Fact). Used by
    /// the engine to populate `QueryRequest::stateful_heads` for
    /// heads-only queries (docs/14 §Query, docs/03 §Stateful Fact
    /// schemas).
    #[must_use]
    pub fn stateful_filter_for(
        &self,
        schema_id: &SchemaId,
    ) -> Option<crate::verbs::query::StatefulHeadsFilter> {
        let info = self
            .schemas
            .iter()
            .find(|s| s.schema_id == *schema_id && s.kind == PayloadKind::Fact)?;
        if info.natural_key_columns.is_empty() {
            return None;
        }
        let sidecar_table = info.sidecar_table.clone()?;
        Some(crate::verbs::query::StatefulHeadsFilter {
            schema_id: info.schema_id.clone(),
            schema_version: info.schema_version,
            sidecar_table,
            natural_key_columns: info.natural_key_columns.clone(),
            tombstone: info.tombstone.clone(),
        })
    }

    #[must_use]
    pub fn stateful_filters_for_schema(
        &self,
        schema_id: &SchemaId,
    ) -> Vec<crate::verbs::query::StatefulHeadsFilter> {
        self.schemas
            .iter()
            .filter(|s| {
                s.schema_id == *schema_id
                    && s.kind == PayloadKind::Fact
                    && !s.natural_key_columns.is_empty()
            })
            .filter_map(|info| {
                Some(crate::verbs::query::StatefulHeadsFilter {
                    schema_id: info.schema_id.clone(),
                    schema_version: info.schema_version,
                    sidecar_table: info.sidecar_table.clone()?,
                    natural_key_columns: info.natural_key_columns.clone(),
                    tombstone: info.tombstone.clone(),
                })
            })
            .collect()
    }

    #[must_use]
    pub fn stateful_filters(&self) -> Vec<crate::verbs::query::StatefulHeadsFilter> {
        self.schemas
            .iter()
            .filter(|s| s.kind == PayloadKind::Fact && !s.natural_key_columns.is_empty())
            .filter_map(|info| {
                Some(crate::verbs::query::StatefulHeadsFilter {
                    schema_id: info.schema_id.clone(),
                    schema_version: info.schema_version,
                    sidecar_table: info.sidecar_table.clone()?,
                    natural_key_columns: info.natural_key_columns.clone(),
                    tombstone: info.tombstone.clone(),
                })
            })
            .collect()
    }

    pub fn handle(&self, _req: &SchemaRequest) -> SchemaResponse {
        SchemaResponse {
            schemas: self.list(),
        }
    }
}
