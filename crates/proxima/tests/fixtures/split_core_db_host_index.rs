//! [`clone_split_core_db`](crate::split_core_db::clone_split_core_db) for a
//! store whose vectors live in a host index: the database has no `vector`
//! extension.

use std::sync::LazyLock;

use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily};

/// Core lane only, for a store whose vectors live in a host index: its
/// database has no `vector` extension.
static SPLIT_CORE_HOST_INDEX: LazyLock<HostTemplate> = LazyLock::new(|| {
    HostTemplate::host_index(
        TemplateFamily::new("proxima_split_core_hi_").expect("a valid template family"),
        "core lane only, host vector index",
        Vec::new(),
        proxima::PgSidecarRegistryFrozen::default(),
    )
    .expect("the core lane alone is a lineup the runner accepts")
});

/// A split-role database with the core lane migrated, for a boot with `RuntimeBuilder::vector_index`.
///
/// # Errors
///
/// Returns admin connection, template build, or clone errors.
pub async fn clone_split_core_db_host_index(prefix: &str) -> Result<SplitRoleDb, sqlx::Error> {
    SPLIT_CORE_HOST_INDEX.clone_split_role(prefix, &[]).await
}
