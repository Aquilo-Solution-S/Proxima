use tonic::Status;

use proxima_core::outbox::EntityRef as CoreEntityRef;
use proxima_core::relation::SchemaRef as CoreSchemaRef;
use proxima_core::verbs::goal_write::{
    GoalAuthorship as CoreGoalAuthorship, GoalDraft, GoalWriteOutcome,
    SystemOrigin as CoreSystemOrigin,
};
use proxima_core::verbs::query::EntityKind as QueryEntityKind;
use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind as CoreEntityKind, GoalId, OperatorId, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, ToolId,
};

use crate::pb::{
    self, ChangeEvent as PbChangeEvent, EntityAppend, EntityKind as PbEntityKind, GoalAppend,
    GoalAuthorship as PbGoalAuthorship, GoalWriteRequest, GoalWriteResponse, MemoryAppend,
    OperatorKind as PbOperatorKind, OperatorOrigin, QueryRequest as PbQueryRequest,
    QueryResponse as PbQueryResponse, SchemaRequest as PbSchemaRequest,
    SchemaResponse as PbSchemaResponse, SubscribeRequest as PbSubscribeRequest, SystemAuthorship,
    TombstoneFilter as PbTombstoneFilter, ToolOrigin, UserAuthorship,
};

use super::primitives::{timestamp_from_proto, uuid_from_proto, uuid_to_proto};
use super::refs::{
    entity_kind_from_proto, entity_ref_to_proto, goal_state_from_proto, operator_kind_from_proto,
    operator_kind_to_proto, outbox_entity_kind_to_memory_kind, owner_from_proto, owner_to_proto,
    schema_ref_to_proto,
};
use super::rows::{
    edge_to_proto, goal_to_proto, memory_to_proto, relation_descriptor_to_proto,
    schema_info_to_proto,
};

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
                        personality_instance_id: proxima_core::PersonalityInstanceId::new(
                            uuid_from_proto(&o.personality_instance_id)?,
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
                    personality_instance_id,
                } => pb::system_authorship::Origin::Operator(OperatorOrigin {
                    operator_id: uuid_to_proto(operator_id.into_inner()),
                    operator_kind: operator_kind_to_proto(*operator_kind) as i32,
                    model_id: model_id.as_str().to_string(),
                    prompt_version: prompt_version.as_str().to_string(),
                    personality_instance_id: uuid_to_proto(personality_instance_id.into_inner()),
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
                    personality_instance_id: core
                        .authoring_personality_instance_id
                        .map(uuid_to_proto),
                    wake_chain_depth: u32::from(core.wake_chain_depth),
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

    let tombstones = match filter.tombstones() {
        PbTombstoneFilter::Unspecified | PbTombstoneFilter::PresentOnly => {
            proxima_core::verbs::query::TombstoneFilter::PresentOnly
        }
        PbTombstoneFilter::IncludeTombstoned => {
            proxima_core::verbs::query::TombstoneFilter::IncludeTombstoned
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
        tombstones,
        limit,
        memory_ids: Vec::new(),
        goal_ids: Vec::new(),
        edge_ids: Vec::new(),
        stateful_heads: Vec::new(),
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
// Request/Response for EventHistory
// ---------------------------------------------------------------------------

pub fn event_history_request_from_proto(
    pb: pb::EventHistoryRequest,
) -> Result<proxima_core::verbs::event_history::EventHistoryRequest, Status> {
    Ok(proxima_core::verbs::event_history::EventHistoryRequest {
        owner: owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?,
        limit: pb.limit,
        before: pb.before.map(|s| uuid_from_proto(&s)).transpose()?,
    })
}

pub fn event_history_response_to_proto(
    resp: &proxima_core::verbs::event_history::EventHistoryResponse,
) -> Result<pb::EventHistoryResponse, Status> {
    let events = resp
        .events
        .iter()
        .map(change_event_to_proto)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(pb::EventHistoryResponse {
        events,
        seq_high_water: resp.seq_high_water.map(uuid_to_proto),
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
        title: pb.title.clone(),
        text: pb.text.clone(),
        payload: pb.payload.clone(),
        state: goal_state_from_proto(pb.state())?,
        parent_goal_ids: parent_goal_ids?,
        supersedes_goal_id: pb
            .supersedes_goal_id
            .as_deref()
            .map(uuid_from_proto)
            .transpose()?
            .map(GoalId::new),
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

#[cfg(test)]
mod tests {
    use proxima_core::verbs::query::TombstoneFilter;
    use proxima_core::{OrgId, Owner, Principal, UserId};
    use uuid::Uuid;

    use super::*;

    fn owner_proto() -> pb::Owner {
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        owner_to_proto(&owner)
    }

    #[test]
    fn query_unspecified_tombstones_defaults_to_present_only() {
        let core = query_request_from_proto(pb::QueryRequest {
            owner: Some(owner_proto()),
            filter: Some(pb::ReadFilter {
                entity_kind: None,
                schema_id: None,
                supersession: pb::SupersessionFilter::Unspecified as i32,
                flavor_filters: Vec::new(),
                tombstones: pb::TombstoneFilter::Unspecified as i32,
            }),
            pagination: Some(pb::ReadPagination { limit: 10 }),
        })
        .expect("conversion");
        assert_eq!(core.tombstones, TombstoneFilter::PresentOnly);
    }
}
