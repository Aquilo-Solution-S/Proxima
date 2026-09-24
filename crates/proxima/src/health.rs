//! Anonymous liveness and readiness probes (`PROXIMA_HEALTH_ENDPOINTS`).
//!
//! Served on the MCP listener below the Host guard and bearer auth: an
//! orchestrator probes the pod address with no token. Neither answer carries
//! data — a status line — so nothing is gained by reaching one from a page
//! the Host guard would otherwise have refused.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// Process liveness.
pub const HEALTHZ_PATH: &str = "/healthz";
/// Readiness: the database answers and the runtime is not shutting down.
pub const READYZ_PATH: &str = "/readyz";

/// Longest a readiness probe waits on the database. Below the usual probe
/// timeout, so a stalled pool reads as "not ready", not as a hung probe.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct Probe {
    pool: PgPool,
    cancel: CancellationToken,
}

pub(crate) fn router(pool: PgPool, cancel: CancellationToken) -> Router {
    Router::new()
        .route(HEALTHZ_PATH, get(|| async { (StatusCode::OK, "ok") }))
        .route(READYZ_PATH, get(ready))
        .with_state(Probe { pool, cancel })
}

async fn ready(State(probe): State<Probe>) -> (StatusCode, &'static str) {
    if probe.cancel.is_cancelled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down");
    }
    match tokio::time::timeout(READY_TIMEOUT, sqlx::query("SELECT 1").execute(&probe.pool)).await {
        Ok(Ok(_)) => (StatusCode::OK, "ready"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    fn unreachable_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://proxima@127.0.0.1:1/proxima")
            .expect("lazy pool")
    }

    async fn status(router: Router, path: &str) -> StatusCode {
        router
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn liveness_is_unconditional_and_readiness_follows_the_database() {
        let cancel = CancellationToken::new();
        let probes = router(unreachable_pool(), cancel.clone());
        assert_eq!(status(probes.clone(), HEALTHZ_PATH).await, StatusCode::OK);
        assert_eq!(
            status(probes.clone(), READYZ_PATH).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        cancel.cancel();
        assert_eq!(
            status(probes, READYZ_PATH).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
