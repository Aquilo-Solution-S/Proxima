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

use crate::convert::{
    change_event_to_proto, event_history_request_from_proto, event_history_response_to_proto,
    event_ingest_request_from_proto, event_ingest_response_to_proto, goal_write_request_from_proto,
    goal_write_response_to_proto, protocol_error_to_status, query_request_from_proto,
    query_response_to_proto, schema_request_from_proto, schema_response_to_proto,
    subscribe_request_from_proto,
};
use crate::pb::{
    ChangeEvent, EventHistoryRequest, EventHistoryResponse, EventIngestRequest,
    EventIngestResponse, GoalWriteRequest, GoalWriteResponse, QueryRequest, QueryResponse,
    SchemaRequest, SchemaResponse, SubscribeRequest,
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
}
