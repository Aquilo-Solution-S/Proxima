//! Schema verb — registry introspection.
//!
//! See docs/14-protocol-surface.md §"Schema" and
//! docs/03-schema-registry.md.

use crate::{SchemaId, SchemaVersion};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PayloadKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    CitedObject,
    CitationMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchemaInfo {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub filter_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SchemaRequest;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchemaResponse {
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Default)]
pub struct SchemaRegistry {
    schemas: Vec<SchemaInfo>,
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
        Self { schemas }
    }

    pub fn list(&self) -> Vec<SchemaInfo> {
        self.schemas.clone()
    }

    /// Lookup by `(schema_id, schema_version)`. Used by
    /// EventIngest / GoalWrite to validate incoming payloads.
    #[must_use]
    pub fn lookup(&self, schema_id: &SchemaId, version: SchemaVersion) -> Option<&SchemaInfo> {
        self.schemas
            .iter()
            .find(|s| s.schema_id == *schema_id && s.schema_version == version)
    }

    pub fn handle(&self, _req: &SchemaRequest) -> SchemaResponse {
        SchemaResponse {
            schemas: self.list(),
        }
    }
}
