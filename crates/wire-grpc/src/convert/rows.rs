use proxima_core::relation::{
    RelationDescriptor as CoreRelationDescriptor, SchemaRef as CoreSchemaRef,
};
use proxima_core::verbs::query::{EdgeRow, GoalRow, MemoryRow};
use proxima_core::verbs::schema::SchemaInfo as CoreSchemaInfo;

use crate::pb::{
    self, Goal as PbGoal, Memory as PbMemory, RelationClass as PbRelationClass,
    RelationDescriptor as PbRelationDescriptor, SchemaInfo as PbSchemaInfo, TypedGoal, TypedMemory,
};

use super::primitives::uuid_to_proto;
use super::refs::{
    entity_ref_to_proto, goal_state_to_proto, memory_kind_to_proto, owner_to_proto,
    payload_kind_to_proto, relation_class_to_proto, schema_ref_to_proto,
};

// ---------------------------------------------------------------------------
// SchemaInfo
// ---------------------------------------------------------------------------

pub fn schema_info_to_proto(core: &CoreSchemaInfo) -> PbSchemaInfo {
    PbSchemaInfo {
        schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
            core.schema_id.clone(),
            core.schema_version,
        ))),
        kind: payload_kind_to_proto(core.kind) as i32,
        filter_keys: core.filter_keys.clone(),
        sidecar_table: core.sidecar_table.clone(),
        natural_key_columns: core.natural_key_columns.clone(),
    }
}

// ---------------------------------------------------------------------------
// RelationDescriptor
// ---------------------------------------------------------------------------

pub fn relation_descriptor_to_proto(core: &CoreRelationDescriptor) -> PbRelationDescriptor {
    PbRelationDescriptor {
        relation: core.relation.clone(),
        class: relation_class_to_proto(core.class) as i32,
        payload_schema: core.payload_schema.as_ref().map(schema_ref_to_proto),
    }
}

// ---------------------------------------------------------------------------
// Memory / TypedMemory
// ---------------------------------------------------------------------------

pub fn memory_to_proto(core: &MemoryRow) -> TypedMemory {
    TypedMemory {
        memory: Some(PbMemory {
            memory_id: uuid_to_proto(core.id.into_inner()),
            kind: memory_kind_to_proto(core.kind) as i32,
            schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
                core.schema_id.clone(),
                core.schema_version,
            ))),
            owner: Some(owner_to_proto(&core.owner)),
        }),
        payload: core.payload.clone(),
    }
}

// ---------------------------------------------------------------------------
// Goal / TypedGoal
// ---------------------------------------------------------------------------

pub fn goal_to_proto(core: &GoalRow) -> TypedGoal {
    TypedGoal {
        goal: Some(PbGoal {
            goal_id: uuid_to_proto(core.id.into_inner()),
            owner: Some(owner_to_proto(&core.owner)),
            schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
                core.schema_id.clone(),
                core.schema_version,
            ))),
            title: core.title.clone(),
            text: core.text.clone(),
            state: goal_state_to_proto(core.state) as i32,
            parent_goal_ids: core
                .parent_goal_ids
                .iter()
                .map(|id| uuid_to_proto(id.into_inner()))
                .collect(),
            authorship: None,
        }),
        payload: core.payload.clone(),
    }
}

pub fn edge_to_proto(core: &EdgeRow) -> pb::Edge {
    let relation_class = match core.relation_class.as_str() {
        "Provenance" => PbRelationClass::Provenance,
        "Structural" => PbRelationClass::Structural,
        "Causal" => PbRelationClass::Causal,
        "Interpretive" => PbRelationClass::Interpretive,
        "Supersession" => PbRelationClass::Supersession,
        _ => PbRelationClass::Unspecified,
    };
    pb::Edge {
        edge_id: uuid_to_proto(core.id),
        relation: core.relation.clone(),
        relation_class: relation_class as i32,
        source: Some(entity_ref_to_proto(core.source)),
        target: Some(entity_ref_to_proto(core.target)),
        typed_payload: None,
    }
}
