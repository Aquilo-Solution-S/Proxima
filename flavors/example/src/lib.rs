//! `proxima-example`: the small flavor the facade tests, dev-migrate and
//! `docs/build-a-flavor.md` build against. It is a test and tutorial
//! fixture (`publish = false`), not a product.
//!
//! Stateful note Fact ([`payload::NoteV1`]), digest Abstraction
//! ([`payload::DigestV1`]), stamp Fact that follows transfer
//! ([`payload::StampV1`]), a notebook scope registry, an ownerless settings
//! table, one read-only tool and one migration lane with its own ledger.

pub mod contract;
mod heads;
pub mod payload;
pub mod tool;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use contract::{EXAMPLE_BASELINE_CONTRACT, EXAMPLE_CONTRACT, FLAVOR_ID, PG_SCHEMA};
pub use heads::note_heads;
pub use payload::{DigestV1, NoteV1, StampV1};
pub use tool::{ExampleStore, ListNotebooksTool};

/// The flavor's embedded migrations.
#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

/// The baseline migration as shipped.
pub const BASELINE_SQL: &str = include_str!("../migrations/20261018000011_baseline.sql");

/// The `stamp_follows` migration (#474 `owner:follows`).
pub const STAMP_FOLLOWS_SQL: &str = include_str!("../migrations/20261019100000_stamp_follows.sql");

const OWNER_RLS_BEGIN: &str = "-- owner-rls: begin\n";
const OWNER_RLS_END: &str = "-- owner-rls: end\n";

/// The baseline split around its owner-RLS section: before, the section
/// (one installer call, then the census), after.
fn owner_rls_split() -> (&'static str, &'static str, &'static str) {
    let (before, rest) = BASELINE_SQL
        .split_once(OWNER_RLS_BEGIN)
        .expect("the baseline marks the start of its owner-RLS section");
    let (section, after) = rest
        .split_once(OWNER_RLS_END)
        .expect("the baseline marks the end of its owner-RLS section");
    (before, section, after)
}

/// The statements of the baseline's owner-RLS section, in order: the one
/// installer call, then the census. The tutorial's variants replace them.
#[must_use]
pub fn installer_calls() -> Vec<&'static str> {
    owner_rls_split()
        .1
        .split(";\n")
        .filter(|statement| !statement.trim().is_empty())
        .collect()
}

/// The baseline with its owner-RLS section replaced by `calls`, in place,
/// for the tutorial's variants.
#[must_use]
pub fn baseline_sql_with_owner_rls(calls: &[&str]) -> String {
    let (before, _, after) = owner_rls_split();
    let mut parts = vec![before];
    for call in calls {
        parts.extend([*call, ";\n"]);
    }
    parts.push(after);
    parts.concat()
}

/// The baseline with no owner-RLS section at all.
#[must_use]
pub fn baseline_sql_without_installer() -> String {
    baseline_sql_with_owner_rls(&[])
}

#[allow(clippy::unnecessary_wraps, reason = "FlavorApp::services signature")]
fn services(
    ctx: &proxima::AppContext,
) -> Result<proxima::flavor::FlavorServices, proxima::flavor::FlavorServiceError> {
    Ok(proxima::flavor::FlavorServices::with(
        ExampleStore::from_host_pool(ctx.host().clone_pool_for_host()),
    ))
}

proxima::flavor_bundle! {
    bundle = ExampleFlavor,
    name = "proxima-example",
    display_name = "Example",
    fact_schemas = [payload::NoteV1, payload::StampV1],
    abstraction_schemas = [payload::DigestV1],
    mcp_tools = [tool::ListNotebooksTool],
    contract = &contract::EXAMPLE_CONTRACT,
    migrations = migrator(),
    app = { title = "Proxima example flavor (test fixture)", services = services },
}
