//! Test helpers for the example flavor: plain functions, not tools.
//!
//! Behind `cfg(any(test, feature = "testkit"))`. A test and tutorial
//! fixture, not a product.

use proxima_core::{AuthzContext, Engine, OwnerId, OwnerScope, ProtocolError};
use uuid::Uuid;

use crate::NoteV1;

/// Static SQL: register one notebook for its owner, on an owner-scoped
/// transaction.
const REGISTER_NOTEBOOK_SQL: &str =
    "INSERT INTO proxima_example.notebooks (notebook_id, owner_id, name) VALUES ($1, $2, $3)";

/// Register a notebook named `name` for `owner` and return its id.
///
/// # Errors
///
/// Storage errors from the owner-scoped transaction or the insert.
pub async fn register_notebook(
    pool: &sqlx::PgPool,
    scope: &OwnerScope,
    owner: OwnerId,
    name: &str,
) -> Result<Uuid, Box<dyn std::error::Error + Send + Sync>> {
    let notebook_id = Uuid::now_v7();
    let mut tx = proxima::host::begin_owner_transaction(pool, scope).await?;
    sqlx::query(REGISTER_NOTEBOOK_SQL)
        .bind(notebook_id)
        .bind(owner.into_inner())
        .bind(name)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(notebook_id)
}

/// Remember one note for `owner` through the Engine and return its memory
/// id. The Engine's scope fence and liveness probe run, as on every write.
///
/// # Errors
///
/// The Engine's refusal or storage error.
pub async fn remember_note(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    note: &NoteV1,
) -> Result<proxima_core::MemoryId, ProtocolError> {
    let outcome = engine
        .ingest_fact(
            authz,
            proxima::FactWrite::new(owner, "proxima-example/testkit", note),
        )
        .await?;
    Ok(outcome.memory_id)
}

/// Boot the example flavor as a single-owner app on a split-role database.
///
/// # Errors
///
/// The boot's config, storage or migration error.
pub async fn build_engine(
    runtime_url: &str,
    platform_url: &str,
    owner: OwnerId,
) -> Result<proxima::BuiltProxima, proxima::ProximaError> {
    proxima::Proxima::<crate::ExampleFlavor>::app()
        .database_url(runtime_url)
        .store_number(0)
        .platform_database_url(platform_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .served_surface(proxima::ServedSurface::All)
        .build()
        .await
}
