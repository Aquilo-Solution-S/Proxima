//! Per-flavor migrations. The composite binary runs core migrations
//! (`proxima_storage_pg::PgStorage::run_migrations`) first, then iterates
//! linked flavors and applies each `migrator()` once. Idempotent — sqlx
//! tracks applied versions in `_sqlx_migrations`.
//!
//! `ignore_missing = true` is load-bearing: core and every flavor share
//! the default `_sqlx_migrations` tracking table, so each `Migrator` sees
//! versions it didn't author. Without `ignore_missing`, the flavor would
//! reject the run with `VersionMissing(<core version>)`. The flavor's
//! own version-set is still validated; we only relax the cross-author
//! check.

/// Embedded migration set. Compile-time `include_str!`s every file under
/// `flavors/code/migrations/`. Version numbers must not collide with core
/// (core uses `20260504000001..3`; flavor starts at `20260504000010`).
#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut m = sqlx::migrate!("./migrations");
    m.set_ignore_missing(true);
    m
}
