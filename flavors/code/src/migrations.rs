//! Per-flavor migrations. The composite binary runs core migrations
//! first, then each linked flavor's `migrator()` once. sqlx tracks
//! versions in the flavor's own table.
//!
//! Tracking table is `public._sqlx_migrations_proxima_code`, not
//! `proxima_code`: baselines `DROP SCHEMA proxima_code`, and the ledger
//! must survive. `ignore_missing = true` forgives orphaned rows after a
//! lane is squashed (docs/how-to/migrations.md).

/// Embedded migration set (`flavors/code/migrations/`).
///
/// `SQLx` checksums content, not filename: do not edit a comment byte in
/// an already-applied file.
#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut m = sqlx::migrate!("./migrations");
    m.dangerous_set_table_name("public._sqlx_migrations_proxima_code");
    m.set_ignore_missing(true);
    m
}
