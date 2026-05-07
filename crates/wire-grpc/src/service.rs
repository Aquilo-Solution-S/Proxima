//! gRPC service implementation for the Engine trait.

use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use proxima_core::Engine;
use proxima_core::auth::Credentials;

use crate::convert::refs::{owner_from_proto, owner_to_proto};
use crate::convert::{
    change_event_to_proto, event_history_request_from_proto, event_history_response_to_proto,
    event_ingest_request_from_proto, event_ingest_response_to_proto, goal_write_request_from_proto,
    goal_write_response_to_proto, protocol_error_to_status, query_request_from_proto,
    query_response_to_proto, schema_request_from_proto, schema_response_to_proto,
    subscribe_request_from_proto,
};
use crate::pb::{
    self, AuthorFilter, ChangeEvent, CustomFilter, EventHistoryRequest, EventHistoryResponse,
    EventIngestRequest, EventIngestResponse, GoalWriteRequest, GoalWriteResponse,
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, ListPersonalityInstancesRequest,
    ListPersonalityInstancesResponse, OnEdgeFilter, OnMemoryFilter, PersonalityAuthor,
    PersonalityInstance, ProvisionOwnerRequest, ProvisionOwnerResponse, QueryRequest,
    QueryResponse, SchemaRequest, SchemaResponse, SetWakeConfigRequest, SetWakeConfigResponse,
    SubscribeRequest, WakeFilter, WakeTarget, engine_server::Engine as EngineTrait,
};

/// gRPC server wrapper for the Engine.
#[derive(Debug)]
pub struct EngineGrpcServer {
    engine: Arc<Engine>,
}

impl EngineGrpcServer {
    /// Create a new gRPC server wrapping the given engine.
    #[must_use]
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

#[tonic::async_trait]
impl EngineTrait for EngineGrpcServer {
    async fn query(
        &self,
        request: Request<QueryRequest>,
    ) -> Result<Response<QueryResponse>, Status> {
        let req = query_request_from_proto(request.into_inner())?;
        let response = self
            .engine
            .query(&Credentials::None, &req)
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(query_response_to_proto(&response)))
    }

    type SubscribeStream = Pin<Box<dyn Stream<Item = Result<ChangeEvent, Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = subscribe_request_from_proto(request.into_inner())?;
        let stream = self
            .engine
            .subscribe(&Credentials::None, req)
            .await
            .map_err(protocol_error_to_status)?;

