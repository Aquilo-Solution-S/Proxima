//! Anonymous liveness and readiness probes (`PROXIMA_HEALTH_ENDPOINTS`).
//!
//! Served on the MCP listener below the Host guard and bearer auth: an
//! orchestrator probes the pod address with no token. Answers contain only
//! process readiness, sanitized embedding-configuration status codes, and
//! the object purge queue's counts as the in-process drain last saw them.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use proxima_core::Engine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::object_maintenance::ObjectPurgeHealth;

/// Process liveness.
pub(crate) const HEALTHZ_PATH: &str = "/healthz";
/// Readiness: database available and runtime active. A paused embedding client
/// retains HTTP 200 with a sanitized degraded status.
pub(crate) const READYZ_PATH: &str = "/readyz";

/// Longest a readiness probe waits on the database. Below the usual probe
/// timeout, so a stalled pool reads as "not ready", not as a hung probe.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct Probe {
    pool: PgPool,
    cancel: CancellationToken,
    engine: Arc<Engine>,
    objects: ObjectPurgeHealth,
}

pub(crate) fn router(
    pool: PgPool,
    cancel: CancellationToken,
    engine: Arc<Engine>,
    objects: ObjectPurgeHealth,
) -> Router {
    Router::new()
        .route(HEALTHZ_PATH, get(|| async { (StatusCode::OK, "ok") }))
        .route(READYZ_PATH, get(ready))
        .with_state(Probe {
            pool,
            cancel,
            engine,
            objects,
        })
}

async fn ready(State(probe): State<Probe>) -> (StatusCode, String) {
    if probe.cancel.is_cancelled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down".into());
    }
    match tokio::time::timeout(READY_TIMEOUT, sqlx::query("SELECT 1").execute(&probe.pool)).await {
        Ok(Ok(_)) => {
            let faults = probe.engine.embedding_configuration_faults();
            let mut answer = if faults.is_empty() {
                "ready".to_owned()
            } else {
                let statuses = faults
                    .into_iter()
                    .map(|fault| fault.status().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("degraded: embedding configuration fault: HTTP {statuses}")
            };
            // Counts only, from the object-purge feature's last pass: the
            // probe is anonymous and must not touch the queue itself.
            // Readiness asks whether this replica can serve traffic now;
            // background purge debt does not change that, so it never
            // degrades the answer. Alert on `stuck` instead.
            if let Some(objects) = probe.objects.snapshot() {
                answer.push('\n');
                answer.push_str(&objects.line());
            }
            (StatusCode::OK, answer)
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_failure_and_shutdown_remain_unavailable() {
        let pool = PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        pool.close().await;
        let probe = Probe {
            pool,
            cancel: CancellationToken::new(),
            engine: Arc::new(Engine::new(
                proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
            )),
            objects: ObjectPurgeHealth::default(),
        };
        assert_eq!(
            ready(State(probe.clone())).await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "database unavailable".into()
            )
        );
        probe.cancel.cancel();
        assert_eq!(
            ready(State(probe)).await,
            (StatusCode::SERVICE_UNAVAILABLE, "shutting down".into())
        );
    }
}
