//! Split-role test databases whose core lane is already migrated.
//!
//! `create_db` + `split_role_urls` + boot runs the whole core lane in every
//! test. [`clone_split_core_db`] clones a [`HostTemplate`] that holds exactly
//! what that produces — split roles provisioned, then the core lane applied as
//! the platform role through boot's own migration runner — so boot finds every
//! core migration applied and runs only the flavor lanes the test composes.
//! Tests that exercise a fresh migration keep `create_db`.
//!
//! The clone is a [`SplitRoleDb`]: it carries both URLs and drops the database
//! with the test (and keeps it, with a redacted `psql` URL, on a panic), so a
//! caller never drops it by hand.

use std::sync::LazyLock;

use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily};

/// Core lane only: no flavor and no seed, so nothing beyond the core
/// migrators decides the template name.
static SPLIT_CORE: LazyLock<HostTemplate> = LazyLock::new(|| {
    HostTemplate::new(
        TemplateFamily::new("proxima_split_core_").expect("a valid template family"),
        "core lane only",
        Vec::new(),
    )
    .expect("the core lane alone is a lineup the runner accepts")
});

/// The drop-in for `create_db(name)` before `split_role_urls(name)` and a
/// boot: a database named `<prefix>_<id>` with the split roles provisioned and
/// the core lane migrated.
///
/// # Errors
///
/// Returns admin connection, template build, or clone errors.
pub async fn clone_split_core_db(prefix: &str) -> Result<SplitRoleDb, sqlx::Error> {
    SPLIT_CORE.clone_split_role(prefix, &[]).await
}
