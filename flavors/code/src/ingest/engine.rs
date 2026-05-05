use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_storage_pg::PgStorage;

use super::schemas::schema_registry;

/// Convenience: build a fully-wired `Engine` over a `PgStorage` and the
/// proxima-code flavor's schemas plus the helper-required cited /
/// citation schemas. Used by tests and the composite binary.
#[must_use]
pub fn build_engine(storage: PgStorage, auth: Box<dyn proxima_core::auth::AuthResolver>) -> Engine {
    use proxima_core::verbs::query::MemoryStore;

    Engine::new(schema_registry(), MemoryStore::new(), auth).with_storage(Arc::new(storage))
}
