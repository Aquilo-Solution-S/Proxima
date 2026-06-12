//! Bootstrap a blank Postgres with the substrate schema and every in-repo
//! flavor sidecar, in the same order the desktop shell boots them.
//!
//! `sqlx migrate run` cannot do this: the substrate and flavor migrators
//! share one `_sqlx_migrations` table, so the CLI fails with
//! `VersionMissing` on the second source. This bin delegates to the same
//! framework facade used by embedded hosts: core first, then flavors in
//! composition order, with duplicate migration versions rejected up front.
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

use proxima_embed::{NamedMigrator, run_core_and_flavor_migrations};
use proxima_storage_pg::PgStorage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").map_err(
        |_| "DATABASE_URL must be set, e.g. postgres://proxima:proxima@localhost/proxima",
    )?;
    let pg = PgStorage::connect(&url).await?;
    let report = run_core_and_flavor_migrations(
        &pg,
        [
            NamedMigrator::new("proxima-code", proxima_code::migrator()),
            NamedMigrator::new("proxima-agent-memory", proxima_agent_memory::migrator()),
            NamedMigrator::new("proxima-flavor-goal", proxima_flavor_goal::migrator()),
        ],
    )
    .await?;
    for source in report.sources {
        println!("{source} migrations applied");
    }
    Ok(())
}
