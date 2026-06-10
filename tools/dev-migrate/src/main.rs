//! Bootstrap a blank Postgres with the substrate schema and every in-repo
//! flavor sidecar, in the same order the desktop shell boots them.
//!
//! `sqlx migrate run` cannot do this: the substrate and flavor migrators
//! share one `_sqlx_migrations` table, so the CLI fails with
//! `VersionMissing` on the second source. Sequential `Migrator::run` calls
//! are version-disjoint and idempotent, which is exactly what this bin does.
//!
//! Usage (two steps — exporting `DATABASE_URL` at compile time would point
//! the workspace's `sqlx::query!` validation at the still-blank target DB):
//!
//! ```text
//! SQLX_OFFLINE=true cargo build -p proxima-dev-migrate
//! DATABASE_URL=postgres://proxima:proxima@localhost/<db> ./target/debug/dev-migrate
//! ```
//!
//! Afterwards `cargo sqlx prepare --workspace` has every schema it needs.

use proxima_storage_pg::PgStorage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").map_err(
        |_| "DATABASE_URL must be set, e.g. postgres://proxima:proxima@localhost/proxima",
    )?;
    let pg = PgStorage::connect(&url).await?;
    pg.run_migrations().await?;
    println!("substrate migrations applied");
    proxima_code::migrator().run(pg.pool()).await?;
    println!("proxima-code flavor migrations applied");
    proxima_mcp_substrate::migrator().run(pg.pool()).await?;
    println!("proxima-mcp-substrate flavor migrations applied");
    proxima_flavor_goal::migrator().run(pg.pool()).await?;
    println!("proxima-flavor-goal flavor migrations applied");
    Ok(())
}
