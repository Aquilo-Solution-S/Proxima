use tonic::Status;

use proxima_core::outbox::{EntityKind as OutboxEntityKind, EntityRef as CoreEntityRef};
use proxima_core::owner::Principal as CorePrincipal;
use proxima_core::relation::{RelationClass as CoreRelationClass, SchemaRef as CoreSchemaRef};
use proxima_core::verbs::goal_write::{
    GoalState as CoreGoalState, OperatorKind as CoreOperatorKind,
};
use proxima_core::verbs::query::EntityKind as QueryEntityKind;
use proxima_core::verbs::schema::PayloadKind as CorePayloadKind;
use proxima_core::{
    EntityKind as CoreEntityKind, GoalId, GroupId, MemoryId, OrgId, Owner, SchemaId, SchemaVersion,
    UserId,
};

use crate::pb::{
    self, EntityKind as PbEntityKind, EntityRef as PbEntityRef, GoalState as PbGoalState,
    MemoryKind as PbMemoryKind, OperatorKind as PbOperatorKind, Owner as PbOwner,
    Principal as PbPrincipal, RelationClass as PbRelationClass, SchemaRef as PbSchemaRef,
};

use super::primitives::{uuid_from_proto, uuid_to_proto};

pub fn schema_ref_from_proto(pb: PbSchemaRef) -> Result<CoreSchemaRef, Status> {
    Ok(CoreSchemaRef::new(
        SchemaId::new(pb.schema_id),
        SchemaVersion::new(pb.schema_version),
    ))
}

