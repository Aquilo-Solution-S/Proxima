//! Anonymous liveness and readiness probes (`PROXIMA_HEALTH_ENDPOINTS`).
//!
//! Served on the MCP listener below the Host guard and bearer auth: an
//! orchestrator probes the pod address with no token. Answers contain only
//! process readiness and sanitized embedding-configuration status codes.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use proxima_core::Engine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// Process liveness.
pub const HEALTHZ_PATH: &str = "/healthz";
/// Readiness: database available, runtime active, and no observed embedding
/// client paused for a configuration fault.
pub const READYZ_PATH: &str = "/readyz";

/// Longest a readiness probe waits on the database. Below the usual probe
/// timeout, so a stalled pool reads as "not ready", not as a hung probe.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct Probe {
    pool: PgPool,
    cancel: CancellationToken,
    engine: Arc<Engine>,
}

pub(crate) fn router(pool: PgPool, cancel: CancellationToken, engine: Arc<Engine>) -> Router {
    Router::new()
        .route(HEALTHZ_PATH, get(|| async { (StatusCode::OK, "ok") }))
        .route(READYZ_PATH, get(ready))
        .with_state(Probe {
            pool,
            cancel,
            engine,
        })
}

async fn ready(State(probe): State<Probe>) -> (StatusCode, String) {
    if probe.cancel.is_cancelled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down".into());
    }
    match tokio::time::timeout(READY_TIMEOUT, sqlx::query("SELECT 1").execute(&probe.pool)).await {
        Ok(Ok(_)) => {
            let faults = probe.engine.embedding_configuration_faults();
            if faults.is_empty() {
                (StatusCode::OK, "ready".into())
            } else {
                let statuses = faults
                    .into_iter()
                    .map(|fault| fault.status().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("embedding configuration fault: HTTP {statuses}"),
                )
            }
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable".into(),
        ),
    }
}