        // Create a bounded channel for backpressure
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut inbound = Box::pin(stream);
            while let Some(event) = inbound.next().await {
                let pb_event = match change_event_to_proto(&event) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                if tx.send(Ok(pb_event)).await.is_err() {
                    // Receiver dropped
                    return;
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::SubscribeStream))
    }

    async fn goal_write(
        &self,
        request: Request<GoalWriteRequest>,
    ) -> Result<Response<GoalWriteResponse>, Status> {
        let req = goal_write_request_from_proto(request.into_inner())?;
        let response = self
            .engine
            .write_goal(&Credentials::None, req)
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(goal_write_response_to_proto(&response)))
    }

    async fn event_history(
        &self,
        request: Request<EventHistoryRequest>,
    ) -> Result<Response<EventHistoryResponse>, Status> {
        let req = event_history_request_from_proto(request.into_inner())?;
        let response = self
            .engine
            .event_history(&Credentials::None, &req)
            .await
            .map_err(protocol_error_to_status)?;
        event_history_response_to_proto(&response).map(Response::new)
    }

    async fn event_ingest(
        &self,
        request: Request<EventIngestRequest>,
    ) -> Result<Response<EventIngestResponse>, Status> {
        let req = event_ingest_request_from_proto(request.into_inner())?;
        let response = self
            .engine
            .event_ingest(&Credentials::None, req)
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(event_ingest_response_to_proto(&response)))
    }

    async fn schema(
        &self,
        request: Request<SchemaRequest>,
    ) -> Result<Response<SchemaResponse>, Status> {
        let req = schema_request_from_proto(request.into_inner());
        let response = self.engine.schema(&req);
        // The proto SchemaResponse includes relations, but the core SchemaResponse doesn't.
        // We need to get relations from the engine's registry.
        let relations = self.engine.registry().list_relations().to_vec();
        Ok(Response::new(schema_response_to_proto(
            &response, &relations,
        )))
    }

    async fn provision_owner(
        &self,
        request: Request<ProvisionOwnerRequest>,
    ) -> Result<Response<ProvisionOwnerResponse>, Status> {
        let owner = owner_from_proto(
            request
                .into_inner()
                .owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?;
        self.engine
            .provision_owner(&owner)
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(ProvisionOwnerResponse {}))
    }

    async fn instantiate_personality(
        &self,
        request: Request<InstantiatePersonalityRequest>,
    ) -> Result<Response<InstantiatePersonalityResponse>, Status> {
        let pb = request.into_inner();
        let owner = owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?;
        let payload_overrides = pb
            .payload_overrides_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| Status::invalid_argument(format!("payload_overrides_json: {e}")))?;
        let out = self
            .engine
            .instantiate_personality(proxima_core::InstantiatePersonalityRequest {
                owner,
                personality_type_id: pb.personality_type_id,
                payload_overrides,
            })
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(InstantiatePersonalityResponse {
            personality_instance_id: out.instance_id.into_inner().to_string(),
        }))
    }

    async fn set_wake_config(
        &self,
        request: Request<SetWakeConfigRequest>,
    ) -> Result<Response<SetWakeConfigResponse>, Status> {
        let pb = request.into_inner();
        let owner = owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?;
        let instance_id = uuid_from_str(&pb.personality_instance_id)?;
        let filters = pb
            .wake_filters
            .into_iter()
            .map(wake_filter_from_proto)
            .collect::<Result<Vec<_>, _>>()?;
        let out = self
            .engine
            .set_wake_config(proxima_core::SetWakeConfigRequest {
                owner,
                personality_type_id: pb.personality_type_id,
                personality_instance_id: proxima_core::PersonalityInstanceId::new(instance_id),
                wake_filters: filters,
            })
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(SetWakeConfigResponse { status: out.status }))
    }

    async fn list_personality_instances(
        &self,
        request: Request<ListPersonalityInstancesRequest>,
    ) -> Result<Response<ListPersonalityInstancesResponse>, Status> {
        let pb = request.into_inner();
        let owner = owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?;
        let rows = self
            .engine
            .list_personality_instances(&owner, pb.personality_type_id.as_deref())
            .await
            .map_err(protocol_error_to_status)?;
        let instances = rows
            .into_iter()
            .map(personality_instance_to_proto)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(ListPersonalityInstancesResponse {
            instances,
        }))
    }
}

fn uuid_from_str(value: &str) -> Result<uuid::Uuid, Status> {
    uuid::Uuid::parse_str(value).map_err(|e| Status::invalid_argument(e.to_string()))
}

fn personality_instance_to_proto(
    row: proxima_core::PersonalityInstanceRow,
) -> Result<PersonalityInstance, Status> {
    let wake_filters = row
        .wake_filters
        .into_iter()
        .map(wake_filter_to_proto)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PersonalityInstance {
        owner: Some(owner_to_proto(&row.owner)),
        personality_type_id: row.personality_type_id,
        personality_instance_id: row.personality_instance_id.into_inner().to_string(),
        current_self_perspective_memory_id: row
            .current_self_perspective_memory_id
            .into_inner()
            .to_string(),
        display_name: row.display_name,
        status: row.status,
        wake_filters,
    })
}

fn wake_filter_from_proto(pb: WakeFilter) -> Result<proxima_core::WakeFilter, Status> {
    let version = u16::try_from(pb.version)
        .map_err(|_| Status::invalid_argument("wake filter version too large"))?;
    let kind = pb
        .kind
        .ok_or_else(|| Status::invalid_argument("missing wake filter kind"))?;
    Ok(match kind {
        pb::wake_filter::Kind::OnMemory(OnMemoryFilter {
            schema_id,
            authored_by,
            probability,
        }) => proxima_core::WakeFilter::OnMemory {
            version,
            schema_id: proxima_core::SchemaId::new(schema_id),
            authored_by: authored_by
                .map(author_filter_from_proto)
                .transpose()?
                .unwrap_or(proxima_core::AuthorFilter::Any),
            probability,
        },
        pb::wake_filter::Kind::OnEdge(OnEdgeFilter {
            relation_id,
            source,
            target,
            probability,
        }) => proxima_core::WakeFilter::OnEdge {
            version,
            relation_id,
            source: source
                .map(wake_target_from_proto)
                .transpose()?
                .unwrap_or(proxima_core::WakeTarget::Any),
            target: target
                .map(wake_target_from_proto)
                .transpose()?
                .unwrap_or(proxima_core::WakeTarget::Any),
            probability,
        },
        pb::wake_filter::Kind::Custom(CustomFilter {
            kind_id,
            params_json,
            probability,
        }) => proxima_core::WakeFilter::Custom {
            version,
            kind_id,
            params: serde_json::from_slice(&params_json)
                .map_err(|e| Status::invalid_argument(format!("params_json: {e}")))?,
            probability,
        },
    })
}

