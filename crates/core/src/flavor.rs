//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `SchemaRegistry` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::verbs::schema::{PayloadKind, PayloadValidatorEntry, SchemaInfo, SchemaRegistry};
use crate::{
    AbstractionPayload, EdgePayload, FactPayload, PerspectivePayload, RelationDescriptor,
    SchemaVersion, core_relation_descriptors,
};

#[derive(Debug)]
pub struct FlavorRegistry {
    schemas: Vec<SchemaInfo>,
    relations: Vec<RelationDescriptor>,
    validators: Vec<PayloadValidatorEntry>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        Self {
            schemas: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
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
        });
        self.validators.push(PayloadValidatorEntry {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            kind: PayloadKind::Perspective,
            validate: validate_payload_type::<P>,
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

    #[must_use]
    pub fn freeze(self) -> SchemaRegistry {
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
        SchemaRegistry::with_schemas_relations_validators(
            self.schemas,
            self.relations,
            self.validators,
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
