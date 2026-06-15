//! Schema verb — registry introspection.
//!
//! See docs/14-protocol-surface.md §"Schema" and
//! docs/03-schema-registry.md.

use crate::{
    DependencySatisfactionRule, FlavorDescriptor, McpToolDescriptor, RegisteredRelation,
    RelationDescriptor, SchemaId, SchemaVersion, SearchProjectionColumnKind, StorageError,
};
use futures::future::BoxFuture;
use sqlx::{Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;

pub type PayloadValidator = fn(&serde_json::Value) -> Result<(), String>;
pub type PayloadJsonEncoder = fn(&serde_json::Value) -> Result<Vec<u8>, String>;
pub type CitedObjectContentHasher = fn(&[u8]) -> Result<[u8; 32], StorageError>;
pub type FactRenderer = fn(&[u8]) -> Result<String, StorageError>;
pub type SidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    Uuid,
    &'t [u8],
) -> BoxFuture<'t, Result<(), StorageError>>;

#[derive(Debug, Clone)]
pub(crate) struct PayloadValidatorEntry {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub validate: PayloadValidator,
    pub json_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct CitedObjectContentHasherEntry {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub hash: CitedObjectContentHasher,
}

#[derive(Debug, Clone)]
pub(crate) struct FactRendererEntry {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub render: FactRenderer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PayloadKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    /// Typed sidecar for an edge row, keyed on `edge_id`. See
    /// `EdgePayload` (docs/03 §`EdgePayload`) and the relation registry
    /// (docs/02 §"Relation registry").
    Edge,
    CitedObject,
    CitationMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchemaTombstone {
    pub column: String,
    pub value: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaInfo {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub filter_keys: Vec<String>,
    /// Sidecar table identifier (qualified, e.g. `proxima_code.code_chunk_v1`)
    /// when the payload trait declares one, including typed `CitedObject` and
    /// `CitationMapping` sidecars. `None` only for opaque schemas.
    pub sidecar_table: Option<String>,
    /// Natural-key columns for stateful Fact schemas (docs/03 §Stateful
    /// Fact schemas). Empty for stateless / non-Fact schemas. Drives the
    /// head-by-natural-key SQL emission in `Query` heads-only mode.
    pub natural_key_columns: Vec<String>,
    /// Build-time tombstone discriminator for stateful Fact schemas.
    pub tombstone: Option<SchemaTombstone>,
    /// Build-time typed encoder for read-path canonical JSON projection.
    /// Function pointer is process-local only; not serialized on
    /// Schema responses.
    #[serde(skip)]
    pub json_encoder: Option<PayloadJsonEncoder>,
    /// Build-time typed sidecar inserter for inline citation writes.
    /// Function pointer is process-local only; not serialized on
    /// Schema responses.
    #[serde(skip)]
    pub sidecar_inserter: Option<SidecarInserter>,
    /// `CitedObjectPayload` schema id accepted by a `CitationMappingPayload`.
    /// Populated only for citation-mapping schemas.
    pub cited_object_schema: Option<SchemaId>,
}

impl SchemaInfo {
    /// Construct an *opaque* schema — one with no Rust payload type.
    /// Used for content-addressed `CitedObject`s and structural
    /// `CitationMapping`s whose payload is an opaque blob addressed by
    /// content hash. An opaque schema carries no validator, no JSON
    /// encoder, no JSON schema, and no sidecar table.
    ///
    /// `json_encoder.is_none()` is the typed/opaque discriminant the
    /// registry enforces: `FlavorRegistry::freeze` asserts every schema
    /// either has both a `json_encoder` and a validator, or neither.
    /// See docs/03 §Registry rules.
    #[must_use]
    pub fn opaque(schema_id: SchemaId, schema_version: SchemaVersion, kind: PayloadKind) -> Self {
        Self {
            schema_id,
            schema_version,
            kind,
            filter_keys: Vec::new(),
            sidecar_table: None,
            natural_key_columns: Vec::new(),
            tombstone: None,
            json_encoder: None,
            sidecar_inserter: None,
            cited_object_schema: None,
        }
    }
}

#[must_use]
pub fn sidecar_tables(schemas: &[SchemaInfo], kind: PayloadKind) -> Vec<String> {
    let mut tables = schemas
        .iter()
        .filter(|schema| schema.kind == kind)
        .filter_map(|schema| schema.sidecar_table.clone())
        .collect::<Vec<_>>();
    tables.sort();
    tables.dedup();
    tables
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemorySearchProjectionField {
    pub column: String,
    pub kind: SearchProjectionColumnKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySearchProjection {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub sidecar_table: String,
    pub fields: Vec<MemorySearchProjectionField>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SchemaRequest;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaResponse {
    pub schemas: Vec<SchemaInfo>,
}

/// Build-time lookup acceleration for `FlavorRegistryFrozen`. Each map
/// stores the position of the *first* matching entry in the owning
/// `Vec`, mirroring the `.iter().find()` first-wins semantics of the
/// linear scans it replaces. The frozen `Vec`s are append-only, so a
/// stored index never goes stale; `with_additional_schemas` rebuilds
/// the whole index after extending `schemas`.
///
/// Only the collections that scale with *schema* count and sit on the
/// `EventIngest` / `GoalWrite` / edge-write paths are indexed. `flavors`
/// and `dependency_satisfaction_rules` scale with *flavor* count (bounded
/// by linked crates) and stay linear scans — indexing a handful of
/// entries would not earn its keep.
#[derive(Debug, Clone, Default)]
struct FrozenIndex {
    /// `schemas` keyed by `(schema_id, version)`, kind-agnostic.
    schema_by_id_version: HashMap<(SchemaId, SchemaVersion), usize>,
    /// `schemas` keyed by `(schema_id, version, kind)` — required
    /// because one payload type may register across F/A/P layers.
    schema_by_id_version_kind: HashMap<(SchemaId, SchemaVersion, PayloadKind), usize>,
    /// `validators` keyed by `(schema_id, version, kind)`.
    validator_by_key: HashMap<(SchemaId, SchemaVersion, PayloadKind), usize>,
    /// Cited-object content hashers keyed by `(schema_id, version)`.
    cited_object_content_hasher_by_key: HashMap<(SchemaId, SchemaVersion), usize>,
    /// Fact payload renderers keyed by `(schema_id, version)`.
    fact_renderer_by_key: HashMap<(SchemaId, SchemaVersion), usize>,
    /// `relations` keyed by relation id.
    relation_by_name: HashMap<String, usize>,
}

impl FrozenIndex {
    fn build(
        schemas: &[SchemaInfo],
        validators: &[PayloadValidatorEntry],
        cited_object_content_hashers: &[CitedObjectContentHasherEntry],
        fact_renderers: &[FactRendererEntry],
        relations: &[RelationDescriptor],
    ) -> Self {
        let mut index = Self::default();
        for (position, schema) in schemas.iter().enumerate() {
            index
                .schema_by_id_version
                .entry((schema.schema_id.clone(), schema.schema_version))
                .or_insert(position);
            index
                .schema_by_id_version_kind
                .entry((schema.schema_id.clone(), schema.schema_version, schema.kind))
                .or_insert(position);
        }
        for (position, validator) in validators.iter().enumerate() {
            index
                .validator_by_key
                .entry((
                    validator.schema_id.clone(),
                    validator.schema_version,
                    validator.kind,
                ))
                .or_insert(position);
        }
        for (position, hasher) in cited_object_content_hashers.iter().enumerate() {
            index
                .cited_object_content_hasher_by_key
                .entry((hasher.schema_id.clone(), hasher.schema_version))
                .or_insert(position);
        }
        for (position, renderer) in fact_renderers.iter().enumerate() {
            index
                .fact_renderer_by_key
                .entry((renderer.schema_id.clone(), renderer.schema_version))
                .or_insert(position);
        }
        for (position, relation) in relations.iter().enumerate() {
            index
                .relation_by_name
                .entry(relation.relation.clone())
                .or_insert(position);
        }
        index
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlavorRegistryFrozen {
    schemas: Vec<SchemaInfo>,
    search_projections: Vec<MemorySearchProjection>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
    cited_object_content_hashers: Vec<CitedObjectContentHasherEntry>,
    fact_renderers: Vec<FactRendererEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    flavors: Vec<FlavorDescriptor>,
    dependency_satisfaction_rules: Vec<(String, std::sync::Arc<dyn DependencySatisfactionRule>)>,
    /// Lookup acceleration, rebuilt by every constructor. Not part of
    /// the logical registry — derived purely from the `Vec`s above.
    index: FrozenIndex,
}

impl FlavorRegistryFrozen {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build-time / test-time constructor. The struct stays
    /// immutable on the public surface (no `register` method)
    /// per AGENTS.md invariant 7. `..Self::default()` leaves every
    /// vocabulary field beyond `schemas` empty.
    #[must_use]
    pub fn with_schemas(schemas: Vec<SchemaInfo>) -> Self {
        let index = FrozenIndex::build(&schemas, &[], &[], &[], &[]);
        Self {
            schemas,
            index,
            ..Self::default()
        }
    }

    /// Build-time / test-time constructor that also seeds the
    /// relation registry.
    #[must_use]
    pub fn with_schemas_and_relations(
        schemas: Vec<SchemaInfo>,
        relations: Vec<RelationDescriptor>,
    ) -> Self {
        let index = FrozenIndex::build(&schemas, &[], &[], &[], &relations);
        Self {
            schemas,
            relations,
            index,
            ..Self::default()
        }
    }

    /// Freeze a `FlavorRegistry` into its immutable, index-accelerated
    /// form — the production constructor, called by
    /// `FlavorRegistry::freeze`. Consumes the builder whole and rehomes
    /// every vocabulary field. A new vocabulary kind is added in the
    /// two struct definitions plus the destructure below, which the
    /// compiler keeps exhaustive; the constructor signature never
    /// widens.
    pub(crate) fn from_registry(registry: crate::FlavorRegistry) -> Self {
        let crate::FlavorRegistry {
            schemas,
            search_projections,
            relations,
            validators,
            cited_object_content_hashers,
            fact_renderers,
            mcp_tools,
            flavors,
            dependency_satisfaction_rules,
        } = registry;
        let index = FrozenIndex::build(
            &schemas,
            &validators,
            &cited_object_content_hashers,
            &fact_renderers,
            &relations,
        );
        Self {
            schemas,
            search_projections,
            relations,
            validators,
            cited_object_content_hashers,
            fact_renderers,
            mcp_tools,
            flavors,
            dependency_satisfaction_rules,
            index,
        }
    }

    /// Append opaque schemas to an already-frozen registry.
    ///
    /// # Panics
    ///
    /// Panics if any added schema carries a `json_encoder` — typed
    /// schemas must be registered through `FlavorRegistry` before
    /// `freeze()`.
    #[must_use]
    pub fn with_additional_schemas(
        mut self,
        schemas: impl IntoIterator<Item = SchemaInfo>,
    ) -> Self {
        // This post-freeze path provides no way to attach a validator,
        // so it accepts only opaque schemas — a typed schema added here
        // would be silently unvalidated. Typed schemas go through
        // `FlavorRegistry` before `freeze()`.
        let added: Vec<SchemaInfo> = schemas.into_iter().collect();
        for schema in &added {
            assert!(
                schema.json_encoder.is_none() && schema.sidecar_inserter.is_none(),
                "with_additional_schemas accepts only opaque schemas; \
                 {:?} carries typed process-local functions — register typed schemas \
                 through FlavorRegistry before freeze()",
                schema.schema_id.as_str(),
            );
        }
        self.schemas.extend(added);
        // Rebuild the index over the extended schema list — a stale
        // index would silently miss the appended schemas.
        self.index = FrozenIndex::build(
            &self.schemas,
            &self.validators,
            &self.cited_object_content_hashers,
            &self.fact_renderers,
            &self.relations,
        );
        self
    }

    #[must_use]
    pub fn search_projections(&self) -> &[MemorySearchProjection] {
        &self.search_projections
    }

    #[must_use]
    pub fn schemas(&self) -> &[SchemaInfo] {
        &self.schemas
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
        let position =
            *self
                .index
                .validator_by_key
                .get(&(schema_id.clone(), schema_version, kind))?;
        self.validators[position].json_schema.as_ref()
    }

    #[must_use]
    pub fn mcp_tool_ids(&self) -> std::collections::HashSet<String> {
        self.mcp_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
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

    #[must_use]
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
        let position = *self.index.relation_by_name.get(relation)?;
        Some(&self.relations[position])
    }

    /// Resolve a relation for an edge write. Typed relations also
    /// resolve their registered `EdgePayload` sidecar table; substrate
    /// relations return `payload_sidecar_table = None`.
    #[must_use]
    pub fn resolve_relation(&self, relation: &str) -> Option<RegisteredRelation<'_>> {
        let descriptor = self.lookup_relation(relation)?;
        let payload_sidecar_table = match &descriptor.payload_schema {
            Some(payload_schema) => {
                let position = *self.index.schema_by_id_version_kind.get(&(
                    payload_schema.schema_id.clone(),
                    payload_schema.schema_version,
                    PayloadKind::Edge,
                ))?;
                Some(self.schemas[position].sidecar_table.as_deref()?)
            }
            None => None,
        };
        Some(RegisteredRelation {
            descriptor,
            payload_sidecar_table,
        })
    }

    /// Lookup by `(schema_id, schema_version)`. Used by
    /// `EventIngest` / `GoalWrite` to validate incoming payloads.
    #[must_use]
    pub fn lookup(&self, schema_id: &SchemaId, version: SchemaVersion) -> Option<&SchemaInfo> {
        let position = *self
            .index
            .schema_by_id_version
            .get(&(schema_id.clone(), version))?;
        Some(&self.schemas[position])
    }

    /// Lookup by `(schema_id, schema_version, kind)`. Required when one
    /// typed payload is registered for multiple F/A/P layers.
    #[must_use]
    pub fn lookup_payload(
        &self,
        schema_id: &SchemaId,
        version: SchemaVersion,
        kind: PayloadKind,
    ) -> Option<&SchemaInfo> {
        let position =
            *self
                .index
                .schema_by_id_version_kind
                .get(&(schema_id.clone(), version, kind))?;
        Some(&self.schemas[position])
    }

    /// Validate a JSON payload against the build-time registered Rust
    /// payload type when the registry was produced by `FlavorRegistry`.
    /// Ad-hoc test registries may not carry validators; those still
    /// enforce the minimum F/A/P sidecar contract that payloads are
    /// JSON objects before storage casts them into sidecar rows.
    ///
    /// # Errors
    ///
    /// Returns an error string when `payload` is not a JSON object or
    /// when the registered validator rejects it.
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

        if let Some(&position) =
            self.index
                .validator_by_key
                .get(&(schema_id.clone(), version, kind))
        {
            (self.validators[position].validate)(payload)?;
        }

        Ok(())
    }

    /// Compute a typed cited-object content hash through the frozen
    /// registry.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` when the schema is unknown, is
    /// not registered as `PayloadKind::CitedObject`, or has no typed
    /// content hasher.
    pub fn content_hash_for(
        &self,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        payload_bytes: &[u8],
    ) -> Result<[u8; 32], StorageError> {
        let Some(&position) = self
            .index
            .cited_object_content_hasher_by_key
            .get(&(schema_id.clone(), schema_version))
        else {
            let reason = if self
                .lookup_payload(schema_id, schema_version, PayloadKind::CitedObject)
                .is_some()
            {
                "has no typed content hasher"
            } else {
                "is not a registered cited-object schema"
            };
            return Err(StorageError::Internal(format!(
                "cited object schema {} v{} {reason}",
                schema_id.as_str(),
                schema_version.into_inner(),
            )));
        };
        (self.cited_object_content_hashers[position].hash)(payload_bytes)
    }

    /// Render typed Fact payload bytes through the build-time registered
    /// `FactPayload::render` implementation when one exists.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` when the schema is unknown, is
    /// not registered as `PayloadKind::Fact`, or the payload bytes fail
    /// to decode as the registered JSON type.
    pub fn try_render_fact_payload(
        &self,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        payload_bytes: &[u8],
    ) -> Result<Option<String>, StorageError> {
        let Some(&position) = self
            .index
            .fact_renderer_by_key
            .get(&(schema_id.clone(), schema_version))
        else {
            if self
                .lookup_payload(schema_id, schema_version, PayloadKind::Fact)
                .is_some()
            {
                return Ok(None);
            }
            return Err(StorageError::Internal(format!(
                "Fact schema {} v{} is not a registered Fact schema",
                schema_id.as_str(),
                schema_version.into_inner(),
            )));
        };
        (self.fact_renderers[position].render)(payload_bytes).map(Some)
    }

    /// Render typed Fact payload bytes through the build-time registered
    /// `FactPayload::render` implementation.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` when the schema is unknown, is
    /// not registered as `PayloadKind::Fact`, has no typed renderer, or
    /// the payload bytes fail to decode as the registered JSON type.
    pub fn render_fact_payload(
        &self,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        payload_bytes: &[u8],
    ) -> Result<String, StorageError> {
        self.try_render_fact_payload(schema_id, schema_version, payload_bytes)?
            .ok_or_else(|| {
                StorageError::Internal(format!(
                    "Fact schema {} v{} has no typed renderer",
                    schema_id.as_str(),
                    schema_version.into_inner(),
                ))
            })
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

    #[must_use]
    pub fn handle(&self, _req: &SchemaRequest) -> SchemaResponse {
        SchemaResponse {
            schemas: self.list(),
        }
    }
}
