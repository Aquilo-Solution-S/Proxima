//! Conversion between proto wire types and `proxima_core` types.
//!
//! All fallible conversions return `Result<_, tonic::Status>`. Helper
//! converters are emitted exhaustively across the verb surface; some
//! are not yet referenced by `service.rs` (`Subscribe` / `EventIngest`
//! Goal-row paths land in A2.3+) — they're kept here as the contract
//! changeover point so the whole conversion table is in one file.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_lines
)]

use std::str::FromStr;

use prost_types::Timestamp;
use tonic::Status;
use uuid::Uuid;

use proxima_core::outbox::{EntityKind as OutboxEntityKind, EntityRef as CoreEntityRef};
use proxima_core::owner::Principal as CorePrincipal;
use proxima_core::relation::{
    RelationClass as CoreRelationClass, RelationDescriptor as CoreRelationDescriptor,
    SchemaRef as CoreSchemaRef,
};
use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind as CoreEntityKind, ErrorCode as CoreErrorCode, GoalId,
    GroupId, MemoryId, OperatorId, OrgId, Owner, ProtocolError, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, ToolId, UserId,
};

use proxima_core::verbs::goal_write::{
    GoalAuthorship as CoreGoalAuthorship, GoalDraft, GoalState as CoreGoalState, GoalWriteOutcome,
    OperatorKind as CoreOperatorKind, SystemOrigin as CoreSystemOrigin,
};
use proxima_core::verbs::query::{EdgeRow, EntityKind as QueryEntityKind, GoalRow, MemoryRow};
use proxima_core::verbs::schema::{PayloadKind as CorePayloadKind, SchemaInfo as CoreSchemaInfo};

use crate::pb::{
    self, ChangeEvent as PbChangeEvent, EntityAppend, EntityKind as PbEntityKind,
    EntityRef as PbEntityRef, ErrorCode as PbErrorCode, GoalAppend,
    GoalAuthorship as PbGoalAuthorship, GoalState as PbGoalState, GoalWriteRequest,
    GoalWriteResponse, Memory as PbMemory, MemoryAppend, MemoryKind as PbMemoryKind,
    OperatorKind as PbOperatorKind, OperatorOrigin, Owner as PbOwner, Principal as PbPrincipal,
    QueryRequest as PbQueryRequest, QueryResponse as PbQueryResponse,
    RelationClass as PbRelationClass, RelationDescriptor as PbRelationDescriptor,
    SchemaInfo as PbSchemaInfo, SchemaRef as PbSchemaRef, SchemaRequest as PbSchemaRequest,
    SchemaResponse as PbSchemaResponse, SubscribeRequest as PbSubscribeRequest, SystemAuthorship,
    ToolOrigin, TypedGoal, TypedMemory, UserAuthorship,
};

// ---------------------------------------------------------------------------
// IDs and primitives
// ---------------------------------------------------------------------------

pub fn uuid_from_proto(s: &str) -> Result<Uuid, Status> {
    Uuid::from_str(s).map_err(|e| Status::invalid_argument(format!("invalid UUID: {e}")))
}

pub fn uuid_to_proto(u: Uuid) -> String {
    u.to_string()
}

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
        PbGoalState::Active => Ok(CoreGoalState::Active),
        PbGoalState::Paused => Ok(CoreGoalState::Paused),
        PbGoalState::Achieved => Ok(CoreGoalState::Achieved),
        PbGoalState::Abandoned => Ok(CoreGoalState::Abandoned),
    }
}

