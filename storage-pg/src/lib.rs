//! Postgres `Storage` impl.
//!
//! See docs/07-storage.md and the `Storage` trait in
//! `proxima_core`.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, migration runner, and
//! outbox plumbing, then delegates each `Storage` trait method to its
//! per-verb implementation.

use std::sync::Arc;
use std::time::Duration;

use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{ChangeEvent, GoalId, Owner, Storage, StorageError, StorageHandle};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::broadcast;

mod authorship;
mod error;
pub mod outbox;
mod verbs;

use outbox::BROADCAST_CAPACITY;

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
    tx: broadcast::Sender<ChangeEvent>,
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        let tx = broadcast::channel(BROADCAST_CAPACITY).0;

        Ok(Self { pool, tx })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub fn into_handle(self) -> StorageHandle {
        Arc::new(self)
    }

    /// Return a fresh broadcast receiver for `ChangeEvents`.
    /// Multiple calls produce independent receivers that each
    /// see all future events.
    #[must_use]
    pub fn changes(&self) -> broadcast::Receiver<ChangeEvent> {
        self.tx.subscribe()
    }

    /// Spawn the outbox publisher task.
    ///
    /// Opens a `PgListener` on the same pool, LISTENs on
    /// `outbox::NOTIFY_CHANNEL`, and publishes typed `ChangeEvent`s to
    /// the broadcast channel. On error, logs and reconnects with
    /// exponential backoff.
    ///
    /// # Errors
    ///
    /// This function always returns `Ok(())`; the listener task runs
    /// in the background and handles its own errors with backoff.
    pub fn start_outbox(&self) -> Result<(), StorageError> {
        let pool = self.pool.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            outbox::outbox_publisher(pool, tx).await;
        });
        Ok(())
    }

    /// Apply all pending migrations under
    /// `storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Storage for PgStorage {
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        verbs::event_ingest::ingest_event_atomic(&self.pool, draft).await
    }

    async fn write_goal_atomic(
        &self,
        draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::write_goal_atomic(&self.pool, draft).await
    }

    async fn supersede_goal_atomic(
        &self,
        prior: GoalId,
        draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::supersede_goal_atomic(&self.pool, prior, draft).await
    }

    async fn subscribe_changes(
        &self,
        owner: &Owner,
        since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        verbs::subscribe::subscribe_changes(&self.pool, &self.tx, owner, since).await
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, req).await
    }
}
