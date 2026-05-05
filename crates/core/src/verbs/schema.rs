//! Schema verb — registry introspection.
//!
//! See docs/14-protocol-surface.md §"Schema" and
//! docs/03-schema-registry.md.

use crate::{RegisteredRelation, RelationDescriptor, SchemaId, SchemaVersion};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
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
pub struct SchemaInfo {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub filter_keys: Vec<String>,
    /// Sidecar table identifier (qualified, e.g. `proxima_code.code_chunk_v1`)
    /// when the payload trait declares one; `None` for `Goal`, `CitedObject`,
    /// and `CitationMapping` payloads which don't participate in F/A/P
    /// queries.
    pub sidecar_table: Option<String>,
    /// Natural-key columns for stateful Fact schemas (docs/03 §Stateful
    /// Fact schemas). Empty for stateless / non-Fact schemas. Drives the
    /// head-by-natural-key SQL emission in `Query` heads-only mode.
    pub natural_key_columns: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaRequest;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaResponse {
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Default)]
pub struct SchemaRegistry {
    schemas: Vec<SchemaInfo>,
    relations: Vec<RelationDescriptor>,
}

impl SchemaRegistry {
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
        Self { schemas, relations }
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
            sidecar_table,
            natural_key_columns: info.natural_key_columns.clone(),
        })
    }

    pub fn handle(&self, _req: &SchemaRequest) -> SchemaResponse {
        SchemaResponse {
            schemas: self.list(),
        }
    }
}
