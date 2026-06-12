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
    use proxima_core::verbs::query::MemoryStore;

    Engine::new(schema_registry_with_config(|_| {}), MemoryStore::new())
        .with_storage(Arc::new(storage))
}

/// Build an `Engine` whose schema registry layers `extra` flavor
/// registrations *before* `proxima_code::register`. Tauri Shell uses
/// this to add substrate (`proxima_mcp_substrate::register`) so the
/// engine's snapshot path joins agent-note sidecars; headless code
/// callers keep using `build_engine` to stay substrate-free.
/// Authentication lives at the transport edge.
#[must_use]
pub fn build_engine_with(
    storage: PgStorage,
    extra: impl FnOnce(&mut proxima_core::FlavorRegistry),
) -> Engine {
    use proxima_core::verbs::query::MemoryStore;

    Engine::new(schema_registry_with_config(extra), MemoryStore::new())
        .with_storage(Arc::new(storage))
}
