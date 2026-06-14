//! gRPC service implementation for the Engine trait.

use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use proxima_core::{Authenticator, AuthzContext, Engine, RevalidationConfig, revalidate_stream};

use crate::convert::refs::{owner_to_proto, principal_from_proto};
use crate::convert::{
    change_event_to_proto, event_history_request_from_proto, event_history_response_to_proto,
    event_ingest_request_from_proto, event_ingest_response_to_proto, goal_write_request_from_proto,
    goal_write_response_to_proto, protocol_error_to_status, query_request_from_proto,
    query_response_to_proto, schema_request_from_proto, schema_response_to_proto,
    subscribe_request_from_proto, wake_entry_draft_from_proto, wake_entry_to_proto,
};
use crate::pb::{
    ChangeEvent, EventHistoryRequest, EventHistoryResponse, EventIngestRequest,
    EventIngestResponse, GoalWriteRequest, GoalWriteResponse, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListPersonalityInstancesRequest,
    ListPersonalityInstancesResponse, PersonalityInstance, QueryRequest, QueryResponse,
    SchemaRequest, SchemaResponse, SetWakeEntriesRequest, SetWakeEntriesResponse, SubscribeRequest,
    TombstonePersonalityRequest, TombstonePersonalityResponse,
    engine_server::Engine as EngineTrait,
};

/// gRPC server wrapper for the Engine.
pub struct EngineGrpcServer {
    engine: Arc<Engine>,
    authz: AuthzContext,
    authenticator: Option<Arc<dyn Authenticator>>,
    revalidation: RevalidationConfig,
}

impl std::fmt::Debug for EngineGrpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineGrpcServer")
            .field("authz", &self.authz)
            .field("has_authenticator", &self.authenticator.is_some())
            .field("revalidation", &self.revalidation)
            .finish_non_exhaustive()
    }
}

impl EngineGrpcServer {
    /// Create a new dev-only gRPC server wrapping the given engine.
    ///
    /// The caller supplies the trusted local authorization context;
    /// this transport performs no per-RPC identity extraction.
    #[must_use]
    pub fn new(engine: Arc<Engine>, authz: AuthzContext) -> Self {
        Self {
            engine,
            authz,
            authenticator: None,
            revalidation: RevalidationConfig::default(),
        }
    }

    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    #[must_use]
    pub fn with_revalidation_config(mut self, revalidation: RevalidationConfig) -> Self {
        self.revalidation = revalidation;
        self
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
            .query(&self.authz, &req)
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
            .subscribe(&self.authz, req)
            .await
            .map_err(protocol_error_to_status)?;
        let stream = revalidate_stream(
            stream,
            self.authz.identity.clone(),
            self.authenticator.clone(),
            self.revalidation,
        );

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
            .write_goal(&self.authz, req)
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
            .event_history(&self.authz, &req)
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
            .event_ingest(&self.authz, req)
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

    async fn instantiate_personality(
        &self,
        request: Request<InstantiatePersonalityRequest>,
    ) -> Result<Response<InstantiatePersonalityResponse>, Status> {
        let pb = request.into_inner();
        let principal = principal_from_proto(
            pb.principal
                .ok_or_else(|| Status::invalid_argument("missing principal"))?,
        )?;
        let out = self
            .engine
            .instantiate_personality(
                &self.authz,
                proxima_core::InstantiatePersonalityRequest {
                    principal,
                    org_id: None,
                    display_name: pb.display_name,
                    purpose: pb.purpose,
                },
            )
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(InstantiatePersonalityResponse {
            personality_instance_id: out.instance_id.into_inner().to_string(),
        }))
    }

    async fn set_wake_entries(
        &self,
        request: Request<SetWakeEntriesRequest>,
    ) -> Result<Response<SetWakeEntriesResponse>, Status> {
        let pb = request.into_inner();
        let principal = principal_from_proto(
            pb.principal
                .ok_or_else(|| Status::invalid_argument("missing principal"))?,
        )?;
        let personality_instance_id =
            proxima_core::PersonalityInstanceId::new(uuid_from_str(&pb.personality_instance_id)?);
        let entries = pb
            .entries
            .into_iter()
            .map(|entry| wake_entry_draft_from_proto(entry, personality_instance_id))
            .collect::<Result<Vec<_>, _>>()?;
        let out = self
            .engine
            .set_wake_entries(
                &self.authz,
                &proxima_core::SetWakeEntriesRequest {
                    principal,
                    org_id: None,
                    personality_instance_id,
                    entries,
                },
            )
            .await
            .map_err(protocol_error_to_status)?;
        Ok(Response::new(SetWakeEntriesResponse {
            active_entries: out.active_entries,
        }))
    }

    async fn list_personality_instances(
        &self,
        request: Request<ListPersonalityInstancesRequest>,
    ) -> Result<Response<ListPersonalityInstancesResponse>, Status> {
        let pb = request.into_inner();
        let principal = principal_from_proto(
            pb.principal
                .ok_or_else(|| Status::invalid_argument("missing principal"))?,
        )?;
        let rows = self
            .engine
            .list_personality_instances(&self.authz, &principal, pb.include_tombstoned)
            .await
            .map_err(protocol_error_to_status)?;
        let instances = rows
            .into_iter()
            .map(|row| Ok::<_, Status>(personality_instance_to_proto(row)))
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
        let principal = principal_from_proto(
            pb.principal
                .ok_or_else(|| Status::invalid_argument("missing principal"))?,
        )?;
        let instance_id = uuid_from_str(&pb.personality_instance_id)?;
        let out = self
            .engine
            .tombstone_personality(
                &self.authz,
                proxima_core::TombstonePersonalityRequest {
                    principal,
                    org_id: None,
                    personality_instance_id: proxima_core::PersonalityInstanceId::new(instance_id),
                },
            )
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

fn personality_instance_to_proto(row: proxima_core::PersonalityInstanceRow) -> PersonalityInstance {
    PersonalityInstance {
        owner: Some(owner_to_proto(&row.owner)),
        personality_instance_id: row.personality_instance_id.into_inner().to_string(),
        current_root_perspective_memory_id: row
            .current_root_perspective_memory_id
            .into_inner()
            .to_string(),
        display_name: row.display_name,
        status: row.status.as_str().to_string(),
        flavor: None,
        wake_entries: row.wake_entries.iter().map(wake_entry_to_proto).collect(),
    }
}