pub fn goal_state_to_proto(core: CoreGoalState) -> PbGoalState {
    match core {
        CoreGoalState::Active => PbGoalState::Active,
        CoreGoalState::Paused => PbGoalState::Paused,
        CoreGoalState::Achieved => PbGoalState::Achieved,
        CoreGoalState::Abandoned => PbGoalState::Abandoned,
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
        goal: Some(pb::Goal {
            goal_id: uuid_to_proto(core.id.into_inner()),
            owner: Some(owner_to_proto(&core.owner)),
            schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
                core.schema_id.clone(),
                core.schema_version,
            ))),
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

pub fn goal_authorship_from_proto(pb: PbGoalAuthorship) -> Result<CoreGoalAuthorship, Status> {
    let kind = pb
        .kind
        .ok_or_else(|| Status::invalid_argument("missing goal authorship kind"))?;
    match kind {
        pb::goal_authorship::Kind::User(_) => Ok(CoreGoalAuthorship::User),
        pb::goal_authorship::Kind::System(s) => {
            let origin = s
                .origin
                .ok_or_else(|| Status::invalid_argument("missing system origin"))?;
            match origin {
                pb::system_authorship::Origin::Operator(o) => {
                    Ok(CoreGoalAuthorship::System(CoreSystemOrigin::Operator {
                        operator_id: OperatorId::new(uuid_from_proto(&o.operator_id)?),
                        operator_kind: operator_kind_from_proto(
                            PbOperatorKind::try_from(o.operator_kind)
                                .unwrap_or(PbOperatorKind::Unspecified),
                        )?,
                        model_id: proxima_core::ModelId::new(o.model_id.clone()),
                        prompt_version: proxima_core::PromptVersion::new(o.prompt_version.clone()),
                        personality_id: proxima_core::PersonalityId::new(o.personality_id.clone()),
                        personality_state_hash: proxima_core::PersonalityStateHash::new(
                            o.personality_state_hash.try_into().map_err(|_| {
                                Status::invalid_argument("invalid personality_state_hash length")
                            })?,
                        ),
                    }))
                }
                pb::system_authorship::Origin::Tool(t) => {
                    Ok(CoreGoalAuthorship::System(CoreSystemOrigin::Tool {
                        tool_id: ToolId::new(t.tool_id.clone()),
                    }))
                }
            }
        }
        pb::goal_authorship::Kind::External(_) => Ok(CoreGoalAuthorship::External),
    }
}

pub fn goal_authorship_to_proto(core: &CoreGoalAuthorship) -> PbGoalAuthorship {
    match core {
        CoreGoalAuthorship::User => PbGoalAuthorship {
            kind: Some(pb::goal_authorship::Kind::User(UserAuthorship {})),
        },
        CoreGoalAuthorship::System(s) => {
            let origin = match s {
                CoreSystemOrigin::Operator {
                    operator_id,
                    operator_kind,
                    model_id,
                    prompt_version,
                    personality_id,
                    personality_state_hash,
                } => pb::system_authorship::Origin::Operator(OperatorOrigin {
                    operator_id: uuid_to_proto(operator_id.into_inner()),
                    operator_kind: operator_kind_to_proto(*operator_kind) as i32,
                    model_id: model_id.as_str().to_string(),
                    prompt_version: prompt_version.as_str().to_string(),
                    personality_id: personality_id.as_str().to_string(),
                    personality_state_hash: personality_state_hash.into_inner().to_vec(),
                }),
                CoreSystemOrigin::Tool { tool_id } => {
                    pb::system_authorship::Origin::Tool(ToolOrigin {
                        tool_id: tool_id.as_str().to_string(),
                    })
                }
            };
            PbGoalAuthorship {
                kind: Some(pb::goal_authorship::Kind::System(SystemAuthorship {
                    origin: Some(origin),
                })),
            }
        }
        CoreGoalAuthorship::External => PbGoalAuthorship {
            kind: Some(pb::goal_authorship::Kind::External(
                pb::ExternalAuthorship {},
            )),
        },
    }
}

// ---------------------------------------------------------------------------
// ChangeEvent
// ---------------------------------------------------------------------------

pub fn change_event_to_proto(core: &ChangeEvent) -> Result<PbChangeEvent, Status> {
    let kind = match &core.kind {
        ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
            supersedes,
        } => {
            let body = match entity {
                CoreEntityRef::Memory(m) => pb::entity_append::Body::Memory(MemoryAppend {
                    memory_id: uuid_to_proto(m.into_inner()),
                    kind: outbox_entity_kind_to_memory_kind(*entity_kind) as i32,
                    schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
                        schema_id.clone(),
                        *schema_version,
                    ))),
                    supersedes_memory_id: supersedes.and_then(|e| match e {
                        CoreEntityRef::Memory(m) => Some(uuid_to_proto(m.into_inner())),
                        CoreEntityRef::Goal(_) => None,
                    }),
                }),
                CoreEntityRef::Goal(g) => pb::entity_append::Body::Goal(GoalAppend {
                    goal_id: uuid_to_proto(g.into_inner()),
                    schema: Some(schema_ref_to_proto(&CoreSchemaRef::new(
                        schema_id.clone(),
                        *schema_version,
                    ))),
                    supersedes_goal_id: supersedes.and_then(|e| match e {
                        CoreEntityRef::Goal(g) => Some(uuid_to_proto(g.into_inner())),
                        CoreEntityRef::Memory(_) => None,
                    }),
                }),
            };
            pb::change_event::Kind::EntityAppend(EntityAppend { body: Some(body) })
        }
        ChangeEventKind::EdgeAppend {
            edge_id,
            relation,
            source,
            target,
        } => pb::change_event::Kind::EdgeAppend(pb::EdgeAppend {
            edge_id: uuid_to_proto(*edge_id),
            relation: relation.clone(),
            source: Some(entity_ref_to_proto(*source)),
            target: Some(entity_ref_to_proto(*target)),
        }),
    };
    Ok(PbChangeEvent {
        seq: uuid_to_proto(core.seq),
        owner: Some(owner_to_proto(&core.owner)),
        kind: Some(kind),
    })
}

