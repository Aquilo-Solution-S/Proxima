use std::sync::Arc;

use futures_util::StreamExt;
use proxima_core::auth::Credentials;
use proxima_core::error::ProtocolError;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::{SchemaRequest, SchemaResponse};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{ChangeEvent, Engine};
use tauri::State;
use tauri::ipc::Channel;

#[tauri::command]
#[specta::specta]
pub async fn schema(engine: State<'_, Arc<Engine>>) -> Result<SchemaResponse, ProtocolError> {
    crate::perf::ipc::record(
        "schema",
        0,
        async move { Ok(engine.schema(&SchemaRequest)) },
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn query(
    engine: State<'_, Arc<Engine>>,
    req: QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("query", req_bytes, async move {
        engine.query(&Credentials::None, &req).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn event_history(
    engine: State<'_, Arc<Engine>>,
    req: EventHistoryRequest,
) -> Result<EventHistoryResponse, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("event_history", req_bytes, async move {
        engine.event_history(&Credentials::None, &req).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn event_ingest(
    engine: State<'_, Arc<Engine>>,
    draft: EventDraft,
) -> Result<EventIngestOutcome, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&draft);
    crate::perf::ipc::record("event_ingest", req_bytes, async move {
        engine.event_ingest(&Credentials::None, draft).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn goal_write(
    engine: State<'_, Arc<Engine>>,
    draft: GoalDraft,
) -> Result<GoalWriteOutcome, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&draft);
    crate::perf::ipc::record("goal_write", req_bytes, async move {
        engine.write_goal(&Credentials::None, draft).await
    })
    .await
}

/// Subscribe — engine returns a `Stream<Item = ChangeEvent>`; we
/// spawn a forwarder onto the caller-supplied `Channel<ChangeEvent>`
/// so events flow back through Tauri IPC. The handler returns when
/// the subscription is established; the stream lifetime is bound to
/// the spawned task and ends when storage closes its end (or the JS
/// side drops the channel, surfaced as a send error).
#[tauri::command]
#[specta::specta]
pub async fn subscribe(
    engine: State<'_, Arc<Engine>>,
    req: SubscribeRequest,
    on_event: Channel<ChangeEvent>,
) -> Result<(), ProtocolError> {
    let stream = engine.subscribe(&Credentials::None, req).await?;
    tokio::spawn(async move {
        let mut inbound = stream;
        while let Some(event) = inbound.next().await {
            if on_event.send(event).is_err() {
                break;
            }
        }
    });
    Ok(())
}