fn wake_filter_to_proto(core: proxima_core::WakeFilter) -> Result<WakeFilter, Status> {
    let (version, kind) = match core {
        proxima_core::WakeFilter::OnMemory {
            version,
            schema_id,
            authored_by,
            probability,
        } => (
            version,
            pb::wake_filter::Kind::OnMemory(OnMemoryFilter {
                schema_id: schema_id.into_inner(),
                authored_by: Some(author_filter_to_proto(authored_by)),
                probability,
            }),
        ),
        proxima_core::WakeFilter::OnEdge {
            version,
            relation_id,
            source,
            target,
            probability,
        } => (
            version,
            pb::wake_filter::Kind::OnEdge(OnEdgeFilter {
                relation_id,
                source: Some(wake_target_to_proto(source)),
                target: Some(wake_target_to_proto(target)),
                probability,
            }),
        ),
        proxima_core::WakeFilter::Custom {
            version,
            kind_id,
            params,
            probability,
        } => (
            version,
            pb::wake_filter::Kind::Custom(CustomFilter {
                kind_id,
                params_json: serde_json::to_vec(&params)
                    .map_err(|e| Status::internal(e.to_string()))?,
                probability,
            }),
        ),
    };
    Ok(WakeFilter {
        version: u32::from(version),
        kind: Some(kind),
    })
}

fn author_filter_from_proto(pb: AuthorFilter) -> Result<proxima_core::AuthorFilter, Status> {
    let kind = pb
        .kind
        .ok_or_else(|| Status::invalid_argument("missing author filter kind"))?;
    Ok(match kind {
        pb::author_filter::Kind::Any(_) => proxima_core::AuthorFilter::Any,
        pb::author_filter::Kind::External(_) => proxima_core::AuthorFilter::External,
        pb::author_filter::Kind::Personality(PersonalityAuthor {
            personality_type_id,
            personality_instance_id,
        }) => proxima_core::AuthorFilter::Personality {
            personality_type_id,
            personality_instance_id: personality_instance_id
                .as_deref()
                .map(uuid_from_str)
                .transpose()?
                .map(proxima_core::PersonalityInstanceId::new),
        },
    })
}

fn author_filter_to_proto(core: proxima_core::AuthorFilter) -> AuthorFilter {
    let kind = match core {
        proxima_core::AuthorFilter::Any => pb::author_filter::Kind::Any(true),
        proxima_core::AuthorFilter::External => pb::author_filter::Kind::External(true),
        proxima_core::AuthorFilter::Personality {
            personality_type_id,
            personality_instance_id,
        } => pb::author_filter::Kind::Personality(PersonalityAuthor {
            personality_type_id,
            personality_instance_id: personality_instance_id.map(|id| id.into_inner().to_string()),
        }),
    };
    AuthorFilter { kind: Some(kind) }
}

fn wake_target_from_proto(pb: WakeTarget) -> Result<proxima_core::WakeTarget, Status> {
    let kind = pb
        .kind
        .ok_or_else(|| Status::invalid_argument("missing wake target kind"))?;
    Ok(match kind {
        pb::wake_target::Kind::Any(_) => proxima_core::WakeTarget::Any,
        pb::wake_target::Kind::SelfPerspective(_) => proxima_core::WakeTarget::SelfPerspective,
        pb::wake_target::Kind::MemoryId(id) => proxima_core::WakeTarget::Memory {
            memory_id: proxima_core::MemoryId::new(uuid_from_str(&id)?),
        },
        pb::wake_target::Kind::GoalId(id) => proxima_core::WakeTarget::Goal {
            goal_id: proxima_core::GoalId::new(uuid_from_str(&id)?),
        },
    })
}

fn wake_target_to_proto(core: proxima_core::WakeTarget) -> WakeTarget {
    let kind = match core {
        proxima_core::WakeTarget::Any => pb::wake_target::Kind::Any(true),
        proxima_core::WakeTarget::SelfPerspective => pb::wake_target::Kind::SelfPerspective(true),
        proxima_core::WakeTarget::Memory { memory_id } => {
            pb::wake_target::Kind::MemoryId(memory_id.into_inner().to_string())
        }
        proxima_core::WakeTarget::Goal { goal_id } => {
            pb::wake_target::Kind::GoalId(goal_id.into_inner().to_string())
        }
    };
    WakeTarget { kind: Some(kind) }
}
