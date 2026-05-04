//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `SchemaRegistry` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
use crate::{AbstractionPayload, FactPayload, PerspectivePayload, SchemaVersion};

#[derive(Debug, Default)]
pub struct FlavorRegistry {
    schemas: Vec<SchemaInfo>,
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
        });
    }

    pub fn add_abstraction_schema<A: AbstractionPayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: A::schema_id(),
            schema_version: SchemaVersion::new(A::SCHEMA_VERSION),
            kind: PayloadKind::Abstraction,
            filter_keys: vec![],
        });
    }

    pub fn add_perspective_schema<P: PerspectivePayload>(&mut self) {
        self.schemas.push(SchemaInfo {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            kind: PayloadKind::Perspective,
            filter_keys: vec![],
        });
    }

    #[must_use]
    pub fn freeze(self) -> SchemaRegistry {
        SchemaRegistry::with_schemas(self.schemas)
    }
}