// ---------------------------------------------------------------------------
// Timestamp conversions
// ---------------------------------------------------------------------------

pub fn timestamp_from_proto(ts: Option<Timestamp>) -> Result<time::OffsetDateTime, Status> {
    let ts = ts.ok_or_else(|| Status::invalid_argument("missing timestamp"))?;
    let total_nanos = i128::from(ts.seconds) * 1_000_000_000 + i128::from(ts.nanos);
    time::OffsetDateTime::from_unix_timestamp_nanos(total_nanos)
        .map_err(|e| Status::invalid_argument(format!("invalid timestamp: {e}")))
}

pub fn timestamp_to_proto(ts: time::OffsetDateTime) -> Timestamp {
    let nanos = ts.unix_timestamp_nanos();
    let seconds_i128 = nanos / 1_000_000_000;
    let nanos_part = (nanos % 1_000_000_000) as i32;
    Timestamp {
        seconds: i64::try_from(seconds_i128).unwrap_or(i64::MAX),
        nanos: nanos_part,
    }
}

// ---------------------------------------------------------------------------
// Request/Response for Query
// ---------------------------------------------------------------------------

pub fn query_request_from_proto(
    pb: PbQueryRequest,
) -> Result<proxima_core::verbs::query::QueryRequest, Status> {
    let filter = pb.filter.unwrap_or_default();
    let pagination = pb.pagination.unwrap_or_default();

    let entity_kind = filter
        .entity_kind
        .and_then(|k| {
            let pb_kind = PbEntityKind::try_from(k).unwrap_or(PbEntityKind::Unspecified);
            if pb_kind == PbEntityKind::Unspecified {
                None
            } else {
                Some(entity_kind_from_proto(pb_kind))
            }
        })
        .transpose()?;

    // Convert from CoreEntityKind to QueryEntityKind
    let entity_kind: Option<QueryEntityKind> = entity_kind.map(|k| match k {
        CoreEntityKind::Fact => QueryEntityKind::Fact,
        CoreEntityKind::Abstraction => QueryEntityKind::Abstraction,
        CoreEntityKind::Perspective => QueryEntityKind::Perspective,
        CoreEntityKind::Goal => QueryEntityKind::Goal,
    });

    let schema_id = filter.schema_id.clone().map(SchemaId::new);

    let supersession = match filter.supersession() {
        pb::SupersessionFilter::Unspecified | pb::SupersessionFilter::HeadsOnly => {
            proxima_core::verbs::query::SupersessionStatus::HeadsOnly
        }
        pb::SupersessionFilter::IncludeSuperseded => {
            proxima_core::verbs::query::SupersessionStatus::IncludeSuperseded
        }
    };

    let limit = if pagination.limit == 0 {
        100
    } else {
        pagination.limit
    };

    Ok(proxima_core::verbs::query::QueryRequest {
        owner: owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?,
        entity_kind,
        schema_id,
        supersession,
        limit,
        memory_ids: Vec::new(),
        goal_ids: Vec::new(),
        edge_ids: Vec::new(),
        stateful_heads: None,
    })
}

