//! Embedded engine wiring for the desktop shell.
//!
//! The shell holds an `Arc<Engine>` via `tauri::Builder::manage` and
//! exposes the five verb surfaces from docs/14 as `#[tauri::command]`
//! handlers. tauri-specta generates the matching TS bindings into
//! `../src/lib/bindings.ts` on debug builds — Rust traits remain the
//! source of truth (docs/09 §Generation pipeline).
//!
//! v1 uses Postgres-backed storage (mandatory via `DATABASE_URL`)
//! with the proxima-code flavor. `NoopStorage` was removed once
//! settings persistence became required.

pub mod config;
pub mod secrets;

use std::sync::Arc;

use futures_util::StreamExt;
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::error::ProtocolError;
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::{SchemaRequest, SchemaResponse};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{ChangeEvent, Engine, OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use tauri::ipc::Channel;
use tauri::State;
use tauri_specta::{Builder, collect_commands};
use uuid::Uuid;

/// Build the embedded engine for v1 desktop.
///
/// Connects to Postgres (mandatory via `DATABASE_URL`), runs migrations,
/// starts the outbox listener, and wires the proxima-code flavor's
/// schemas via `proxima_code::build_engine`. Returns both the
/// `Arc<Engine>` (for verb handlers) and an `Arc<PgStorage>` clone
/// (for settings commands) so the Tauri command layer can hold both
/// independently.
///
/// # Panics
///
/// Panics if `DATABASE_URL` is not set — settings persistence is
/// required for the desktop shell.
fn build_engine() -> (Arc<Engine>, Arc<PgStorage>) {
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    };

    let url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for the desktop shell — settings persistence is required");

    tauri::async_runtime::block_on(async {
        let pg = PgStorage::connect(&url)
            .await
            .expect("failed to connect to Postgres; check DATABASE_URL");
        pg.run_migrations()
            .await
            .expect("failed to run migrations");
        pg.start_outbox()
            .await
            .expect("failed to start outbox listener");

        let pg_for_settings = Arc::new(pg.clone());
        let auth = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(proxima_code::build_engine(pg, Box::new(auth)));

        // Validation step — non-fatal. See validate_at_boot.
        validate_at_boot(&pg_for_settings, &owner, &engine).await;

        (engine, pg_for_settings)
    })
}

/// Validate the loaded `AppConfig` at engine boot.
/// Loads settings from PG, assembles an `AppConfig`, and runs
/// `validate_config` against the engine. Failures are logged as
/// warnings only — the settings UI exists to fix broken config;
/// panicking would brick the app.
async fn validate_at_boot(pg: &Arc<PgStorage>, owner: &Owner, engine: &Engine) {
    let cfg = match crate::config::load_app_config(pg, owner).await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("could not load AppConfig at boot: {e}");
            return;
        }
    };
    if let Err(e) = crate::config::validate_config(&cfg, engine) {
        tracing::warn!(
            "AppConfig validation failed at boot — running with degraded \
             config; user must fix via settings UI: {e}"
        );
    } else {
        tracing::info!("AppConfig validated successfully at boot");
    }
}

#[tauri::command]
#[specta::specta]
async fn schema(engine: State<'_, Arc<Engine>>) -> Result<SchemaResponse, ProtocolError> {
    Ok(engine.schema(&SchemaRequest))
}

#[tauri::command]
#[specta::specta]
async fn query(
    engine: State<'_, Arc<Engine>>,
    req: QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    engine.query(&Credentials::None, &req).await
}

#[tauri::command]
#[specta::specta]
async fn event_ingest(
    engine: State<'_, Arc<Engine>>,
    draft: EventDraft,
) -> Result<EventIngestOutcome, ProtocolError> {
    engine.event_ingest(&Credentials::None, draft).await
}

#[tauri::command]
#[specta::specta]
async fn goal_write(
    engine: State<'_, Arc<Engine>>,
    draft: GoalDraft,
) -> Result<GoalWriteOutcome, ProtocolError> {
    engine.write_goal(&Credentials::None, draft).await
}

/// Subscribe — engine returns a `Stream<Item = ChangeEvent>`; we
/// spawn a forwarder onto the caller-supplied `Channel<ChangeEvent>`
/// so events flow back through Tauri IPC. The handler returns when
/// the subscription is established; the stream lifetime is bound to
/// the spawned task and ends when storage closes its end (or the JS
/// side drops the channel, surfaced as a send error).
#[tauri::command]
#[specta::specta]
async fn subscribe(
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

fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        schema,
        query,
        event_ingest,
        goal_write,
        subscribe,
    ])
}

/// Entry point for the Tauri application.
///
/// # Panics
///
/// Panics if the Tauri application fails to start (window creation,
/// plugin init, or context generation).
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    let (engine, pg) = build_engine();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(engine)
        .manage(pg)
        .invoke_handler(specta_builder().invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::specta_builder;

    /// Regenerate `proxima-shell/src/lib/bindings.ts` from the
    /// command surface. Run via `cargo test -p proxima-shell`. The
    /// emitted file is git-tracked so JS-only contributors see the
    /// types without compiling Rust; CI compares the regen against
    /// the committed file to catch missed regenerations.
    #[test]
    fn export_ts_bindings() {
        specta_builder()
            .export(
                specta_typescript::Typescript::default(),
                "../../frontend-core/src/bindings.ts",
            )
            .expect("failed to export TS bindings");
    }
}