pub fn schema_ref_to_proto(core: &CoreSchemaRef) -> PbSchemaRef {
    PbSchemaRef {
        schema_id: core.schema_id.as_str().to_string(),
        schema_version: core.schema_version.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// Owner / Principal
// ---------------------------------------------------------------------------

pub fn owner_from_proto(pb: PbOwner) -> Result<Owner, Status> {
    let principal = match pb
        .principal
        .ok_or_else(|| Status::invalid_argument("missing principal"))?
    {
        PbPrincipal {
            kind: Some(pb::principal::Kind::UserId(s)),
        } => CorePrincipal::User(UserId::new(uuid_from_proto(&s)?)),
        PbPrincipal {
            kind: Some(pb::principal::Kind::GroupId(s)),
        } => CorePrincipal::Group(GroupId::new(uuid_from_proto(&s)?)),
        PbPrincipal { kind: None } => {
            return Err(Status::invalid_argument("principal kind is none"));
        }
    };
    Ok(Owner {
        principal,
        org_id: OrgId::new(uuid_from_proto(&pb.org_id)?),
    })
}

pub fn owner_to_proto(core: &Owner) -> PbOwner {
    let principal = match &core.principal {
        CorePrincipal::User(u) => PbPrincipal {
            kind: Some(pb::principal::Kind::UserId(uuid_to_proto(u.into_inner()))),
        },
        CorePrincipal::Group(g) => PbPrincipal {
            kind: Some(pb::principal::Kind::GroupId(uuid_to_proto(g.into_inner()))),
        },
    };
    PbOwner {
        principal: Some(principal),
        org_id: uuid_to_proto(core.org_id.into_inner()),
    }
}

pub fn entity_ref_from_proto(pb: PbEntityRef) -> Result<CoreEntityRef, Status> {
    match pb
        .r#ref
        .ok_or_else(|| Status::invalid_argument("missing entity ref"))?
    {
        pb::entity_ref::Ref::MemoryId(s) => {
            Ok(CoreEntityRef::Memory(MemoryId::new(uuid_from_proto(&s)?)))
        }
        pb::entity_ref::Ref::GoalId(s) => {
            Ok(CoreEntityRef::Goal(GoalId::new(uuid_from_proto(&s)?)))
        }
    }
}

pub fn entity_ref_to_proto(core: CoreEntityRef) -> PbEntityRef {
    match core {
        CoreEntityRef::Memory(m) => PbEntityRef {
            r#ref: Some(pb::entity_ref::Ref::MemoryId(uuid_to_proto(m.into_inner()))),
        },
        CoreEntityRef::Goal(g) => PbEntityRef {
            r#ref: Some(pb::entity_ref::Ref::GoalId(uuid_to_proto(g.into_inner()))),
        },
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

pub fn entity_kind_from_proto(pb: PbEntityKind) -> Result<CoreEntityKind, Status> {
    match pb {
        PbEntityKind::Unspecified => Err(Status::invalid_argument("unspecified entity kind")),
        PbEntityKind::Fact => Ok(CoreEntityKind::Fact),
        PbEntityKind::Abstraction => Ok(CoreEntityKind::Abstraction),
        PbEntityKind::Perspective => Ok(CoreEntityKind::Perspective),
        PbEntityKind::Goal => Ok(CoreEntityKind::Goal),
    }
}

pub fn entity_kind_to_proto(core: CoreEntityKind) -> PbEntityKind {
    match core {
        CoreEntityKind::Fact => PbEntityKind::Fact,
        CoreEntityKind::Abstraction => PbEntityKind::Abstraction,
        CoreEntityKind::Perspective => PbEntityKind::Perspective,
        CoreEntityKind::Goal => PbEntityKind::Goal,
    }
}

pub fn memory_kind_from_proto(pb: PbMemoryKind) -> Result<CoreEntityKind, Status> {
    match pb {
        PbMemoryKind::Unspecified => Err(Status::invalid_argument("unspecified memory kind")),
        PbMemoryKind::Fact => Ok(CoreEntityKind::Fact),
        PbMemoryKind::Abstraction => Ok(CoreEntityKind::Abstraction),
        PbMemoryKind::Perspective => Ok(CoreEntityKind::Perspective),
    }
}

pub fn memory_kind_to_proto(core: QueryEntityKind) -> PbMemoryKind {
    match core {
        QueryEntityKind::Fact => PbMemoryKind::Fact,
        QueryEntityKind::Abstraction => PbMemoryKind::Abstraction,
        QueryEntityKind::Perspective => PbMemoryKind::Perspective,
        QueryEntityKind::Goal => unreachable!("Goal cannot appear in a MemoryRow — invariant 11"),
    }
}

pub fn outbox_entity_kind_to_memory_kind(core: OutboxEntityKind) -> PbMemoryKind {
    match core {
        OutboxEntityKind::Fact => PbMemoryKind::Fact,
        OutboxEntityKind::Abstraction => PbMemoryKind::Abstraction,
        OutboxEntityKind::Perspective => PbMemoryKind::Perspective,
        OutboxEntityKind::Goal => {
            unreachable!("Goal cannot appear in a MemoryAppend — invariant 11")
        }
    }
}

pub fn payload_kind_from_proto(pb: pb::PayloadKind) -> Result<CorePayloadKind, Status> {
    match pb {
        pb::PayloadKind::Unspecified => Err(Status::invalid_argument("unspecified payload kind")),
        pb::PayloadKind::Fact => Ok(CorePayloadKind::Fact),
        pb::PayloadKind::Abstraction => Ok(CorePayloadKind::Abstraction),
        pb::PayloadKind::Perspective => Ok(CorePayloadKind::Perspective),
        pb::PayloadKind::Goal => Ok(CorePayloadKind::Goal),
        pb::PayloadKind::Edge => Ok(CorePayloadKind::Edge),
        pb::PayloadKind::CitedObject => Ok(CorePayloadKind::CitedObject),
        pb::PayloadKind::CitationMapping => Ok(CorePayloadKind::CitationMapping),
    }
}

pub fn payload_kind_to_proto(core: CorePayloadKind) -> pb::PayloadKind {
    match core {
        CorePayloadKind::Fact => pb::PayloadKind::Fact,
        CorePayloadKind::Abstraction => pb::PayloadKind::Abstraction,
        CorePayloadKind::Perspective => pb::PayloadKind::Perspective,
        CorePayloadKind::Goal => pb::PayloadKind::Goal,
        CorePayloadKind::Edge => pb::PayloadKind::Edge,
        CorePayloadKind::CitedObject => pb::PayloadKind::CitedObject,
        CorePayloadKind::CitationMapping => pb::PayloadKind::CitationMapping,
    }
}

pub fn relation_class_from_proto(pb: PbRelationClass) -> Result<CoreRelationClass, Status> {
    match pb {
        PbRelationClass::Unspecified => Err(Status::invalid_argument("unspecified relation class")),
        PbRelationClass::Structural => Ok(CoreRelationClass::Structural),
        PbRelationClass::Provenance => Ok(CoreRelationClass::Provenance),
        PbRelationClass::Supersession => Ok(CoreRelationClass::Supersession),
        PbRelationClass::Causal => Ok(CoreRelationClass::Causal),
        PbRelationClass::Interpretive => Ok(CoreRelationClass::Interpretive),
    }
}

pub fn relation_class_to_proto(core: CoreRelationClass) -> PbRelationClass {
    match core {
        CoreRelationClass::Structural => PbRelationClass::Structural,
        CoreRelationClass::Provenance => PbRelationClass::Provenance,
        CoreRelationClass::Supersession => PbRelationClass::Supersession,
        CoreRelationClass::Causal => PbRelationClass::Causal,
        CoreRelationClass::Interpretive => PbRelationClass::Interpretive,
    }
}

pub fn goal_state_from_proto(pb: PbGoalState) -> Result<CoreGoalState, Status> {
    match pb {
        PbGoalState::Unspecified => Err(Status::invalid_argument("unspecified goal state")),
        PbGoalState::Proposed => Ok(CoreGoalState::Proposed),
        PbGoalState::Active => Ok(CoreGoalState::Active),
        PbGoalState::Paused => Ok(CoreGoalState::Paused),
        PbGoalState::Achieved => Ok(CoreGoalState::Achieved),
        PbGoalState::Abandoned => Ok(CoreGoalState::Abandoned),
        PbGoalState::Rejected => Ok(CoreGoalState::Rejected),
    }
}

pub fn goal_state_to_proto(core: CoreGoalState) -> PbGoalState {
    match core {
        CoreGoalState::Proposed => PbGoalState::Proposed,
        CoreGoalState::Active => PbGoalState::Active,
        CoreGoalState::Paused => PbGoalState::Paused,
        CoreGoalState::Achieved => PbGoalState::Achieved,
        CoreGoalState::Abandoned => PbGoalState::Abandoned,
        CoreGoalState::Rejected => PbGoalState::Rejected,
    }
}

pub fn operator_kind_from_proto(pb: PbOperatorKind) -> Result<CoreOperatorKind, Status> {
    match pb {
        PbOperatorKind::Unspecified => Err(Status::invalid_argument("unspecified operator kind")),
        PbOperatorKind::AToGoal => Ok(CoreOperatorKind::AtoGoal),
    }
}

pub fn operator_kind_to_proto(core: CoreOperatorKind) -> PbOperatorKind {
    match core {
        CoreOperatorKind::AtoGoal => PbOperatorKind::AToGoal,
    }
}