pub fn query_response_to_proto(
    core: &proxima_core::verbs::query::QueryResponse,
) -> PbQueryResponse {
    PbQueryResponse {
        memories: core.memories.iter().map(memory_to_proto).collect(),
        goals: core.goals.iter().map(goal_to_proto).collect(),
        seq_high_water: core.seq_high_water.map(uuid_to_proto),
        edges: core.edges.iter().map(edge_to_proto).collect(),
    }
}

// ---------------------------------------------------------------------------
// Request/Response for Subscribe
// ---------------------------------------------------------------------------

pub fn subscribe_request_from_proto(
    pb: PbSubscribeRequest,
) -> Result<proxima_core::verbs::subscribe::SubscribeRequest, Status> {
    let since = pb.since.map(|s| uuid_from_proto(&s)).transpose()?;

    // Filter is not used in the current SubscribeRequest, but we parse it for completeness
    let _filter = pb.filter.unwrap_or_default();

    Ok(proxima_core::verbs::subscribe::SubscribeRequest {
        owner: owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?,
        since,
    })
}

// ---------------------------------------------------------------------------
// Request/Response for GoalWrite
// ---------------------------------------------------------------------------

pub fn goal_write_request_from_proto(pb: GoalWriteRequest) -> Result<GoalDraft, Status> {
    let schema_ref = pb
        .schema
        .clone()
        .ok_or_else(|| Status::invalid_argument("missing schema"))?;

    let authorship = goal_authorship_from_proto(
        pb.authorship
            .clone()
            .ok_or_else(|| Status::invalid_argument("missing authorship"))?,
    )?;

    let parent_goal_ids: Result<Vec<GoalId>, Status> = pb
        .parent_goal_ids
        .iter()
        .map(|s| uuid_from_proto(s).map(GoalId::new))
        .collect();

    Ok(GoalDraft {
        owner: owner_from_proto(
            pb.owner
                .clone()
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?,
        schema_id: SchemaId::new(schema_ref.schema_id.clone()),
        schema_version: SchemaVersion::new(schema_ref.schema_version),
        text: pb.text.clone(),
        state: goal_state_from_proto(pb.state())?,
        parent_goal_ids: parent_goal_ids?,
        authorship,
        request_id: pb.request_id.clone(),
    })
}

pub fn goal_write_response_to_proto(core: &GoalWriteOutcome) -> GoalWriteResponse {
    GoalWriteResponse {
        goal_id: uuid_to_proto(core.goal_id.into_inner()),
        change_event_seq: uuid_to_proto(core.change_event_seq),
        idempotent_replay: core.idempotent_replay,
    }
}

// ---------------------------------------------------------------------------
// Request/Response for EventIngest
// ---------------------------------------------------------------------------

pub fn event_ingest_request_from_proto(
    pb: pb::EventIngestRequest,
) -> Result<proxima_core::verbs::event_ingest::EventDraft, Status> {
    let schema_ref = pb
        .schema
        .ok_or_else(|| Status::invalid_argument("missing schema"))?;

    let cited_object = pb
        .cited_object
        .map(|c| {
            let schema = c
                .schema
                .ok_or_else(|| Status::invalid_argument("missing cited_object schema"))?;
            Result::<_, Status>::Ok(proxima_core::verbs::event_ingest::CitedObjectHint {
                schema_id: SchemaId::new(schema.schema_id.clone()),
                schema_version: SchemaVersion::new(schema.schema_version),
                content_hash: c
                    .content_hash
                    .try_into()
                    .map_err(|_| Status::invalid_argument("invalid content_hash length"))?,
            })
        })
        .transpose()?
        .unwrap_or_else(|| proxima_core::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new(String::new()),
            schema_version: SchemaVersion::new(0),
            content_hash: [0; 32],
        });

    let citation_mapping = pb
        .citation_mapping
        .map(|c| {
            let schema = c
                .schema
                .ok_or_else(|| Status::invalid_argument("missing citation_mapping schema"))?;
            Result::<_, Status>::Ok(proxima_core::verbs::event_ingest::CitationMappingHint {
                schema_id: SchemaId::new(schema.schema_id.clone()),
                schema_version: SchemaVersion::new(schema.schema_version),
            })
        })
        .transpose()?
        .unwrap_or_else(|| proxima_core::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new(String::new()),
            schema_version: SchemaVersion::new(0),
        });

    Ok(proxima_core::verbs::event_ingest::EventDraft {
        source_id: SourceId::new(pb.source_id.clone()),
        source_batch_id: SourceBatchId::new(uuid_from_proto(&pb.source_batch_id)?),
        owner: owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?,
        schema_id: SchemaId::new(schema_ref.schema_id.clone()),
        schema_version: SchemaVersion::new(schema_ref.schema_version),
        payload: pb.payload.clone(),
        observed_at: timestamp_from_proto(pb.observed_at)?,
        occurred_at: timestamp_from_proto(pb.occurred_at)?,
        cited_object,
        citation_mapping,
    })
}

