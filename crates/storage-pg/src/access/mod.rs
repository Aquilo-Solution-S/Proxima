pub mod owner_columns;

use async_trait::async_trait;
use proxima_core::{
    AccessError, OwnerAccessPort, OwnerRef, OwnerRoles, Relation, StorageError, UserId,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Stable Postgres-backed [`OwnerAccessPort`] adapter for embedding hosts.
///
/// Wraps `proxima_core.group_memberships` so hosts resolve `(iss, sub) ->
/// OwnerRoles` through this exported adapter instead of hand-rolling the
/// raw membership SQL.
#[derive(Clone)]
pub struct PgOwnerAccessResolver {
    pool: PgPool,
}

impl std::fmt::Debug for PgOwnerAccessResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgOwnerAccessResolver")
            .finish_non_exhaustive()
    }
}

impl PgOwnerAccessResolver {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Build a resolver against a lazily-connected pool: `database_url` is
    /// parsed synchronously (this can fail, e.g. on a malformed URL) but no
    /// network I/O happens until the first query. Host boot order is
    /// typically "construct the authenticator, then connect storage"; a
    /// lazy pool lets this adapter compose into that order without forcing
    /// an eager async connect at authenticator-construction time.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` when `database_url` does not
    /// parse as a Postgres connection string.
    pub fn connect_lazy(database_url: &str) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .connect_lazy(database_url)
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl OwnerAccessPort for PgOwnerAccessResolver {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        let memberships =
            owner_columns::resolve_membership(&self.pool, &OwnerRef::Personal(subject))
                .await
                .map_err(|err| AccessError::Resolution(err.to_string()))?;
        OwnerRoles::for_subject(
            subject,
            memberships
                .into_iter()
                .map(|row| (OwnerRef::Group(row.group), row.relation.role())),
        )
    }
}

impl PgOwnerAccessResolver {
    /// Point-in-time probe: does `subject` currently hold exactly `role` on
    /// `owner`? Distinct from [`OwnerAccessPort::resolve_roles_for_subject`]'s
    /// full enumeration — a single indexed `EXISTS` query for callers that
    /// only need one relation check (e.g. gating a manage-only action).
    ///
    /// Only [`OwnerRef::Group`] owners carry row-backed relations in
    /// `proxima_core.group_memberships`; Personal access is
    /// derived by the kernel rules, never by a membership row, so probing
    /// either always returns `Ok(false)` — fail closed rather than mint a
    /// relation that was never granted.
    ///
    /// # Errors
    ///
    /// Returns `Internal` on sqlx failure.
    pub async fn has_role_for_owner(
        &self,
        subject: UserId,
        owner: OwnerRef,
        role: Relation,
    ) -> Result<bool, StorageError> {
        let OwnerRef::Group(group) = owner else {
            return Ok(false);
        };
        owner_columns::has_group_relation(&self.pool, group, subject, role).await
    }
}
