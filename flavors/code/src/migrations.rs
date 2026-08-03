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
    m.set_ignore_missing(true);
    m
}
