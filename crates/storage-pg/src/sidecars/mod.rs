//! Postgres-owned sidecar registration.
//!
//! Core owns schema metadata. This module owns backend-specific sidecar
//! coverage so flavor composition can remain build-time and storage can
//! stay out of `proxima-core`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, FactPayload, GoalId,
    GoalPayload, MemoryId, PerspectivePayload, SchemaId, SchemaVersion, SidecarPayload,
    StorageError,
};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::pg_ident::PgIdent;
use crate::verbs::fact_ingest::PgFactSidecar;

mod core_sidecars;
mod dispatch;
mod entry;
mod frozen;
mod macros;
mod read_ctx;
mod registry;
mod sql;
mod traits;

#[cfg(test)]
mod tests;

pub use core_sidecars::{core_pg_sidecars, register_core_pg_sidecars};
pub use entry::{PgSidecarEntry, PgSidecarKey};
pub use frozen::PgSidecarRegistryFrozen;
pub use read_ctx::PgSidecarReadCtx;
#[cfg(test)]
pub(crate) use read_ctx::validate_sidecar_read_sql;
pub use registry::PgSidecarRegistry;
pub use sql::{bytes32, int_to_u32, int_to_u64, memory_insert_sql, memory_select_batch_sql};
pub use traits::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgGoalSidecar, PgMemoryPayload, PgMemorySidecar,
};

pub type PgSidecarFuture<'t> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;
pub type PgMemoryPayloadFuture<'t> =
    Pin<Box<dyn Future<Output = Result<Option<SidecarPayload>, StorageError>> + Send + 't>>;
pub type PgMemoryPayloadBatchFuture<'t> = Pin<
    Box<dyn Future<Output = Result<Vec<(MemoryId, SidecarPayload)>, StorageError>> + Send + 't>,
>;
