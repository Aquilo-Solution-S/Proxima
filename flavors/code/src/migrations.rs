//! Per-flavor migrations. The composite binary runs core migrations
//! (`proxima_storage_pg::PgStorage::run_migrations`) first, then iterates
//! linked flavors and applies each `migrator()` once. Idempotent — sqlx
//! tracks applied versions per flavor in the flavor's own tracking table.
//!
//! Since v0.0.7 this flavor records into its own tracking table,
//! `public._sqlx_migrations_proxima_code`, instead of the shared
//! `public._sqlx_migrations`; the migration facade
//! (`proxima::run_core_and_flavor_migrations`) moves a pre-split database's
//! rows over once before this migrator first runs. The table deliberately
//! lives in `public`, NOT in `proxima_code`: this flavor's baselines are
//! destructive (`DROP SCHEMA proxima_code` and rebuild), and a ledger inside
//! the flavor schema would be destroyed by the very migration it is
//! recording.
//!
//! `ignore_missing = true` stays on for a different reason than before the
//! split: it is what forgives orphaned ledger rows after a dev-cycle lane is
//! squashed under a fresh version number (docs/how-to/migrations.md). The
//! flavor's own version-set is still checksum-validated.

/// Embedded migration set. Compile-time `include_str!`s every file under
/// `flavors/code/migrations/`.
///
/// The set is a single v0.0.7 baseline, `20260801000020_v007_baseline.sql`.
/// The earlier lanes were folded into it and deleted: the old baseline created
/// an edge sidecar with a foreign key to `proxima_core.edges(edge_id)`, and
/// core's v0.0.7 lane removed that column along with the idea of an edge
/// having an id, so the old lane can no longer run on any database.
/// `ignore_missing` above is what lets a database that already applied those
/// versions tolerate their absence and apply the reset. Re-register and
/// re-index is the way back — the flavor already ships that runbook.
///
/// The file's own header still says "v0.0.8" and names core migration `0015`,
/// which the v0.0.7 release preparation renamed and squashed. That text is
/// deliberately not corrected: `SQLx` checksums a migration's CONTENT and not
/// its filename, so the rename to `_v007_baseline` left every recorded
/// checksum valid while editing one comment byte would invalidate all of
/// them. Read the header as describing the lane it replaced, not the release
/// it ships in.
#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut m = sqlx::migrate!("./migrations");
    // In `public`, not `proxima_code` — see the module docs: destructive
    // baselines drop the flavor schema, and the ledger must survive them.
    m.dangerous_set_table_name("public._sqlx_migrations_proxima_code");
    m.set_ignore_missing(true);
    m
}
