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
    self, ChangeEvent, EventHistoryRequest, EventHistoryResponse, EventIngestRequest,
    EventIngestResponse, GoalWriteRequest, GoalWriteResponse,
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, ListPersonalityInstancesRequest,
    ListPersonalityInstancesResponse, PersonalityInstance, ProvisionOwnerRequest,
    ProvisionOwnerResponse, QueryRequest,
    QueryResponse, SchemaRequest, SchemaResponse, SetWakeConfigRequest, SetWakeConfigResponse,
    SubscribeRequest, TombstonePersonalityRequest, TombstonePersonalityResponse,
    engine_server::Engine as EngineTrait,
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
        let _ = request.into_inner();
        Err(Status::unimplemented(
            "SetWakeConfig was removed by the Phase 1a WakeEntry migration",
        ))
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
            .list_personality_instances(&owner, pb.personality_type_id.as_deref(), pb.include_tombstoned)
            .await
            .map_err(protocol_error_to_status)?;
        let registry = self.engine.registry();
        let instances = rows
            .into_iter()
            .map(|row| {
                let flavor = registry
                    .flavor_for_personality_type(&row.personality_type_id)
                    .ok_or_else(|| {
                        Status::internal(format!(
                            "no FlavorDescriptor for personality_type_id {}",
                            row.personality_type_id,
                        ))
                    })?;
                personality_instance_to_proto(row, flavor)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(ListPersonalityInstancesResponse {
            instances,
        }))
    }

    async fn tombstone_personality(
        &self,
        request: Request<TombstonePersonalityRequest>,
    ) -> Result<Response<TombstonePersonalityResponse>, Status> {
        let pb = request.into_inner();
        let owner = owner_from_proto(
            pb.owner
                .ok_or_else(|| Status::invalid_argument("missing owner"))?,
        )?;
        let instance_id = uuid_from_str(&pb.personality_instance_id)?;
        let out = self
            .engine
            .tombstone_personality(proxima_core::TombstonePersonalityRequest {
                owner,
                personality_type_id: pb.personality_type_id,
                personality_instance_id: proxima_core::PersonalityInstanceId::new(instance_id),
            })
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(TombstonePersonalityResponse {
            status: out.status,
            idempotent_replay: out.idempotent_replay,
        }))
    }
}

fn uuid_from_str(value: &str) -> Result<uuid::Uuid, Status> {
    uuid::Uuid::parse_str(value).map_err(|e| Status::invalid_argument(e.to_string()))
}

fn personality_instance_to_proto(
    row: proxima_core::PersonalityInstanceRow,
    flavor: &proxima_core::FlavorDescriptor,
) -> Result<PersonalityInstance, Status> {
    Ok(PersonalityInstance {
        owner: Some(owner_to_proto(&row.owner)),
        personality_type_id: row.personality_type_id,
        personality_instance_id: row.personality_instance_id.into_inner().to_string(),
        current_self_perspective_memory_id: row
            .current_root_perspective_memory_id
            .into_inner()
            .to_string(),
        display_name: row.display_name,
        status: row.status,
        wake_filters: Vec::new(),
        flavor: Some(flavor_descriptor_to_proto(flavor)),
    })
}

fn flavor_descriptor_to_proto(
    descriptor: &proxima_core::FlavorDescriptor,
) -> pb::FlavorDescriptor {
    pb::FlavorDescriptor {
        flavor_id: descriptor.flavor_id.clone(),
        display_name: descriptor.display_name.clone(),
        package_version: descriptor.package_version.clone(),
        author: descriptor.author.clone(),
        provenance: Some(flavor_provenance_to_proto(&descriptor.provenance)),
    }
}

fn flavor_provenance_to_proto(
    provenance: &proxima_core::FlavorProvenance,
) -> pb::FlavorProvenance {
    use pb::flavor_provenance::{Builtin, Kind, Local, Marketplace};
    let kind = match provenance {
        proxima_core::FlavorProvenance::Builtin => Kind::Builtin(Builtin {}),
        proxima_core::FlavorProvenance::Marketplace { source_url } => {
            Kind::Marketplace(Marketplace {
                source_url: source_url.clone(),
            })
        }
        proxima_core::FlavorProvenance::Local { workspace_path } => Kind::Local(Local {
            workspace_path: workspace_path.clone(),
        }),
    };
    pb::FlavorProvenance { kind: Some(kind) }
}
