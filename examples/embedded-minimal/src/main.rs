//! Minimal Proxima embedding: substrate + goal flavor, no shell.
//!
//! The canonical host-binary recipe:
//!   1. connect storage and run substrate + per-flavor migrations,
//!   2. `Engine::compose` with each flavor's `register` fn,
//!   3. `start()` the engine, hold the `EngineHandle`,
//!   4. stop via `stop_tx` and await the dispatch task.
//!
//! Run with the dev database:
//!   `docker compose -f docker-compose.dev.yml up -d`
//!   `DATABASE_URL=... cargo run -p embedded-minimal`

use std::sync::Arc;

use proxima_core::auth::NoAuth;
use proxima_core::{Engine, OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL")?;

    // 1. Storage + migrations (substrate first, then each flavor).
    let pg = PgStorage::connect(&url).await?;
    pg.run_migrations().await?;
    proxima_flavor_goal::migrator().run(pg.pool()).await?;

    // 2. Compose. Single-tenant sentinel owner; real hosts plug an
    //    AuthResolver wired to their identity provider here.
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    };
    let auth = NoAuth::new(owner.principal.clone(), owner);
    let engine = Arc::new(Engine::compose(Box::new(auth), Arc::new(pg), |registry| {
        proxima_flavor_goal::register(registry);
    }));

    // 3. Start. No MCP listener attached, so mcp_url stays None.
    let handle = engine.clone().start().await?;
    println!("engine started; mcp_url={:?}", engine.mcp_url());

    // 4. Clean shutdown.
    handle.stop_tx.send(true)?;
    handle.dispatch_join.await?;
    println!("engine stopped cleanly");
    Ok(())
}
