pub mod owner_columns;
pub mod scope_surfaces;

use async_trait::async_trait;
use proxima_core::{
    AccessError, GroupId, OwnerAccessPort, OwnerId, OwnerRoles, Role, StorageError, UserId,
};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// Stable Postgres-backed [`OwnerAccessPort`] adapter for embedding hosts.
///
/// Wraps the resolved membership projection so hosts resolve `(iss, sub) ->
/// OwnerRoles` through this exported adapter instead of hand-rolling the
/// raw membership SQL.
#[derive(Clone)]
pub struct PgOwnerAccessResolver {
    pool: PgPool,
    platform: crate::PgPlatformScope,
}

impl std::fmt::Debug for PgOwnerAccessResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgOwnerAccessResolver")
            .finish_non_exhaustive()
    }
}

impl PgOwnerAccessResolver {
    #[must_use]
    pub fn new(pool: PgPool, platform: crate::PgPlatformScope) -> Self {
        Self { pool, platform }
    }

    #[must_use]
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// The transaction one probe runs in, rolled back by the caller. Every
    /// resolver carries the boot-validated platform authority.
    async fn begin_probe_transaction(&self) -> Result<Transaction<'static, Postgres>, AccessError> {
        self.platform
            .begin()
            .await
            .map_err(|err| resolution_error(&err))
    }

    /// Whether `group` is registered as a group in the owner registry.
    ///
    /// The forwarder policy's question: a forwarder holds its role only in
    /// a registered group, never in a user's owner.
    ///
    /// # Errors
    ///
    /// [`AccessError::Resolution`] when the probe fails.
    pub async fn is_registered_group(&self, group: GroupId) -> Result<bool, AccessError> {
        let mut tx = self.begin_probe_transaction().await?;
        let result = owner_columns::registered_group_on_connection(&mut tx, group)
            .await
            .map_err(|err| resolution_error(&err));
        tx.rollback().await.ok();
        result
    }
}

#[async_trait]
impl OwnerAccessPort for PgOwnerAccessResolver {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        let mut tx = self.begin_probe_transaction().await?;
        let result = roles_for_subject(&mut tx, subject).await;
        tx.rollback().await.ok();
        result
    }

    /// One probe on the membership primary key instead of the member's whole
    /// enumeration, folded through the same [`OwnerRoles::for_subject`] path
    /// the eager map uses — so the answer is byte-identical to the default
    /// method's and bounded by one group however many the subject belongs
    /// to. That bound is the point for a host whose single forwarder subject
    /// acts for a different party per request.
    async fn resolve_group_role(
        &self,
        subject: UserId,
        group: GroupId,
    ) -> Result<Option<Role>, AccessError> {
        let mut tx = self.begin_probe_transaction().await?;
        let result = group_role(&mut tx, subject, group).await;
        tx.rollback().await.ok();
        result
    }
}

/// Every role `subject` holds, resolved on the caller's connection.
///
/// The connection-taking form of [`OwnerAccessPort::resolve_roles_for_subject`]
/// for a host that already holds a transaction (a platform-scoped one from
/// [`crate::PgPlatformScope::begin`], or an owner-scoped one): it runs the
/// query [`PgOwnerAccessResolver`] runs and folds it through
/// [`OwnerRoles::for_subject`], so several relations in one group give their
/// join and a Personal owner is only ever the subject's own, derived by the
/// kernel rules. Pass a `Transaction` as `&mut *tx`.
///
/// It does not begin, commit or roll back, and sets no scope: it reads what the
/// connection's scope lets it read, which is every group on a platform-scoped
/// transaction. The owner registry is read the same way: an owner scope that
/// does not admit the subject's own id hides the row that refuses a
/// group-registered subject, so resolve on a platform-scoped transaction. A
/// row inserted earlier in the same open transaction is visible. There is no
/// bool probe: ask for the role and use [`Role::manages`] or its ceilings,
/// because a membership bit does not authorize a write
/// (`Causa.Authorization` `may_write`).
///
/// # Errors
///
/// Returns [`AccessError::Resolution`] when the query fails, or
/// [`AccessError::SubjectIsGroup`] when `subject` is registered as a group.
pub async fn roles_for_subject(
    conn: &mut PgConnection,
    subject: UserId,
) -> Result<OwnerRoles, AccessError> {
    resolve_subject_roles(conn, subject, None).await
}

async fn resolve_subject_roles(
    conn: &mut PgConnection,
    subject: UserId,
    group: Option<GroupId>,
) -> Result<OwnerRoles, AccessError> {
    let resolved = owner_columns::resolve_subject_membership(&mut *conn, subject, group)
        .await
        .map_err(|err| resolution_error(&err))?;
    if resolved.subject_is_group {
        return Err(AccessError::SubjectIsGroup(subject));
    }
    OwnerRoles::for_subject(
        subject,
        resolved
            .memberships
            .into_iter()
            .map(|(group, role)| (OwnerId::from(group), role)),
    )
}

/// The role `subject` holds in `group`, resolved on the caller's connection;
/// `None` when the subject holds no relation there.
///
/// The connection-taking form of [`OwnerAccessPort::resolve_group_role`], with
/// the same contract as [`roles_for_subject`]: one probe on the membership
/// primary key, the relations folded through [`OwnerRoles::for_subject`] (their
/// join), no transaction control, and only the rows the connection's scope can
/// see.
///
/// The probe is typed on [`GroupId`]: a Personal owner is derived by the
/// kernel rules, never resolved, so it cannot be asked.
///
/// ```no_run
/// # async fn probe(
/// #     conn: &mut sqlx::PgConnection,
/// #     user: proxima_core::UserId,
/// #     group: proxima_core::GroupId,
/// # ) {
/// let _ = proxima_storage_pg::group_role(conn, user, group).await;
/// # }
/// ```
///
/// ```compile_fail
/// # async fn probe(
/// #     conn: &mut sqlx::PgConnection,
/// #     user: proxima_core::UserId,
/// # ) {
/// let personal = OwnerId::from(user);
/// let _ = proxima_storage_pg::group_role(conn, user, personal).await;
/// # }
/// ```
///
/// # Errors
///
/// Returns [`AccessError::Resolution`] when the query fails, or
/// [`AccessError::SubjectIsGroup`] when `subject` is registered as a group.
pub async fn group_role(
    conn: &mut PgConnection,
    subject: UserId,
    group: GroupId,
) -> Result<Option<Role>, AccessError> {
    let owner = OwnerId::from(group);
    let roles = resolve_subject_roles(conn, subject, Some(group)).await?;
    Ok(roles.role_for(&owner))
}

fn resolution_error(err: &StorageError) -> AccessError {
    AccessError::Resolution(err.to_string())
}
