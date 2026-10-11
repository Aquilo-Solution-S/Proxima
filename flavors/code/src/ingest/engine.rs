#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_storage_pg::PgStorage;

use super::schemas::schema_registry_with_config;

/// Convenience: build a fully-wired `Engine` over a `PgStorage` and the
/// proxima-code flavor's schemas plus the helper-required cited /
/// citation schemas. Used by tests and the composite binary.
/// Authentication lives at the transport edge.
#[must_use]
pub fn build_engine(storage: PgStorage) -> Engine {
    Engine::new(schema_registry_with_config(|_| {}))
        .with_storage_ports(Arc::new(storage).storage_ports())
}
