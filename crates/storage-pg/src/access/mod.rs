pub mod owner_columns;
pub mod scope_surfaces;

use async_trait::async_trait;
use proxima_core::{
    AccessError, GroupId, OwnerAccessPort, OwnerRef, OwnerRoles, Relation, Role, StorageError,
    UserId,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// Stable Postgres-backed [`OwnerAccessPort`] adapter for embedding hosts.
///
/// Wraps `proxima_core.group_memberships` so hosts resolve `(iss, sub) ->
/// OwnerRoles` through this exported adapter instead of hand-rolling the
/// raw membership SQL.
#[derive(Clone)]
pub struct PgOwnerAccessResolver {
    pool: PgPool,
    platform: Option<crate::PgPlatformScope>,
    platform_url: Option<String>,
    platform_schemas: Vec<String>,
    platform_lazy: std::sync::Arc<tokio::sync::OnceCell<crate::PgPlatformScope>>,
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
        Self {
            pool,
            platform: None,
            platform_url: None,
            platform_schemas: Vec::new(),
            platform_lazy: std::sync::Arc::default(),
        }
    }

    #[must_use]
    pub fn with_platform_scope(mut self, platform: crate::PgPlatformScope) -> Self {
        self.platform = Some(platform);
        self
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
        Ok(Self {
            pool,
            platform: None,
            platform_url: None,
            platform_schemas: Vec::new(),
            platform_lazy: std::sync::Arc::default(),
        })
    }

    /// Lazily validate and share a separate platform pool for identity resolution.
    ///
    /// # Errors
    /// Returns a storage error for an invalid runtime connection string.
    /// Platform connection and role errors are returned on the first lookup.
    pub fn connect_lazy_platform(
        runtime_url: &str,
        platform_url: &str,
        schemas: &[&str],
    ) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .connect_lazy(runtime_url)
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        Ok(Self {
            pool,
            platform: None,
            platform_url: Some(platform_url.to_owned()),
            platform_schemas: schemas.iter().map(|s| (*s).to_owned()).collect(),
            platform_lazy: std::sync::Arc::default(),
        })
    }

    async fn platform_scope_lazy(&self) -> Result<Option<crate::PgPlatformScope>, StorageError> {
        if let Some(scope) = &self.platform {
            return Ok(Some(scope.clone()));
        }
        let Some(url) = &self.platform_url else {
            return Ok(None);
        };
        let scope = self
            .platform_lazy
            .get_or_try_init(|| async {
                let pool = PgPoolOptions::new()
                    .connect(url)
                    .await
                    .map_err(|err| StorageError::Unavailable(err.to_string()))?;
                let schemas: Vec<&str> = self.platform_schemas.iter().map(String::as_str).collect();
                crate::PgPlatformScope::new(pool, &schemas).await
            })
            .await?;
        Ok(Some(scope.clone()))
    }

    /// The transaction one probe runs in, rolled back by the caller: platform
    /// scoped when the resolver has a platform scope, else the unscoped
    /// compatible owner transaction.
    async fn begin_probe_transaction(&self) -> Result<Transaction<'static, Postgres>, AccessError> {
        let platform_scope = self
            .platform_scope_lazy()
            .await
            .map_err(|err| resolution_error(&err))?;
        match platform_scope.as_ref() {
            Some(platform) => platform.begin().await,
            None => crate::owner_scope::begin_compatible_owner_transaction(&self.pool, None).await,
        }
        .map_err(|err| resolution_error(&err))
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
/// transaction. A row inserted earlier in the same open transaction is
/// visible. There is no bool probe: ask for the role and use
/// [`Role::manages`] or its ceilings, because a membership bit does not
/// authorize a write (`Causa.Authorization` `may_write`).
///
/// # Errors
///
/// Returns [`AccessError::Resolution`] when the query fails.
pub async fn roles_for_subject(
    conn: &mut PgConnection,
    subject: UserId,
) -> Result<OwnerRoles, AccessError> {
    let memberships = owner_columns::resolve_membership(&mut *conn, &OwnerRef::Personal(subject))
        .await
        .map_err(|err| resolution_error(&err))?;
    OwnerRoles::for_subject(
        subject,
        memberships
            .into_iter()
            .map(|row| (OwnerRef::Group(row.group), row.relation.role())),
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
/// let personal = proxima_core::OwnerRef::Personal(user);
/// let _ = proxima_storage_pg::group_role(conn, user, personal).await;
/// # }
/// ```
///
/// # Errors
///
/// Returns [`AccessError::Resolution`] when the query fails.
pub async fn group_role(
    conn: &mut PgConnection,
    subject: UserId,
    group: GroupId,
) -> Result<Option<Role>, AccessError> {
    let owner = OwnerRef::Group(group);
    let relations = owner_columns::group_relations_for_member_on_connection(conn, group, subject)
        .await
        .map_err(|err| resolution_error(&err))?;
    let roles = OwnerRoles::for_subject(
        subject,
        relations
            .into_iter()
            .map(|relation| (owner, relation.role())),
    )?;
    Ok(roles.role_for(&owner))
}

fn resolution_error(err: &StorageError) -> AccessError {
    AccessError::Resolution(err.to_string())
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
        let platform_scope = self.platform_scope_lazy().await?;
        if let Some(platform) = platform_scope.as_ref() {
            let mut tx = platform.begin().await?;
            let result =
                owner_columns::has_group_relation_on_connection(&mut tx, group, subject, role)
                    .await;
            tx.rollback().await.ok();
            result
        } else {
            let mut tx =
                crate::owner_scope::begin_compatible_owner_transaction(&self.pool, None).await?;
            let result =
                owner_columns::has_group_relation_on_connection(&mut tx, group, subject, role)
                    .await;
            tx.rollback().await.ok();
            result
        }
    }
}