pub fn event_ingest_response_to_proto(
    core: &proxima_core::verbs::event_ingest::EventIngestOutcome,
) -> pb::EventIngestResponse {
    pb::EventIngestResponse {
        event_id: core.event_id.into_inner().to_vec(),
        memory_id: uuid_to_proto(core.memory_id.into_inner()),
        change_event_seq: uuid_to_proto(core.change_event_seq),
        idempotent_replay: core.idempotent_replay,
    }
}

// ---------------------------------------------------------------------------
// Request/Response for Schema
// ---------------------------------------------------------------------------

pub fn schema_request_from_proto(
    _pb: PbSchemaRequest,
) -> proxima_core::verbs::schema::SchemaRequest {
    proxima_core::verbs::schema::SchemaRequest
}

pub fn schema_response_to_proto(
    core: &proxima_core::verbs::schema::SchemaResponse,
    relations: &[proxima_core::relation::RelationDescriptor],
) -> PbSchemaResponse {
    PbSchemaResponse {
        schemas: core.schemas.iter().map(schema_info_to_proto).collect(),
        relations: relations.iter().map(relation_descriptor_to_proto).collect(),
    }
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

pub fn protocol_error_to_status(err: ProtocolError) -> Status {
    use prost::Message as _;

    let code = match err.code {
        CoreErrorCode::AuthRequired => tonic::Code::Unauthenticated,
        CoreErrorCode::Forbidden => tonic::Code::PermissionDenied,
        CoreErrorCode::UnknownSchema => tonic::Code::InvalidArgument,
        CoreErrorCode::AlreadyIngested | CoreErrorCode::IdempotencyConflict => {
            tonic::Code::AlreadyExists
        }
        CoreErrorCode::NotFound => tonic::Code::NotFound,
        CoreErrorCode::Internal => tonic::Code::Internal,
    };

    let pb_error = pb::ProtocolError {
        code: pb_error_code_from_core(err.code) as i32,
        message: err.message.clone(),
        details: Vec::new(),
        request_id: err.request_id,
    };

    let mut status = Status::new(code, err.message);
    status.metadata_mut().insert_bin(
        "proxima-error-bin",
        tonic::metadata::MetadataValue::from_bytes(&pb_error.encode_to_vec()),
    );
    status
}

fn pb_error_code_from_core(code: CoreErrorCode) -> PbErrorCode {
    match code {
        CoreErrorCode::AuthRequired => PbErrorCode::AuthRequired,
        CoreErrorCode::Forbidden => PbErrorCode::Forbidden,
        CoreErrorCode::UnknownSchema => PbErrorCode::UnknownSchema,
        CoreErrorCode::AlreadyIngested | CoreErrorCode::IdempotencyConflict => {
            PbErrorCode::IdempotencyConflict
        }
        CoreErrorCode::NotFound => PbErrorCode::NotFound,
        CoreErrorCode::Internal => PbErrorCode::Internal,
    }
}
