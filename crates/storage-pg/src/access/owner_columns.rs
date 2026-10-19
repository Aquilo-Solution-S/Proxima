//! Direct `OwnerId` column helpers.

use proxima_core::flavor::TransferLeg;
use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::scope::ScopeKind;
use proxima_core::storage_ports::{MembershipEndpointAuthority, MembershipMemberAuthority};
use proxima_core::{
    AccessKind, EntityId, GroupId, MembershipRow, OwnerId, Relation, Role, StorageError, UserId,
};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "proxima_core.access_ceiling", rename_all = "lowercase")]
pub(crate) enum PgAccessCeiling {
    None,
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl From<PgAccessCeiling> for proxima_core::AccessCeiling {
    fn from(value: PgAccessCeiling) -> Self {
        match value {
            PgAccessCeiling::None => Self::None,
            PgAccessCeiling::Fact => Self::Fact,
            PgAccessCeiling::Abstraction => Self::Abstraction,
            PgAccessCeiling::Perspective => Self::Perspective,
            PgAccessCeiling::Goal => Self::Goal,
        }
    }
}

/// One cross-process boundary shared by every host-capable `UoW` and exclusive
/// owner/source erasure. ASCII `proxhlcy` is a stable advisory-lock key.
pub(crate) const HOST_STATE_LIFECYCLE_FENCE_KEY: i64 = i64::from_be_bytes(*b"proxhlcy");

pub(crate) async fn lock_host_lifecycle_fence_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(HOST_STATE_LIFECYCLE_FENCE_KEY)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

pub(crate) async fn lock_host_lifecycle_fence_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(HOST_STATE_LIFECYCLE_FENCE_KEY)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

pub(crate) async fn lock_host_lifecycle_fence_shared_session(
    conn: &mut sqlx::postgres::PgConnection,
) -> Result<(), StorageError> {
    sqlx::query("SELECT pg_advisory_lock_shared($1)")
        .bind(HOST_STATE_LIFECYCLE_FENCE_KEY)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    Ok(())
}

pub(crate) async fn lock_owner_fence_exclusive_session(
    conn: &mut sqlx::postgres::PgConnection,
    owner: &OwnerId,
) -> Result<(), StorageError> {
    lock_owner_fences_session(conn, std::slice::from_ref(owner), true).await
}

async fn lock_owner_fences_session(
    conn: &mut sqlx::postgres::PgConnection,
    owners: &[OwnerId],
    exclusive: bool,
) -> Result<(), StorageError> {
    if owners.is_empty() {
        return Ok(());
    }
    let ids = owner_fence_ids(owners);
    let query = if exclusive {
        sqlx::query(
            "SELECT pg_advisory_lock(hashtextextended('proxima-owner-fence:' || i::text, 0)) FROM unnest($1::uuid[]) AS f(i)",
        )
    } else {
        sqlx::query(
            "SELECT pg_advisory_lock_shared(hashtextextended('proxima-owner-fence:' || i::text, 0)) FROM unnest($1::uuid[]) AS f(i)",
        )
    };
    query
        .bind(&ids)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    Ok(())
}

use crate::error::{map_err, with_bounded_retry};
use crate::verbs::fact_embeddings::{
    RouteSpaces, enqueue_series_head_in_tx, purge_series_embedding_spaces_in_tx,
};

/// Sorted, deduplicated owner UUIDs: the canonical order every multi-owner
/// fence acquisition takes.
fn owner_fence_ids(owners: &[OwnerId]) -> Vec<uuid::Uuid> {
    let mut ids: Vec<uuid::Uuid> = owners.iter().map(|owner| owner.into_inner()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The owner admission fence is a distinct advisory-lock namespace from
/// lifecycle keys and Memory series handles. Its key is the owner id alone
/// (`proxima-owner-fence:<uuid>`): one id is one owner. Memory and Goal admission hold
/// this lock in shared mode. Source-scoped erase also holds it shared while
/// taking its source fence exclusively; whole-owner erase takes the
/// single-owner fence exclusively. Multi-owner transfers use the separate
/// canonical-order exclusive helper below.
///
/// # Errors
///
/// Returns [`StorageError`] when Postgres cannot acquire the transaction
/// advisory lock.
pub async fn lock_owner_fence_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
) -> Result<(), StorageError> {
    lock_owner_fences(tx, std::slice::from_ref(owner), false).await
}

/// Acquire shared owner fences in canonical owner order for a multi-owner
/// admission. Batched Goal writes use this before touching their handles and
/// lifecycle targets; sorting prevents crossed batches from forming an
/// owner-fence cycle. Transfers do not use shared fences: their two endpoints
/// need the exclusive helper below so an owner-scoped erase cannot interleave
/// with the move.
pub(crate) async fn lock_owner_fences_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
    owners: &[OwnerId],
) -> Result<(), StorageError> {
    lock_owner_fences(tx, owners, false).await
}

/// Acquire an exclusive owner fence for the whole-owner erase boundary.
pub(crate) async fn lock_owner_fence_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
) -> Result<(), StorageError> {
    lock_owner_fences(tx, std::slice::from_ref(owner), true).await
}

/// Acquire exclusive owner fences in canonical order for an ownership move.
///
/// Transfer can add a sourced series to either endpoint's source scope while
/// it removes the same series from the other. Taking both owner boundaries
/// exclusively makes either source-scoped or whole-owner erase linearize
/// wholly before or after that move, without discovering a new source lock
/// after series locks. The same canonical order prevents crossed transfers
/// from forming an owner-fence cycle.
pub(crate) async fn lock_owner_fences_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
    owners: &[OwnerId],
) -> Result<(), StorageError> {
    lock_owner_fences(tx, owners, true).await
}

const LOCK_OWNER_FENCES_SHARED_SQL: &str = "\
SELECT pg_advisory_xact_lock_shared(
           hashtextextended('proxima-owner-fence:' || i::text, 0)
       )
  FROM unnest($1::uuid[]) AS f(i)";

const LOCK_OWNER_FENCES_EXCLUSIVE_SQL: &str = "\
SELECT pg_advisory_xact_lock(
           hashtextextended('proxima-owner-fence:' || i::text, 0)
       )
  FROM unnest($1::uuid[]) AS f(i)";

/// The one owner-fence acquisition. The four entry points above differ only in
/// arity and lock mode; the key, the ordering rule and the bind chain are
/// shared, so they live here rather than in four copies. SQL cannot
/// parameterize a function name, so the mode picks between two constants that
/// are otherwise identical — the one differing token reads side by side above.
///
/// Ordering comes from the Rust sort plus `unnest`, not from an `ORDER BY` —
/// the same reasoning written on `forget::lock_advisory_ids_tx`: a bare
/// `Function Scan` yields elements in array order, and both lock functions are
/// `VOLATILE` + `PARALLEL RESTRICTED`, so the projection is evaluated once per
/// element, in order.
async fn lock_owner_fences(
    tx: &mut Transaction<'_, Postgres>,
    owners: &[OwnerId],
    exclusive: bool,
) -> Result<(), StorageError> {
    if owners.is_empty() {
        return Ok(());
    }
    let ids = owner_fence_ids(owners);
    let sql = if exclusive {
        LOCK_OWNER_FENCES_EXCLUSIVE_SQL
    } else {
        LOCK_OWNER_FENCES_SHARED_SQL
    };
    // SQL-POLICY: fixed-fragment
    sqlx::query(sql)
        .bind(&ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// Source admission fence.  The owner portion is deliberately repeated in
/// the key, so a source label shared by two owners never serializes them.
pub(crate) async fn lock_source_fence_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    source: &str,
) -> Result<(), StorageError> {
    lock_source_fence(tx, owner, source, false).await
}

/// Exclusive source-scope erase fence.  The caller must already hold the
/// owner's shared fence, preserving owner -> source ordering.
pub(crate) async fn lock_source_fence_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    source: &str,
) -> Result<(), StorageError> {
    lock_source_fence(tx, owner, source, true).await
}

const LOCK_SOURCE_FENCE_SHARED_SQL: &str = "\
SELECT pg_advisory_xact_lock_shared(
           hashtextextended('proxima-source-fence:' || $1::text || ':' || $2, 0)
       )";

const LOCK_SOURCE_FENCE_EXCLUSIVE_SQL: &str = "\
SELECT pg_advisory_xact_lock(
           hashtextextended('proxima-source-fence:' || $1::text || ':' || $2, 0)
       )";

/// One key, one bind chain; only the lock function differs by mode.
async fn lock_source_fence(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    source: &str,
    exclusive: bool,
) -> Result<(), StorageError> {
    let sql = if exclusive {
        LOCK_SOURCE_FENCE_EXCLUSIVE_SQL
    } else {
        LOCK_SOURCE_FENCE_SHARED_SQL
    };
    // SQL-POLICY: fixed-fragment
    sqlx::query(sql)
        .bind(owner.into_inner())
        .bind(source)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// The flavor-owned lifecycle-scope fence — one substrate namespace for
/// every declared scope, whatever the flavor calls it.
///
/// A scope is a lifecycle the substrate could not previously see. Its rows
/// carry a bare `<scope>_id uuid` with no foreign key into the flavor's
/// registry, so the row lock an erase takes on the registry row constrains
/// nothing else, and two writes into two different scopes of one owner share
/// the owner fence and the one source fence with each other — neither
/// separates a scope's erase from a write into that same scope. This key
/// does.
///
/// Key: `proxima-scope-fence:<scope_kind>:<owner_id>:<scope_id>`.
/// The scope KIND leads because two flavors' scopes must not collide on a
/// shared uuid, and the owner portion is repeated for the reason the source
/// fence repeats it: a scope id that somehow appeared under two owners must
/// never serialize them. Scope ids are NEVER hashed into the Memory
/// `t`/handle namespace — a scope is not an admission, and the alias would
/// make a scope id collide with a series handle.
///
/// Position in the global order: AFTER the owner and source fences, BEFORE
/// the Memory handle and lifecycle `t` locks and every row the write
/// touches. Writers take it shared, an erase takes it exclusive: the thing
/// a scope fence excludes is the erase, not the other writers into the same
/// scope.
const LOCK_SCOPE_FENCE_SHARED_SQL: &str = "\
SELECT pg_advisory_xact_lock_shared(
           hashtextextended(
               'proxima-scope-fence:' || $1 || ':' || $2::text || ':' || $3::text,
               0
           )
       )";

const LOCK_SCOPE_FENCE_EXCLUSIVE_SQL: &str = "\
SELECT pg_advisory_xact_lock(
           hashtextextended(
               'proxima-scope-fence:' || $1 || ':' || $2::text || ':' || $3::text,
               0
           )
       )";

/// Take one declared scope's fence SHARED, in an already-open transaction.
///
/// The admission side. Every transaction that persists a payload declaring
/// this scope takes it, before its handle/`t` locks, and re-asks under it
/// whether the scope's registry row is still there. Several writers into one
/// scope hold it together; the scope's eraser does not join them.
///
/// # Errors
///
/// [`StorageError`] when Postgres cannot acquire the advisory lock.
pub async fn lock_scope_fence_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: ScopeKind,
    owner: &OwnerId,
    scope_id: uuid::Uuid,
) -> Result<(), StorageError> {
    lock_scope_fence(tx, kind, owner, scope_id, false).await
}

/// Take one declared scope's fence EXCLUSIVELY, in an already-open
/// transaction.
///
/// The erase side, and the reason the fence exists. A flavor's scope erase
/// takes it BEFORE it reads anything it intends to delete — fence before
/// select — so the footprint it computes is exact by construction rather
/// than by re-checking afterwards: a later writer waits here, and an earlier
/// one has already committed and is in the footprint.
///
/// # Errors
///
/// [`StorageError`] when Postgres cannot acquire the advisory lock.
pub async fn lock_scope_fence_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: ScopeKind,
    owner: &OwnerId,
    scope_id: uuid::Uuid,
) -> Result<(), StorageError> {
    lock_scope_fence(tx, kind, owner, scope_id, true).await
}

/// One key, one bind chain; only the lock function differs by mode — the
/// same shape the owner and source fences above use, and for the same
/// reason: SQL cannot parameterize a function name, so the two constants
/// read side by side and a drift between them is visible.
async fn lock_scope_fence(
    tx: &mut Transaction<'_, Postgres>,
    kind: ScopeKind,
    owner: &OwnerId,
    scope_id: uuid::Uuid,
    exclusive: bool,
) -> Result<(), StorageError> {
    let sql = if exclusive {
        LOCK_SCOPE_FENCE_EXCLUSIVE_SQL
    } else {
        LOCK_SCOPE_FENCE_SHARED_SQL
    };
    // SQL-POLICY: fixed-fragment
    sqlx::query(sql)
        .bind(kind.as_str())
        .bind(owner.into_inner())
        .bind(scope_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// Object-key fence: its own advisory-lock namespace, keyed by the object key
/// itself.
///
/// The invariant it buys: **every writer that creates, moves or destroys a
/// `blob_uploads` row's reference to an object key holds this lock, and every
/// reader that decides whether the object may be destroyed holds it too.**
/// That is what makes the retain anti-join ("no surviving `blob_uploads` row
/// names this key") true AT DECISION TIME rather than true of a READ
/// COMMITTED snapshot. Advisory xact locks are held to commit, so the second
/// of two concurrent erasers of a shared mounted object waits, then observes
/// the first's committed deletes — and the object is enqueued exactly once
/// instead of withheld by both.
///
/// Position in the global order: AFTER the owner and source fences, AFTER the
/// Memory handle and lifecycle `t` locks, and AFTER any row lock the caller
/// already holds — and BEFORE the retain/purge decision and the
/// `cold_purge_pending` insert that follows it. Erase is the one caller that
/// takes it before its row locks, because its rows are the ones the anti-join
/// is about; the pair can therefore still deadlock against a caller that
/// locked its row first, which is a `40P01` and exactly what the bounded
/// retry around both transactions is for.
///
/// Only `blob_uploads` keys are many-to-one, so only they are in this
/// namespace.
///
/// Takers: owner and source-scope erase (every key they might owe), upload
/// abort, the stage locator write, blob GC, and the two transfer legs that
/// re-home an upload row over an object — the in-place move and the mount.
///
/// # Errors
///
/// Returns [`StorageError`] when Postgres cannot acquire the transaction
/// advisory lock.
pub async fn lock_object_keys_tx(
    tx: &mut Transaction<'_, Postgres>,
    object_keys: &[String],
) -> Result<(), StorageError> {
    if object_keys.is_empty() {
        return Ok(());
    }
    // Sorted and deduplicated for the same reason the owner fences are:
    // crossed callers holding two keys each must agree on one order, and one
    // round trip must not take the same lock twice.
    let mut keys: Vec<&str> = object_keys.iter().map(String::as_str).collect();
    keys.sort_unstable();
    keys.dedup();
    // SQL-POLICY: fixed-fragment
    sqlx::query(LOCK_OBJECT_KEYS_SQL)
        .bind(&keys)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

const LOCK_OBJECT_KEYS_SQL: &str = "\
SELECT pg_advisory_xact_lock(
           hashtextextended('proxima-object-key:' || k, 0)
       )
  FROM unnest($1::text[]) AS f(k)";

/// Every object key the upload rows of `blob_ids` name, sorted for
/// [`lock_object_keys_tx`]. Both transfer legs that re-home an upload row over
/// an object take this set before they touch a row.
pub(crate) async fn blob_upload_object_keys(
    tx: &mut Transaction<'_, Postgres>,
    blob_ids: &[uuid::Uuid],
) -> Result<Vec<String>, StorageError> {
    if blob_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(
        "SELECT DISTINCT object_key
           FROM proxima_core.blob_uploads
          WHERE blob_id = ANY($1::uuid[])
          ORDER BY object_key",
    )
    .bind(blob_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

/// Register `owner` in `proxima_core.owners` if it is not registered yet.
///
/// The only production owners upsert on the data path. Memory / goal /
/// citation / cited-blob writes all go through this. A group is registered
/// when it is created (`register_group`), so an id a write reaches unregistered
/// is a user seen for the first time and is registered as `personal`. A
/// registered id keeps the kind it was registered with: the registry is never
/// rewritten from a write.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Storage errors from the upsert.
pub async fn ensure_owner_row(
    conn: &mut sqlx::PgConnection,
    owner: &OwnerId,
) -> Result<uuid::Uuid, StorageError> {
    let owner_id = owner.into_inner();
    // `owners` is append-only (no UPDATE): a concurrent first insert waits
    // on whichever owner uniqueness constraint it reaches, then does nothing.
    // No conflict target is intentional: v0.0.30 also enforces
    // `(owner_id, kind)`, and either arbiter must settle the same first sighting.
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal')
         ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(owner_id)
}

/// Register `group` as a group, eagerly, when it is created.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// [`StorageError::Conflict`] when the id is already registered as a user.
/// Other storage errors from the upsert.
pub async fn register_group(conn: &mut PgConnection, group: GroupId) -> Result<(), StorageError> {
    lock_group_membership_tx(conn, group).await?;
    insert_group_row(conn, group).await?;
    if registered_group_on_connection(conn, group).await? {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "owner id is registered as a user, not a group".into(),
        ))
    }
}

async fn insert_group_row(conn: &mut PgConnection, group: GroupId) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'group')
         ON CONFLICT DO NOTHING",
    )
    .bind(group.into_inner())
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Whether `owner` is registered, regardless of its owner kind.
///
/// # Errors
///
/// Storage errors from the probe.
pub(crate) async fn registered_owner_on_connection(
    conn: &mut PgConnection,
    owner: OwnerId,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.owners WHERE owner_id = $1
         )",
    )
    .bind(owner.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)
}

/// Whether `group` is registered as a group in `proxima_core.owners`.
///
/// # Errors
///
/// Storage errors from the probe.
pub(crate) async fn registered_group_on_connection(
    conn: &mut sqlx::PgConnection,
    group: GroupId,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.owners
              WHERE owner_id = $1 AND kind = 'group'
         )",
    )
    .bind(group.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_membership(
    conn: &mut PgConnection,
    owner: &OwnerId,
) -> Result<Vec<MembershipRow>, StorageError> {
    let resolved = resolve_subject_membership(conn, owner.as_user(), None).await?;
    if resolved.subject_is_group {
        return Err(StorageError::ConstraintViolation(
            "membership lookup subject is registered as a group".into(),
        ));
    }
    Ok(resolved
        .memberships
        .into_iter()
        .map(|(group, role)| MembershipRow { group, role })
        .collect())
}

/// The subject kind and the memberships visible to one resolution probe.
/// The kind is read in the same query as the resolved projection so a
/// group-shaped subject is refused without a second round trip.
pub(crate) struct SubjectMemberships {
    pub(crate) subject_is_group: bool,
    pub(crate) memberships: Vec<(GroupId, Role)>,
}

type SubjectMembershipRow = (
    bool,
    Option<uuid::Uuid>,
    Option<PgAccessCeiling>,
    Option<PgAccessCeiling>,
    Option<bool>,
);

/// Resolve a subject's registered kind and projected roles in one query.
/// `group` narrows the projection rows for the point probe used by
/// `group_role`; `None` enumerates every group for `roles_for_subject`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_subject_membership(
    conn: &mut sqlx::PgConnection,
    subject: UserId,
    group: Option<GroupId>,
) -> Result<SubjectMemberships, StorageError> {
    let query = if let Some(group) = group {
        sqlx::query_as::<_, SubjectMembershipRow>(
            "WITH subject AS (
             SELECT EXISTS (
                 SELECT 1 FROM proxima_core.owners
                  WHERE owner_id = $1 AND kind = 'group'
             ) AS subject_is_group
         )
         SELECT subject.subject_is_group, memberships.group_id,
                memberships.read_ceiling, memberships.write_ceiling, memberships.manage
           FROM subject
           LEFT JOIN proxima_core.group_role_projection AS memberships
             ON memberships.member_user_id = $1
            AND memberships.group_id = $2",
        )
        .bind(subject.into_inner())
        .bind(group.into_inner())
    } else {
        sqlx::query_as::<_, SubjectMembershipRow>(
            "WITH subject AS (
             SELECT EXISTS (
                 SELECT 1 FROM proxima_core.owners
                  WHERE owner_id = $1 AND kind = 'group'
             ) AS subject_is_group
         )
         SELECT subject.subject_is_group, memberships.group_id,
                memberships.read_ceiling, memberships.write_ceiling, memberships.manage
           FROM subject
           LEFT JOIN proxima_core.group_role_projection AS memberships
             ON memberships.member_user_id = $1
          ORDER BY memberships.group_id, memberships.read_ceiling,
                   memberships.write_ceiling, memberships.manage",
        )
        .bind(subject.into_inner())
    };
    let rows = query.fetch_all(&mut *conn).await.map_err(map_err)?;

    let subject_is_group = rows.first().is_some_and(|(is_group, _, _, _, _)| *is_group);
    let mut memberships = Vec::with_capacity(rows.len());
    for (_, group, read, write, manage) in rows {
        let Some(group) = group else { continue };
        let (Some(read), Some(write), Some(manage)) = (read, write, manage) else {
            continue;
        };
        let role = Role::new(read.into(), write.into(), manage).map_err(|error| {
            StorageError::ConstraintViolation(format!(
                "group role projection contains an invalid role: {error}"
            ))
        })?;
        memberships.push((GroupId::new(group), role));
    }
    Ok(SubjectMemberships {
        subject_is_group,
        memberships,
    })
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn lock_group_membership_tx(
    conn: &mut PgConnection,
    group_id: GroupId,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.group_lock_request
            (request_id, left_group_id)
         VALUES ($1, $2)",
    )
    .bind(Uuid::now_v7())
    .bind(group_id.into_inner())
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Acquire the tree lock before reading a member's owner kind. The trigger
/// gateway computes the roots, orders two trees, and rechecks roots after the
/// advisory locks; the row is consumed by its BEFORE trigger.
pub(crate) async fn lock_group_membership_write_tx(
    conn: &mut PgConnection,
    parent: GroupId,
    member: OwnerId,
) -> Result<bool, StorageError> {
    let row: (bool,) = sqlx::query_as(
        "INSERT INTO proxima_core.group_lock_request
            (request_id, left_group_id, right_group_id)
         VALUES ($1, $2, $3)
         RETURNING member_is_group",
    )
    .bind(Uuid::now_v7())
    .bind(parent.into_inner())
    .bind(member.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(row.0)
}

/// # Errors
///
/// Returns `Conflict` when any Admin row already exists for the group, and
/// `Internal` on sqlx failure.
pub(crate) async fn bootstrap_group_admin(
    _pool: &PgPool,
    platform: &crate::PgPlatformScope,
    group_id: GroupId,
    first_admin_user_id: UserId,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = platform.begin().await?;
        let result =
            bootstrap_group_admin_on_connection(&mut tx, group_id, first_admin_user_id).await;
        crate::owner_scope::finish_transaction(tx, result).await
    })
    .await
}

/// Register a group and seed its first Admin; refuse an existing Admin.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Returns [`StorageError::Conflict`] if an Admin exists or the group id is
/// registered as a user (the membership foreign key); other database failures
/// are classified as [`StorageError`].
pub async fn bootstrap_group_admin_on_connection(
    conn: &mut PgConnection,
    group_id: GroupId,
    first_admin_user_id: UserId,
) -> Result<(), StorageError> {
    lock_group_membership_tx(conn, group_id).await?;
    insert_group_row(conn, group_id).await?;
    let inserted = sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         SELECT $1, $2, $3
          WHERE NOT EXISTS (
              SELECT 1
                FROM proxima_core.group_memberships
               WHERE group_id = $1
                 AND relation = $3
          )",
    )
    .bind(group_id.into_inner())
    .bind(first_admin_user_id.into_inner())
    .bind(Relation::Admin)
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;

    if inserted.rows_affected() == 0 {
        return Err(StorageError::Conflict(
            "group already has an Admin membership".into(),
        ));
    }

    Ok(())
}

/// Adding a member to an id never seen before creates the group, so the
/// id is registered as a group first.
///
/// # Errors
///
/// [`StorageError::Conflict`] when the id is registered as a user: a
/// membership names a group, never a user. `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    owner_scope: Option<&proxima_core::OwnerScope>,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
    member_authority: MembershipMemberAuthority,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = add_group_member_on_connection_authorized(
            &mut tx,
            group_id,
            member,
            relation,
            member_authority,
        )
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    })
    .await
}

/// Register an unseen group and add a relation; an existing relation stays.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Returns [`StorageError::Conflict`] if the group id is registered as a user
/// (the membership foreign key); other database failures are classified as
/// [`StorageError`].
pub async fn add_group_member_on_connection(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
) -> Result<(), StorageError> {
    add_group_member_on_connection_authorized(
        conn,
        group_id,
        member,
        relation,
        MembershipMemberAuthority::CallerManagesMember,
    )
    .await
}

async fn add_group_member_on_connection_authorized(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
    member_authority: MembershipMemberAuthority,
) -> Result<(), StorageError> {
    let member_is_group = lock_group_membership_write_tx(conn, group_id, member).await?;
    insert_group_row(conn, group_id).await?;
    if member_is_group {
        if !matches!(
            member_authority,
            MembershipMemberAuthority::CallerManagesMember
        ) {
            return Err(StorageError::MembershipForbidden(
                "attaching a group requires manage authority on the attached group".into(),
            ));
        }
        let existing: Option<(Uuid, Relation)> = sqlx::query_as(
            "SELECT parent_group_id, relation
               FROM proxima_core.group_edges
              WHERE child_group_id = $1",
        )
        .bind(member.into_inner())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_err)?;
        if let Some((existing_parent, existing_relation)) = existing {
            if existing_parent == group_id.into_inner() && existing_relation == relation {
                return Ok(());
            }
            return Err(StorageError::Conflict(
                "group is already attached to a parent".into(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO proxima_core.group_edges (child_group_id, parent_group_id, relation)
             VALUES ($2, $1, $3)
            ON CONFLICT (child_group_id) DO NOTHING",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .bind(relation)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
        if inserted.rows_affected() == 0 {
            return Err(StorageError::Conflict(
                "group is already attached to a parent".into(),
            ));
        }
    } else {
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             VALUES ($1, $2, $3)
             ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .bind(relation)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

/// Under the group-tree lock: the caller the engine authorized must still
/// manage `group`. A role revoked between the engine check and this lock
/// refuses the write. A write without a named caller is the host's own.
async fn recheck_caller_manages(
    conn: &mut PgConnection,
    group_id: GroupId,
    caller: Option<UserId>,
) -> Result<(), StorageError> {
    let Some(caller) = caller else {
        return Ok(());
    };
    let manages: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.group_role_projection
              WHERE group_id = $1 AND member_user_id = $2 AND manage)",
    )
    .bind(group_id.into_inner())
    .bind(caller.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)?;
    if !manages {
        return Err(StorageError::MembershipForbidden(
            "the caller no longer manages this group".into(),
        ));
    }
    Ok(())
}

/// Whether `group` has a stored manager. Groups whose roles the host resolves
/// on demand have none; storage then has no role to re-read, and the change
/// rests on the engine's check alone. Once a group has a stored manager, a
/// change a caller makes under the group-tree lock must not leave it with
/// none. The host's own writes name no caller and may revoke the last one.
async fn group_has_manager(
    conn: &mut PgConnection,
    group_id: GroupId,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.group_role_projection
              WHERE group_id = $1 AND manage)",
    )
    .bind(group_id.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_group_member(
    pool: &PgPool,
    owner_scope: Option<&proxima_core::OwnerScope>,
    group_id: GroupId,
    member: OwnerId,
    member_authority: MembershipMemberAuthority,
    endpoint_authority: MembershipEndpointAuthority,
    caller: Option<UserId>,
) -> Result<(), StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            #[cfg(test)]
            crate::pause_point::reach(crate::pause_point::BEFORE_GROUP_LOCK, group_id.into_inner())
                .await;
            lock_group_membership_write_tx(&mut tx, group_id, member).await?;
            let had_manager = group_has_manager(&mut tx, group_id).await?;
            if had_manager {
                recheck_caller_manages(&mut tx, group_id, caller).await?;
            }
            remove_group_member_on_connection_authorized(
                &mut tx,
                group_id,
                member,
                member_authority,
                endpoint_authority,
            )
            .await?;
            if had_manager && !group_has_manager(&mut tx, group_id).await? {
                return Err(StorageError::Conflict(
                    "the change would leave the group without a manager".into(),
                ));
            }
            Ok(())
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    })
    .await
}

/// Remove every relation of a member in a group; an absent member is `Ok`.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Returns [`StorageError`] for database failures.
pub async fn remove_group_member_on_connection(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
) -> Result<(), StorageError> {
    remove_group_member_on_connection_authorized(
        conn,
        group_id,
        member,
        MembershipMemberAuthority::CallerManagesMember,
        MembershipEndpointAuthority::Parent,
    )
    .await
}

async fn remove_group_member_on_connection_authorized(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    member_authority: MembershipMemberAuthority,
    endpoint_authority: MembershipEndpointAuthority,
) -> Result<(), StorageError> {
    let member_is_group = lock_group_membership_write_tx(conn, group_id, member).await?;
    if member_is_group
        && matches!(endpoint_authority, MembershipEndpointAuthority::Member)
        && !matches!(
            member_authority,
            MembershipMemberAuthority::CallerManagesMember
        )
    {
        return Err(StorageError::MembershipForbidden(
            "detaching a group requires manage authority on the attached group".into(),
        ));
    }
    if !member_is_group && matches!(endpoint_authority, MembershipEndpointAuthority::Member) {
        return Err(StorageError::MembershipForbidden(
            "detaching a person requires parent authority".into(),
        ));
    }
    if member_is_group {
        sqlx::query(
            "DELETE FROM proxima_core.group_edges
              WHERE parent_group_id = $1 AND child_group_id = $2",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
        return Ok(());
    }
    sqlx::query(
        "DELETE FROM proxima_core.group_memberships WHERE group_id = $1 AND member_user_id = $2",
    )
    .bind(group_id.into_inner())
    .bind(member.into_inner())
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Delete the one `relation` row of `member_user_id` in `group_id`, under the
/// group's membership lock. An absent row is `Ok`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn remove_group_member_relation(
    pool: &PgPool,
    owner_scope: Option<&proxima_core::OwnerScope>,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
    member_authority: MembershipMemberAuthority,
    endpoint_authority: MembershipEndpointAuthority,
    caller: Option<UserId>,
) -> Result<(), StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            #[cfg(test)]
            crate::pause_point::reach(crate::pause_point::BEFORE_GROUP_LOCK, group_id.into_inner())
                .await;
            lock_group_membership_write_tx(&mut tx, group_id, member).await?;
            let had_manager = group_has_manager(&mut tx, group_id).await?;
            if had_manager {
                recheck_caller_manages(&mut tx, group_id, caller).await?;
            }
            remove_group_member_relation_on_connection_authorized(
                &mut tx,
                group_id,
                member,
                relation,
                member_authority,
                endpoint_authority,
            )
            .await?;
            if had_manager && !group_has_manager(&mut tx, group_id).await? {
                return Err(StorageError::Conflict(
                    "the change would leave the group without a manager".into(),
                ));
            }
            Ok(())
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    })
    .await
}

/// Remove one relation of a member in a group; an absent relation is `Ok`.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Returns [`StorageError`] for database failures.
pub async fn remove_group_member_relation_on_connection(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
) -> Result<(), StorageError> {
    remove_group_member_relation_on_connection_authorized(
        conn,
        group_id,
        member,
        relation,
        MembershipMemberAuthority::CallerManagesMember,
        MembershipEndpointAuthority::Parent,
    )
    .await
}

async fn remove_group_member_relation_on_connection_authorized(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    relation: Relation,
    member_authority: MembershipMemberAuthority,
    endpoint_authority: MembershipEndpointAuthority,
) -> Result<(), StorageError> {
    let member_is_group = lock_group_membership_write_tx(conn, group_id, member).await?;
    if member_is_group
        && matches!(endpoint_authority, MembershipEndpointAuthority::Member)
        && !matches!(
            member_authority,
            MembershipMemberAuthority::CallerManagesMember
        )
    {
        return Err(StorageError::MembershipForbidden(
            "detaching a group requires manage authority on the attached group".into(),
        ));
    }
    if !member_is_group && matches!(endpoint_authority, MembershipEndpointAuthority::Member) {
        return Err(StorageError::MembershipForbidden(
            "detaching a person requires parent authority".into(),
        ));
    }
    if member_is_group {
        sqlx::query(
            "DELETE FROM proxima_core.group_edges
              WHERE parent_group_id = $1 AND child_group_id = $2 AND relation = $3",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .bind(relation)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    } else {
        sqlx::query(
            "DELETE FROM proxima_core.group_memberships
              WHERE group_id = $1 AND member_user_id = $2 AND relation = $3",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .bind(relation)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

/// Replace the `from` row of `member_user_id` in `group_id` with a `to` row in
/// one transaction under the group's membership lock: delete `from`, insert
/// `to` (an existing `to` row stays), commit. The lock serializes every
/// membership change of the group, so a second replacement of the same row
/// waits, then finds `from` gone.
///
/// # Errors
///
/// Returns `Conflict`, with nothing changed, when the member does not hold
/// `from` (including a row the owner scope cannot see), and `Internal` on sqlx
/// failure.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn replace_group_member_relation(
    pool: &PgPool,
    owner_scope: Option<&proxima_core::OwnerScope>,
    group_id: GroupId,
    member: OwnerId,
    from: Relation,
    to: Relation,
    _granted_by: uuid::Uuid,
    caller: Option<UserId>,
) -> Result<(), StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            #[cfg(test)]
            crate::pause_point::reach(crate::pause_point::BEFORE_GROUP_LOCK, group_id.into_inner())
                .await;
            lock_group_membership_write_tx(&mut tx, group_id, member).await?;
            let had_manager = group_has_manager(&mut tx, group_id).await?;
            if had_manager {
                recheck_caller_manages(&mut tx, group_id, caller).await?;
            }
            replace_group_member_relation_on_connection(&mut tx, group_id, member, from, to)
                .await?;
            if had_manager && !group_has_manager(&mut tx, group_id).await? {
                return Err(StorageError::Conflict(
                    "the change would leave the group without a manager".into(),
                ));
            }
            Ok(())
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    })
    .await
}

/// Replace a held relation with another; an existing target relation stays.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for this write as for its own rows.
/// The caller must supply a platform-scoped transaction (`&mut *tx`). This
/// function never begins, commits, rolls back, retries or sets a scope; on a
/// retryable error, the caller retries its whole transaction. All effects are
/// database rows in that transaction; no post-commit call is needed.
///
/// # Errors
///
/// Returns [`StorageError::Conflict`] if the member does not hold `from`, with
/// nothing changed. Other database failures are classified as [`StorageError`];
/// the caller must roll back the transaction on failure.
pub async fn replace_group_member_relation_on_connection(
    conn: &mut PgConnection,
    group_id: GroupId,
    member: OwnerId,
    from: Relation,
    to: Relation,
) -> Result<(), StorageError> {
    let member_is_group = lock_group_membership_write_tx(conn, group_id, member).await?;
    if member_is_group {
        let updated = sqlx::query(
            "UPDATE proxima_core.group_edges
                SET relation = $4
              WHERE parent_group_id = $1 AND child_group_id = $2 AND relation = $3",
        )
        .bind(group_id.into_inner())
        .bind(member.into_inner())
        .bind(from)
        .bind(to)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
        if updated.rows_affected() == 0 {
            return Err(StorageError::Conflict(
                "member does not hold the relation being replaced".into(),
            ));
        }
        return Ok(());
    }
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND member_user_id = $2
            AND relation = $3",
    )
    .bind(group_id.into_inner())
    .bind(member.into_inner())
    .bind(from)
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    if deleted.rows_affected() == 0 {
        return Err(StorageError::Conflict(
            "member does not hold the relation being replaced".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         VALUES ($1, $2, $3)
         ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
    )
    .bind(group_id.into_inner())
    .bind(member.into_inner())
    .bind(to)
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members(
    conn: &mut PgConnection,
    group_id: GroupId,
) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
    // People remain the membership rows as written; child groups are the
    // forest edge rows. Both use the same stable owner-id/relation order.
    let rows: Vec<(uuid::Uuid, bool, Relation)> = sqlx::query_as(
        "SELECT member_user_id, false, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
         UNION ALL
         SELECT child_group_id, true, relation
           FROM proxima_core.group_edges
          WHERE parent_group_id = $1
          ORDER BY 1, 2, 3",
    )
    .bind(group_id.into_inner())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, _is_group, relation)| (OwnerId::new(member), relation))
        .collect())
}

/// One page of group members in the keyset total order
/// `(member_user_id, relation)`, starting strictly after `after` when
/// given. Fetches at most `limit` rows; callers over-fetch by one to
/// detect further pages. Relation ties order by the
/// `proxima_core.membership_relation` enum definition.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members_page(
    conn: &mut PgConnection,
    group_id: GroupId,
    after: Option<(OwnerId, Relation)>,
    limit: i64,
) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
    let after_member = after.map(|(member, _)| member.into_inner());
    let after_relation = after.map(|(_, relation)| relation);
    let rows: Vec<(uuid::Uuid, bool, Relation)> = sqlx::query_as(
        "SELECT member_id, is_group, relation
           FROM (
             SELECT member_user_id AS member_id, false AS is_group, relation
               FROM proxima_core.group_memberships
              WHERE group_id = $1
             UNION ALL
             SELECT child_group_id, true, relation
               FROM proxima_core.group_edges
              WHERE parent_group_id = $1
           ) members
          WHERE ($2::uuid IS NULL
                 OR (member_id, relation)
                    > ($2::uuid, $3::proxima_core.membership_relation))
          ORDER BY member_id, is_group, relation
          LIMIT $4",
    )
    .bind(group_id.into_inner())
    .bind(after_member)
    .bind(after_relation)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, _is_group, relation)| (OwnerId::new(member), relation))
        .collect())
}

/// Home owner and kind when the row is in `read_owners`. Absent and foreign
/// are both `None`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn visible_home_owner<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity: EntityId,
    read_owners: &[OwnerId],
) -> Result<Option<(OwnerId, AccessKind)>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }

    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerId::into_inner)
        .collect();
    // The entity discriminant is authorization data. Memory visibility is the
    // memory spine, forgotten rows included, while Goal visibility stays on
    // its own spine. The candidate-owner uniqueness guard makes a malformed
    // duplicate `(entity, owner)` state fail closed instead of authorizing the
    // first row returned by PostgreSQL.
    let row: Option<(uuid::Uuid, String)> = match entity {
        EntityId::Memory(memory_id) => {
            sqlx::query_as(
                "WITH candidates AS (
                     SELECT owner_id, kind FROM proxima_core.memory WHERE t = $1 /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
                 )
                 SELECT c.owner_id, c.kind::text
                   FROM candidates c
                  WHERE c.owner_id = ANY($2::uuid[])
                    AND NOT EXISTS (
                        SELECT 1
                          FROM candidates other
                         WHERE other.owner_id <> c.owner_id
                    )",
            )
            .bind(memory_id.into_inner())
            .bind(&owner_ids)
            .fetch_optional(executor)
            .await
        }
        EntityId::Goal(goal_id) => {
            sqlx::query_as(
                "SELECT g.owner_id, 'goal'
               FROM proxima_core.goal g
              WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])",
            )
            .bind(goal_id.into_inner())
            .bind(&owner_ids)
            .fetch_optional(executor)
            .await
        }
    }
    .map_err(map_err)?;

    row.map(|(id, kind)| Ok((OwnerId::new(id), access_kind(&kind)?)))
        .transpose()
}

fn access_kind(stored: &str) -> Result<AccessKind, StorageError> {
    match stored {
        "fact" => Ok(AccessKind::Fact),
        "abstraction" => Ok(AccessKind::Abstraction),
        "perspective" => Ok(AccessKind::Perspective),
        "goal" => Ok(AccessKind::Goal),
        other => Err(StorageError::Internal(format!(
            "unknown entity kind {other}"
        ))),
    }
}

/// Transfer one memory **series** to `to_owner`.
///
/// Same `(handle, t)`: a transfer is an owner UPDATE, not a copy. Head and
/// every version on the handle move together (`MemoryHeadAligned`), including
/// forgotten versions, which keep their row and flag. A request may name a
/// forgotten `t`. The series' existing source-owned head participates in the
/// compare-and-set.
/// Returns `true` iff a row under `from_owner` matched and was updated.
///
/// Sidecars follow the memory unless their surface declares `RetainAtSource`.
/// Such rows retain their own owner and are excluded from generated transfer
/// statements and the forget/hydrate cycle.
///
/// Cited `blob` rows move when no other live series under a different owner
/// still cites them. Embeddings / jobs follow the transferred `t`s so ANN
/// (`emb.owner_id`) stays Tesla-valve. `ingest_keys` for those `t`s are
/// deleted so the prior owner can mint a new series.
///
/// Goals do not transfer — the engine refuses them before storage; this
/// backstop keeps a direct storage call from transferring one.
///
/// # Errors
///
/// `Conflict` when a cited blob is still referenced by another live series or
/// the source owner still has an upload whose publication is unresolved.
/// `ConstraintViolation` for a Goal entity, and for unique / check
/// violations.
pub(crate) async fn transfer_to_owner(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    permit: &proxima_core::storage_ports::OwnerWritePermit,
    surfaces: &OwnerSurfaces,
    entity: EntityId,
    to_owner: OwnerId,
    embeddings: RouteSpaces<'_>,
) -> Result<bool, StorageError> {
    let from_owner = *permit.owner();
    // The backstop, DRIVEN by the declaration rather than merely agreeing
    // with it: the entity picks the surface that speaks for its series, and
    // that surface's resolved leg decides. Re-declare `proxima_core.goal` as
    // a rule that moves rows and goals start transferring, which is what
    // "enforced by the declaration" has to mean if it is to mean anything.
    let memory_id = match entity {
        EntityId::Memory(memory_id) => memory_id,
        EntityId::Goal(_) => {
            return Err(refuse_untransferable_entity(surfaces, GOAL_SPINE));
        }
    };
    if let TransferLeg::Refused { .. } = surfaces.transfer_leg(MEMORY_SPINE) {
        return Err(refuse_untransferable_entity(surfaces, MEMORY_SPINE));
    }
    if from_owner == to_owner {
        return Err(StorageError::ConstraintViolation(
            "transfer destination is the current owner".into(),
        ));
    }
    let from_id = from_owner.into_inner();
    with_bounded_retry(move || async move {
        let mut tx =
            crate::platform_scope::begin_owner_lifecycle_transaction(pool, platform, permit)
                .await?;
        // Transfers participate in the owner fence before probing, locking,
        // or re-homing any series row.  Both endpoints are acquired in the
        // same sorted order, so an owner erase cannot pass the transfer's
        // boundary half-way through and crossed transfers cannot deadlock.
        lock_owner_fences_exclusive_tx(&mut tx, &[from_owner, to_owner]).await?;
        let transferred = transfer_memory_t(
            &mut tx,
            surfaces,
            memory_id.into_inner(),
            from_id,
            to_owner,
            embeddings,
        )
        .await?;
        if !transferred {
            // A false persist can follow real writes: when the head is gone
            // or changed owner after the series reads, blob/content
            // rows are already re-homed to the destination in this
            // transaction. Roll back so they revert to
            // the prior owner.
            tx.rollback().await.map_err(map_err)?;
            return Ok(false);
        }
        tx.commit().await.map_err(map_err)?;
        Ok(transferred)
    })
    .await
}

/// The kernel spine tables the two [`EntityId`] arms name. A transfer asks
/// the contract what it may do to an entity by asking about the surface
/// that IS the entity's series.
const MEMORY_SPINE: &str = "proxima_core.memory";
const GOAL_SPINE: &str = "proxima_core.goal";

/// The refusal a `NotTransferable` surface earns, with the reason its own
/// declaration gave.
///
/// A surface the registry does not carry resolves to
/// [`TransferLeg::Unreachable`], which freeze refuses at boot — so reaching
/// the fallback here means the registry was assembled by hand, and the
/// honest answer is still a refusal.
fn refuse_untransferable_entity(surfaces: &OwnerSurfaces, table: &str) -> StorageError {
    let entity = match table {
        GOAL_SPINE => "goals",
        _ => "these entities",
    };
    match surfaces.transfer_leg(table) {
        // The declared `why` is written headline-first — the sentence a
        // caller needs, a colon, then the rationale an operator needs. The
        // wire refusal carries the headline, so the message is GENERATED
        // from `NotTransferable { why }` rather than restated beside it:
        // rewording the declaration moves this error, which is what the
        // transfer differential golden is for.
        TransferLeg::Refused { why } => {
            let headline = why.split_once(':').map_or(why, |(head, _)| head);
            StorageError::ConstraintViolation(format!("{entity} do not transfer: {headline}"))
        }
        _ => StorageError::Internal(format!(
            "{table} declares no transfer leg this substrate can perform"
        )),
    }
}

async fn transfer_memory_t(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    t: uuid::Uuid,
    from_id: uuid::Uuid,
    to_owner: OwnerId,
    embeddings: RouteSpaces<'_>,
) -> Result<bool, StorageError> {
    let handle: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT handle
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(handle) = handle else {
        return Ok(false);
    };
    transfer_memory_handle(tx, surfaces, t, handle, from_id, to_owner, embeddings).await
}

const SERIES_TS_SQL: &str = "SELECT t FROM proxima_core.memory WHERE handle = $1 AND owner_id = $2 /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
     ORDER BY t";

/// Acquire the transfer's complete lifecycle boundary in the global order:
/// owner fences (the caller), every series handle, then every series `t`.
///
/// The handle lock is what closes the only growth edge. Append, forget,
/// hydrate, and erase all take it before changing membership, so once it is
/// held the sorted `t` set cannot grow behind the transfer. The membership
/// reread after the `t` locks remains mandatory: it turns an unexpected
/// writer that crossed this boundary into `Retryable` before any transfer
/// leg can mutate a row. The public transfer port retries that whole
/// transaction with [`with_bounded_retry`].
///
/// An empty result means the series left `from_id` while this transaction was
/// waiting for its handle; that is the ordinary false result, not a partial
/// transfer.
async fn lock_series_membership(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    // A transfer is one series and therefore one handle, but keep the
    // complete-set helper at the seam: callers that ever resolve a compound
    // transfer must still hand the lock helper a sorted set, never grow it
    // while taking locks.
    crate::verbs::forget::lock_memory_handles_tx(tx, &[handle]).await?;

    let expected: Vec<uuid::Uuid> = sqlx::query_scalar(SERIES_TS_SQL)
        .bind(handle)
        .bind(from_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)?;
    if expected.is_empty() {
        return Ok(expected);
    }

    // The handle lock prevents append/forget/hydrate/erase from changing the
    // series while these lifecycle locks are acquired. The helper sorts and
    // deduplicates, so crossed transfers share one order for every `t`.
    crate::verbs::forget::lock_forget_memories_tx(tx, &expected).await?;
    let actual: Vec<uuid::Uuid> = sqlx::query_scalar(SERIES_TS_SQL)
        .bind(handle)
        .bind(from_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)?;
    if actual != expected {
        return Err(StorageError::Retryable(
            "series membership changed before transfer lifecycle locks were complete".into(),
        ));
    }
    Ok(actual)
}

/// The database's closed upload lifecycle, shared by staging and publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.blob_upload_status",
    rename_all = "lowercase"
)]
pub enum UploadStatus {
    Pending,
    Completed,
    Aborted,
    Expired,
}

/// The result of publishing a staged upload against its cited object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishTransitionDecision {
    WonPending,
    AlreadyFinished,
    OvertookTerminal,
}

/// Classify a locked upload publication result.
///
/// # Errors
/// Refuses contradictory object identities or affected-row counts.
pub fn finish_transition_decision(
    observed_status: UploadStatus,
    observed_cited_object_id: Option<Uuid>,
    cited_object_id: Uuid,
    rows_affected: u64,
) -> Result<FinishTransitionDecision, StorageError> {
    match rows_affected {
        1 => Ok(FinishTransitionDecision::WonPending),
        0 => match observed_status {
            UploadStatus::Completed if observed_cited_object_id == Some(cited_object_id) => {
                Ok(FinishTransitionDecision::AlreadyFinished)
            }
            UploadStatus::Completed => Err(StorageError::ConstraintViolation(
                "upload already completed against a different cited object".into(),
            )),
            UploadStatus::Aborted | UploadStatus::Expired => {
                Ok(FinishTransitionDecision::OvertookTerminal)
            }
            UploadStatus::Pending => Err(StorageError::ConstraintViolation(
                "upload finish did not transition pending row".into(),
            )),
        },
        other => Err(StorageError::ConstraintViolation(format!(
            "upload finish affected {other} rows"
        ))),
    }
}

/// Publish an upload on the caller's transaction; return its cleanup bucket.
///
/// Shared by cited Fact admission and direct blob-port completion. Takes the
/// owner fence, canonical-key fence and row lock in that order, verifies the
/// staged hash against the owner-scoped cited object, and settles the row.
/// Completed replays perform no write. No transaction control or S3 I/O.
///
/// # Errors
/// Refuses absent uploads, unstaged or mismatched bytes, and contradictory
/// completed objects. Database faults retain their storage error class.
pub async fn complete_upload_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    upload_id: Uuid,
    cited_object_id: Uuid,
) -> Result<String, StorageError> {
    let key = proxima_core::citations::uploaded_blob_object_key(upload_id);
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, owner).await?;
    crate::access::owner_columns::lock_object_keys_tx(tx, std::slice::from_ref(&key)).await?;
    let (bucket, status, blob_id, hash): (String, UploadStatus, Option<Uuid>, Option<Vec<u8>>) =
        sqlx::query_as(
            "SELECT bucket, status, blob_id, content_hash
               FROM proxima_core.blob_uploads
              WHERE owner_id = $1 AND upload_id = $2
              FOR UPDATE",
        )
        .bind(owner.into_inner())
        .bind(upload_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?
        .ok_or_else(|| StorageError::ConstraintViolation("upload not found for Owner".into()))?;

    match status {
        UploadStatus::Completed => {
            finish_transition_decision(status, blob_id, cited_object_id, 0)?;
            return Ok(bucket);
        }
        UploadStatus::Aborted | UploadStatus::Expired => {
            finish_transition_decision(status, blob_id, cited_object_id, 0)?;
        }
        UploadStatus::Pending => {}
    }
    let hash = hash
        .as_deref()
        .filter(|hash| hash.len() == 32)
        .ok_or_else(|| {
            StorageError::ConstraintViolation(
                "upload has no exact staged content identity; restage before finish".into(),
            )
        })?;
    sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT content_hash FROM proxima_core.blob
          WHERE blob_id = $1 AND owner_id = $2
            AND schema_id = $3 AND content_hash = $4
          FOR SHARE",
    )
    .bind(cited_object_id)
    .bind(owner.into_inner())
    .bind(proxima_core::UPLOADED_BLOB_SCHEMA_ID)
    .bind(hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?
    .ok_or_else(|| {
        StorageError::ConstraintViolation(
            "cited object does not match the staged upload content identity".into(),
        )
    })?;

    let changed = sqlx::query(
        "UPDATE proxima_core.blob_uploads
            SET object_key = $1, status = 'completed', blob_id = $2, completed_at = now()
          WHERE owner_id = $3 AND upload_id = $4 AND status = $5",
    )
    .bind(key)
    .bind(cited_object_id)
    .bind(owner.into_inner())
    .bind(upload_id)
    .bind(status)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    finish_transition_decision(status, blob_id, cited_object_id, changed)?;
    if changed != 1 {
        return Err(StorageError::ConstraintViolation(
            "late upload finish did not transition terminal row".into(),
        ));
    }
    Ok(bucket)
}

/// Refuse a transfer while upload publication can still attach one of this
/// series' cited blobs to its source-owned locator.
async fn ensure_series_uploads_published(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
) -> Result<(), StorageError> {
    // Staged rows can still await publication by a direct blob-port caller
    // or an older host. Engine completion publishes its upload row in the
    // Fact transaction. Refuse a transfer while another staged row could
    // later attach this blob under the source owner.
    // The ordinary fence is identity- and series-scoped: an abandoned prepare
    // has no content identity, and an unrelated staged upload cannot claim
    // this series. The one exception is a row staged before 0007: it has a
    // canonical locator and SHA-256 witness but no BLAKE3 column to relate it
    // to a cited blob. That ambiguous upgrade state must fence the owner until
    // a completion retry re-hashes it; otherwise transfer can move the Fact's
    // blob before completion attaches the source upload row.
    let unpublished_upload: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.blob_uploads u
               JOIN proxima_core.blob b
                 ON b.owner_id = u.owner_id
                AND b.schema_id = 'core/uploaded-blob-v1'
                AND b.content_hash = u.content_hash
              WHERE u.owner_id = $1
                AND u.content_hash IS NOT NULL
                AND b.blob_id IN (
                    SELECT m.blob_id
                      FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
                     WHERE m.handle = $2
                       AND m.owner_id = $1
                       AND m.blob_id IS NOT NULL
                )
                AND (
                    u.status = 'pending'
                    OR (u.status = 'completed' AND u.blob_id IS NULL)
                    OR (
                        u.status IN ('aborted', 'expired')
                        AND NOT EXISTS (
                            SELECT 1
                              FROM proxima_core.blob_uploads p
                             WHERE p.owner_id = $1
                               AND p.blob_id = b.blob_id
                               AND p.status = 'completed'
                               AND p.completed_at IS NOT NULL
                               AND p.content_hash = b.content_hash
                               AND octet_length(p.sha256) = 32
                               AND p.expected_byte_len >= 0
                               AND btrim(p.bucket) <> ''
                               AND btrim(p.filename) <> ''
                               AND btrim(p.mime) <> ''
                               AND p.object_key = 'objects/' || COALESCE(
                                   p.mounted_from_upload_id, p.upload_id
                               )::text
                        )
                    )
                )
         ) OR EXISTS (
             SELECT 1
               FROM proxima_core.blob_uploads u
              WHERE u.owner_id = $1
                AND u.content_hash IS NULL
                AND u.status = 'pending'
                AND u.completed_at IS NULL
                AND octet_length(u.sha256) = 32
                AND u.expected_byte_len >= 0
                AND btrim(u.bucket) <> ''
                AND btrim(u.filename) <> ''
                AND btrim(u.mime) <> ''
                AND u.object_key = 'objects/' || COALESCE(
                    u.mounted_from_upload_id, u.upload_id
                )::text
         )",
    )
    .bind(from_id)
    .bind(handle)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if unpublished_upload {
        return Err(StorageError::Conflict(
            "owner has an upload whose publication is not complete".into(),
        ));
    }
    Ok(())
}

async fn transfer_memory_handle(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    requested_t: uuid::Uuid,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_owner: OwnerId,
    embeddings: RouteSpaces<'_>,
) -> Result<bool, StorageError> {
    // The destination's `owners` row is not seeded anywhere, so mint it (or
    // confirm its kind) BEFORE any statement below binds `to_id` into
    // an `owner_id` FK — `blob`, `content`, `memory_head`,
    // `memory`, `sketch` and embeddings all reference
    // `proxima_core.owners`.
    let to_id = ensure_owner_row(tx.as_mut(), &to_owner).await?;
    let ts = lock_series_membership(tx, handle, from_id).await?;
    if ts.is_empty() {
        return Ok(false);
    }
    if !ts.contains(&requested_t) {
        // The probe that resolved the handle may have preceded a complete
        // erase. A handle is reusable, so never transfer a replacement
        // series merely because it now occupies the captured handle.
        return Ok(false);
    }
    // A head is mandatory whenever a row exists, forgotten or not (forget
    // keeps the row and its head, #488). Accepting a missing head here would
    // turn a live-head race into a transfer that moves sidecars without
    // moving the spine.
    let head: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT t, owner_id FROM proxima_core.memory_head
          WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let expected_head_t = if let Some((head_t, head_owner)) = head {
        if head_owner != from_id {
            // Preserve the existing live-head denial: a stale owner must not
            // move the series' other rows once its head has left.
            return Ok(false);
        }
        if !ts.contains(&head_t) {
            return Err(StorageError::Retryable(
                "series head advanced after transfer locked its version set".into(),
            ));
        }
        Some(head_t)
    } else {
        // `ts` is non-empty, so the series has rows and must have a head.
        return Err(StorageError::Retryable(
            "memory head disappeared while the series still had rows".into(),
        ));
    };
    ensure_series_uploads_published(tx, handle, from_id).await?;
    // Order, and all of it load-bearing.
    //
    // The two dedupe surfaces run FIRST because both read the series' rows,
    // live and forgotten, under the SOURCE owner to find what the series
    // cites, and the generated re-home is what stops those reads from
    // matching.
    //
    // The generated legs run SECOND, and BEFORE the head compare-and-set. A
    // transfer that took the head row first would hold that lock across
    // every remaining statement, and `memory_head` is the row a same-series
    // ingest has to read: the transfer would be serializing ordinary writes
    // behind its own row work. The transfer race regressions pin exactly
    // that, and pin it as a liveness property ("without adding a lock to
    // normal ingest") rather than as an outcome.
    //
    // Doing real writes before the decision is already this transaction's
    // shape: a false persist rolls the whole thing back, which is why
    // `transfer_to_owner` rolls back rather than committing a `false`.
    transfer_cited_blobs(tx, surfaces, handle, from_id, to_id).await?;
    transfer_content_for_handle(tx, surfaces, handle, from_id, to_id).await?;
    run_generated_transfer_legs(tx, surfaces, from_id, to_id, &ts).await?;
    if let Some(expected_head_t) = expected_head_t
        && !persist_series_head_transfer(tx, handle, from_id, to_id, expected_head_t).await?
    {
        return Ok(false);
    }
    // The series' publication position follows it, so the destination's
    // next capture continues above every position already handed out.
    sqlx::query(
        "UPDATE proxima_core.publication_counter
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    // The vectors moved with the series, but they are the destination's
    // only in spaces its route names: the rest go, and the head is queued
    // for each route space it has no vector in.
    purge_series_embedding_spaces_in_tx(tx, to_id, embeddings.spaces, &ts).await?;
    enqueue_series_head_in_tx(tx, to_id, handle, embeddings).await?;
    Ok(true)
}

/// The head compare-and-set that DECIDES whether the transfer happened.
///
/// This is why `memory_head` is a declared bespoke transfer leg and not a
/// generated one: `rows_affected` carries the question. Zero means either
/// the head advanced under us — a retryable race — or the series left the
/// owner entirely, which is the clean `false`. No generated
/// `UPDATE ... WHERE key = ANY($1)` can carry that question, and the two
/// races that hang off the answer are the ones `owner_transfer.rs` pins.
async fn persist_series_head_transfer(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    expected_head_t: uuid::Uuid,
) -> Result<bool, StorageError> {
    let head = sqlx::query(
        "UPDATE proxima_core.memory_head
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2 AND t = $4",
    )
    .bind(handle)
    .bind(from_id)
    .bind(to_id)
    .bind(expected_head_t)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if head.rows_affected() == 0 {
        let owner_still_matches: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.memory_head
                  WHERE handle = $1 AND owner_id = $2
             )",
        )
        .bind(handle)
        .bind(from_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
        if owner_still_matches {
            return Err(StorageError::Retryable(
                "series head advanced before transfer could persist".into(),
            ));
        }
        return Ok(false);
    }
    Ok(true)
}

/// ONE loop over the declarations, in table order.
///
/// Every statement here is a `TransferLeg` the flavor that declared the
/// surface resolved at registry time:
///
/// - [`TransferLeg::Rehomed`] sets the surface's declared owner column,
///   selecting on the column the key names against the series' `t` set and
///   the source owner. That covers `memory`, `sketch`, the three
///   embedding tables and EVERY flavor's projection with one statement shape.
/// - [`TransferLeg::Dropped`] deletes them — one member, `ingest_keys`,
///   whose receipt proves admission by the SOURCE and does not travel.
///
/// A flavor adding a `Follow` surface cannot have it silently not follow:
/// the surface is in this loop because it is in the contract, and a surface
/// this loop cannot reach is `FlavorRegistryError::UnmovableSurface` at boot
/// rather than a row the source owner can still read after the memory became
/// someone else's.
///
/// `RetainAtSource` resolves to `TransferLeg::Retained`, which is excluded
/// from `generated_transfer_legs()`.
async fn run_generated_transfer_legs(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    ts: &[uuid::Uuid],
) -> Result<(), StorageError> {
    for (table, leg) in surfaces.generated_transfer_legs() {
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(series_leg_sql(table, leg)?))
            .bind(ts)
            .bind(to_id)
            .bind(from_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// The statement one generated transfer leg runs, over the series' `t` set.
///
/// Two shapes, both `$1` = the `t` array, `$2` = the destination owner, and
/// `$3` = the source owner.
/// `Dropped` binds `$2` and `$3` and reads neither as data, which costs two
/// sentinel predicates and buys one call site instead of a second loop whose
/// only difference is a verb.
///
/// Every identifier substituted here is a `&'static str` from a `const`
/// contract that `try_freeze` has already validated, and every one is
/// additionally checked against `information_schema` by
/// `flavor_contract_acceptance::every_column_a_declaration_names_is_a_column_the_catalog_has`.
/// `PgIdent` is the belt to that suspenders: it is what makes the
/// substitution `%I`-equivalent rather than merely well-intentioned.
fn series_leg_sql(table: &str, leg: TransferLeg) -> Result<String, StorageError> {
    let table = crate::pg_ident::PgIdent::table(table)?;
    match leg {
        TransferLeg::Rehomed {
            key_column,
            owner_column,
        } => {
            let owner = crate::pg_ident::PgIdent::column(owner_column)?;
            let key = crate::pg_ident::PgIdent::column(key_column)?;
            // SQL-POLICY: PgIdent
            Ok(format!(
                "UPDATE {} SET {} = $2 WHERE {} = ANY($1::uuid[]) AND {} = $3",
                table.as_str(),
                owner.as_str(),
                key.as_str(),
                owner.as_str()
            ))
        }
        TransferLeg::Dropped { key_column, .. } => {
            let key = crate::pg_ident::PgIdent::column(key_column)?;
            // SQL-POLICY: PgIdent
            Ok(format!(
                concat!(
                    "DELETE FROM {} WHERE {} = ANY($1::uuid[]) ",
                    "AND $2::uuid IS NOT NULL AND $3::uuid IS NOT NULL"
                ),
                table.as_str(),
                key.as_str()
            ))
        }
        other => Err(StorageError::Internal(format!(
            "{} resolved to {other:?}, which is not a generated transfer leg",
            table.as_str()
        ))),
    }
}

/// Move the handle's cited blobs to the destination, deduping rather than
/// refusing when the bytes are shared.
///
/// `TransferRule::FollowOrDedupe` on `proxima_core.blob`, three cases:
///
/// 1. **The destination already holds these bytes.** Its own row wins;
///    this handle's references are repointed at it and the source keeps
///    whatever it still uses. Moving the row instead would collide with the
///    destination's UNIQUE `(owner_id, schema_id, content_hash)`.
/// 2. **Nothing else references the bytes.** Move the rows in place: same
///    `blob_id`, same `upload_id`, same object, no mount. The common case
///    does not pay for the uncommon one, and a citation id a client already
///    holds does not move under it.
/// 3. **Another owner's live series references the bytes.** The destination
///    gets a row of its own and an upload row that MOUNTS the source's
///    object — OCI's cross-repo blob mount, where a mount is an optimisation
///    over a copy and never a correctness requirement. Nothing in S3 is
///    read, written or copied; ownership is metadata over an immutable
///    store.
///
/// The source's rows are never deleted here. Deleting a blob row decides
/// the fate of an S3 object, and a transfer has no object-store handle in
/// scope; erase does, and its purge is refcounted precisely so a shared
/// object survives one owner leaving. An unreferenced source row is the
/// source owner's own row, reachable by its own erase and reported by
/// reconcile.
async fn transfer_cited_blobs(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let leg = dedupe_leg(surfaces, BLOB_SURFACE)?;
    let blob_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT blob_id
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL
          ORDER BY blob_id",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    // Both re-homing legs below put a `blob_uploads` row over an object key
    // that another owner's erase may be deciding about right now: the in-place
    // move hands the row to the destination, and the mount inserts a second
    // row naming the SAME object. Take the whole sorted key set once, here,
    // before any of it — a per-leg lock would grow the set item by item, which
    // is precisely the shape the lock-order rule forbids.
    let object_keys = blob_upload_object_keys(tx, &blob_ids).await?;
    lock_object_keys_tx(tx, &object_keys).await?;
    for blob_id in blob_ids {
        transfer_one_cited_blob(tx, leg, handle, from_id, to_id, blob_id).await?;
    }
    Ok(())
}

/// The tables whose transfer is `FollowOrDedupe`, named where the
/// orchestration lives rather than inside it.
const BLOB_SURFACE: &str = "proxima_core.blob";
const CONTENT_SURFACE: &str = "proxima_core.content";

/// The dedupe arm's payload, carried as itself.
///
/// [`TransferLeg`] spans every arm a surface can resolve to; the statements
/// below serve exactly one of them. Narrowing once, at the resolve, is what
/// lets everything downstream take the two fields rather than a leg it would
/// have to re-test — the refusal happens where the answer is produced, not
/// at each place it is read.
#[derive(Debug, Clone, Copy)]
struct DedupeLeg {
    dedupe_key: &'static [&'static str],
    remaps: &'static [&'static str],
}

/// The registry-resolved dedupe leg for one surface, or a typed refusal.
///
/// `freeze` has already proved this resolves — a `FollowOrDedupe` surface
/// that is not in the flavor's bespoke list is `UnmovableSurface` at boot —
/// so the error arm is reachable only from a hand-assembled registry, and
/// refusing is the honest answer there.
fn dedupe_leg(surfaces: &OwnerSurfaces, table: &str) -> Result<DedupeLeg, StorageError> {
    match surfaces.transfer_leg(table) {
        TransferLeg::Deduped { dedupe_key, remaps } => Ok(DedupeLeg { dedupe_key, remaps }),
        other => Err(StorageError::Internal(format!(
            "{table} resolved to {other:?}; this statement serves the dedupe arm"
        ))),
    }
}

/// The `SELECT` that answers "does the destination already hold these
/// bytes", generated from the surface's declared `dedupe_key`.
///
/// The key is not decoration: it is the UNIQUE constraint the move would
/// collide with, which is exactly why the arm exists. Declaring it and then
/// hardcoding the same three columns in SQL is how the two drift, and
/// `every_dedupe_key_is_a_uniqueness_the_schema_enforces` asks
/// `pg_constraint` whether each declared key really is one.
///
/// The bind order is the declared column order, and the caller binds
/// exactly `dedupe_key.len()` values — a key of a different length is a
/// contract change that fails here rather than silently matching the wrong
/// row.
fn dedupe_lookup_sql(
    table: &str,
    key_column: &str,
    dedupe_key: &[&'static str],
) -> Result<String, StorageError> {
    let table = crate::pg_ident::PgIdent::table(table)?;
    let key = crate::pg_ident::PgIdent::column(key_column)?;
    let mut predicates = Vec::with_capacity(dedupe_key.len());
    for (n, column) in dedupe_key.iter().enumerate() {
        predicates.push(format!(
            "{} = ${}",
            crate::pg_ident::PgIdent::column(column)?.as_str(),
            n + 1
        ));
    }
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT {} FROM {} WHERE {}",
        key.as_str(),
        table.as_str(),
        predicates.join(" AND ")
    ))
}

/// One referring column repointed from the old row to the new one,
/// generated from the surface's declared `remaps`.
///
/// Scoped by `handle` and not by owner: the hot rows have not changed hands
/// at this point in the transfer; they change hands in the generated re-home
/// later in the same transaction. Scoping by owner here
/// would remap nothing or everything depending on the order.
fn remap_sql(entry: &str) -> Result<String, StorageError> {
    let (table, column) = entry
        .split_once('.')
        .ok_or_else(|| StorageError::Internal(format!("remap {entry} is not <table>.<column>")))?;
    let qualified = format!("proxima_core.{table}");
    let table = crate::pg_ident::PgIdent::table(&qualified)?;
    let column = crate::pg_ident::PgIdent::column(column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "UPDATE {} SET {} = $3 WHERE handle = $1 AND {} = $2",
        table.as_str(),
        column.as_str(),
        column.as_str()
    ))
}

/// One cited blob, one of the three cases.
async fn transfer_one_cited_blob(
    tx: &mut Transaction<'_, Postgres>,
    leg: DedupeLeg,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    blob_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let DedupeLeg { dedupe_key, remaps } = leg;
    let Some((schema_id, content_hash)) = sqlx::query_as::<_, (String, Vec<u8>)>(
        "SELECT schema_id, content_hash
           FROM proxima_core.blob
          WHERE blob_id = $1 AND owner_id = $2
          FOR UPDATE",
    )
    .bind(blob_id)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?
    else {
        return Err(StorageError::ConstraintViolation(
            "memory cites a blob outside its source owner".into(),
        ));
    };

    if schema_id == proxima_core::UPLOADED_BLOB_SCHEMA_ID
        && !has_only_exact_upload_publications(tx, blob_id, from_id, &content_hash).await?
    {
        return Err(StorageError::ConstraintViolation(
            "uploaded blob does not have an exact-only completed source-owner publication set"
                .into(),
        ));
    }

    // Case 1: the destination already owns an identical row. The three
    // columns compared are `blob`'s declared `dedupe_key`, read off the
    // contract rather than restated here.
    // SQL-POLICY: generated
    let existing: Option<uuid::Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(dedupe_lookup_sql(
        BLOB_SURFACE,
        "blob_id",
        dedupe_key,
    )?))
    .bind(to_id)
    .bind(&schema_id)
    .bind(&content_hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    if let Some(dest_blob_id) = existing {
        if schema_id == proxima_core::UPLOADED_BLOB_SCHEMA_ID
            && !has_only_exact_upload_publications(tx, dest_blob_id, to_id, &content_hash).await?
        {
            return Err(StorageError::ConstraintViolation(
                "destination uploaded blob does not have an exact-only completed publication set"
                    .into(),
            ));
        }
        if dest_blob_id != blob_id {
            remap_handle_refs(tx, remaps, handle, blob_id, dest_blob_id).await?;
        }
        return Ok(());
    }

    // Case 2: nobody else's live series names these bytes, so the rows can
    // simply change hands. Read `handle <> $2 AND owner_id <> $3` carefully:
    // `$3` is the DESTINATION, so the source owner's OWN second series
    // satisfies it and does not count as sharing.
    let shared: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
              WHERE blob_id = $1
                AND handle <> $2
                AND owner_id <> $3
         )",
    )
    .bind(blob_id)
    .bind(handle)
    .bind(to_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if !shared {
        return move_blob_rows_in_place(tx, blob_id, from_id, to_id, &schema_id, &content_hash)
            .await;
    }

    mount_blob_for_destination(
        tx,
        remaps,
        handle,
        to_id,
        BlobMountSource {
            owner_id: from_id,
            blob_id,
            schema_id: &schema_id,
            content_hash: &content_hash,
        },
    )
    .await
}

/// A blob's completed upload rows are a readable publication set only when at
/// least one exists and every one carries exact metadata and the owner-free
/// key minted by its upload lineage. Transfer must reject a mixed valid and
/// malformed set: the read path's newest-row choice could otherwise select
/// the malformed row after transfer.
async fn has_only_exact_upload_publications(
    tx: &mut Transaction<'_, Postgres>,
    blob_id: uuid::Uuid,
    owner_id: uuid::Uuid,
    content_hash: &[u8],
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.blob_uploads u
              WHERE u.blob_id = $1
                AND u.owner_id = $2
                AND u.status = 'completed'
                AND u.completed_at IS NOT NULL
                AND u.content_hash = $3
                AND octet_length(u.sha256) = 32
                AND u.expected_byte_len >= 0
                AND btrim(u.bucket) <> ''
                AND btrim(u.filename) <> ''
                AND btrim(u.mime) <> ''
                AND u.object_key =
                    'objects/' || COALESCE(u.mounted_from_upload_id, u.upload_id)::text
         ) AND NOT EXISTS (
             SELECT 1
               FROM proxima_core.blob_uploads u
              WHERE u.blob_id = $1
                AND u.owner_id = $2
                AND u.status = 'completed'
                AND (
                    u.completed_at IS NULL
                    OR u.content_hash IS DISTINCT FROM $3
                    OR octet_length(u.sha256) IS DISTINCT FROM 32
                    OR u.expected_byte_len < 0
                    OR btrim(u.bucket) = ''
                    OR btrim(u.filename) = ''
                    OR btrim(u.mime) = ''
                    OR u.object_key IS DISTINCT FROM
                        'objects/' || COALESCE(
                            u.mounted_from_upload_id, u.upload_id
                        )::text
                )
         )",
    )
    .bind(blob_id)
    .bind(owner_id)
    .bind(content_hash)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)
}

/// Case 2: the rows change hands, the object does not move, nothing is
/// minted.
async fn move_blob_rows_in_place(
    tx: &mut Transaction<'_, Postgres>,
    blob_id: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    schema_id: &str,
    content_hash: &[u8],
) -> Result<(), StorageError> {
    let moved_blob = sqlx::query(
        "UPDATE proxima_core.blob
            SET owner_id = $2
          WHERE blob_id = $1 AND owner_id = $3",
    )
    .bind(blob_id)
    .bind(to_id)
    .bind(from_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    if moved_blob != 1 {
        return Err(StorageError::Retryable(
            "cited blob owner changed during transfer".into(),
        ));
    }
    // The upload row moves with the blob row it describes: the read path
    // requires both to name the same owner, so leaving it behind makes a
    // transferred citation unreadable at the destination while still
    // counting against the source. The object itself does not move — its key
    // derives from `upload_id`, which is unchanged.
    let moved_uploads = sqlx::query(
        "UPDATE proxima_core.blob_uploads
            SET owner_id = $2
          WHERE blob_id = $1
            AND owner_id = $3
            AND status = 'completed'
            AND completed_at IS NOT NULL
            AND content_hash = $4
            AND octet_length(sha256) = 32
            AND expected_byte_len >= 0
            AND btrim(bucket) <> ''
            AND btrim(filename) <> ''
            AND btrim(mime) <> ''
            AND object_key =
                'objects/' || COALESCE(mounted_from_upload_id, upload_id)::text",
    )
    .bind(blob_id)
    .bind(to_id)
    .bind(from_id)
    .bind(content_hash)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    if schema_id == proxima_core::UPLOADED_BLOB_SCHEMA_ID && moved_uploads == 0 {
        return Err(StorageError::ConstraintViolation(
            "uploaded blob has no source-owner publication row to move".into(),
        ));
    }
    let foreign_upload_remains: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.blob_uploads
              WHERE blob_id = $1 AND owner_id <> $2
         )",
    )
    .bind(blob_id)
    .bind(to_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if foreign_upload_remains {
        return Err(StorageError::ConstraintViolation(
            "cited blob has an upload row outside its source owner".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BlobMountSource<'a> {
    owner_id: uuid::Uuid,
    blob_id: uuid::Uuid,
    schema_id: &'a str,
    content_hash: &'a [u8],
}

/// Case 3: the destination gets its own row over the source's object.
async fn mount_blob_for_destination(
    tx: &mut Transaction<'_, Postgres>,
    remaps: &'static [&'static str],
    handle: uuid::Uuid,
    to_id: uuid::Uuid,
    source: BlobMountSource<'_>,
) -> Result<(), StorageError> {
    let BlobMountSource {
        owner_id: from_id,
        blob_id,
        schema_id,
        content_hash,
    } = source;
    let dest_blob_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3)
         RETURNING blob_id",
    )
    .bind(to_id)
    .bind(schema_id)
    .bind(content_hash)
    .fetch_one(&mut **tx)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => StorageError::Retryable(
            "destination dedupe row appeared during transfer; retry the complete transaction"
                .into(),
        ),
        _ => map_err(err),
    })?;
    // One mounted upload row per completed source upload row, naming the
    // same object. `COALESCE(u.mounted_from_upload_id, u.upload_id)` rather
    // than `u.upload_id`: mounting a mount must still resolve to the row
    // that actually uploaded bytes, because an intermediate row never had
    // an object of its own.
    //
    // `status`, `sha256`, `etag` and the byte length are copied because
    // they describe the OBJECT, which is the thing being shared; the expiry
    // and completion timestamps are copied for the same reason. Only the
    // identity columns differ.
    let mounted_uploads = sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, content_hash, etag, expires_at, completed_at,
              mounted_from_upload_id)
         SELECT $2, u.bucket, u.object_key, u.filename, u.mime, u.expected_byte_len,
                u.status, $3, u.sha256, u.content_hash, u.etag, u.expires_at, u.completed_at,
                COALESCE(u.mounted_from_upload_id, u.upload_id)
          FROM proxima_core.blob_uploads u
          WHERE u.blob_id = $1
            AND u.owner_id = $4
            AND u.status = 'completed'
            AND u.completed_at IS NOT NULL
            AND u.content_hash = $5
            AND octet_length(u.sha256) = 32
            AND u.expected_byte_len >= 0
            AND btrim(u.bucket) <> ''
            AND btrim(u.filename) <> ''
            AND btrim(u.mime) <> ''
            AND u.object_key =
                'objects/' || COALESCE(u.mounted_from_upload_id, u.upload_id)::text",
    )
    .bind(blob_id)
    .bind(to_id)
    .bind(dest_blob_id)
    .bind(from_id)
    .bind(content_hash)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    if schema_id == proxima_core::UPLOADED_BLOB_SCHEMA_ID && mounted_uploads == 0 {
        return Err(StorageError::ConstraintViolation(
            "uploaded blob has no source-owner publication row to mount".into(),
        ));
    }
    remap_handle_refs(tx, remaps, handle, blob_id, dest_blob_id).await
}

/// Repoint this series' referring columns from one row to another.
///
/// `remaps` IS the loop. The declaration lists the referring columns
/// this crate can see; a column that points at the row by convention rather
/// than by constraint cannot be listed there, because the flavor declaring
/// it is not the flavor declaring this surface. Freeze refuses the one way
/// such a column could arise, a cited-object schema declaring a sidecar
/// table (`FlavorRegistryError::CitationSidecarNotRemappable`).
async fn remap_handle_refs(
    tx: &mut Transaction<'_, Postgres>,
    remaps: &'static [&'static str],
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    for entry in remaps {
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(remap_sql(entry)?))
            .bind(handle)
            .bind(from_id)
            .bind(to_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// Re-home Content under the destination so `Memory.owner = Content.owner`
/// after the transfer. Shared payloads stay on the origin owner; only this
/// series is remapped.
async fn transfer_content_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let DedupeLeg { remaps, .. } = dedupe_leg(surfaces, CONTENT_SURFACE)?;
    let rows: Vec<(uuid::Uuid, String, Vec<u8>)> = sqlx::query_as(
        "SELECT DISTINCT c.content_id, c.schema_id, c.content_hash
           FROM proxima_core.content c
           JOIN (
                 SELECT DISTINCT content_id
                   FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
                  WHERE handle = $1 AND owner_id = $2 AND content_id IS NOT NULL
                ) series_content ON series_content.content_id = c.content_id
          ORDER BY c.content_id",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    for (old_id, schema_id, hash) in rows {
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| StorageError::Internal("content hash is not 32 bytes".into()))?;
        let new_id = crate::verbs::content::ensure_content(tx, to_id, &schema_id, &hash).await?;
        if new_id == old_id {
            sqlx::query("UPDATE proxima_core.content SET owner_id = $2 WHERE content_id = $1")
                .bind(old_id)
                .bind(to_id)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
        } else {
            // The same generated remap the blob arm runs, off `content`'s
            // own declared `remaps`. No owner filter is needed: this runs
            // BEFORE the generated re-home, so every row of the handle is
            // still the source's.
            remap_handle_refs(tx, remaps, handle, old_id, new_id).await?;
            crate::verbs::content::gc_unreferenced_content(tx, old_id).await?;
        }
    }
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity: EntityId,
) -> Result<Option<OwnerId>, StorageError> {
    // The entity discriminant is authorization data, exactly as in
    // `visible_home_owner`. A union over both spines on the bare uuid lets a
    // caller relabel a Memory as a Goal (or vice versa) and resolve the owner
    // space off the wrong spine, so each arm asks its own spine only.
    let row: Option<uuid::Uuid> = match entity {
        EntityId::Memory(memory_id) => {
            sqlx::query_scalar(
                "SELECT m.owner_id
               FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (transfer moves forgotten rows too) */
              WHERE m.t = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(executor)
            .await
        }
        EntityId::Goal(goal_id) => {
            sqlx::query_scalar(
                "SELECT g.owner_id
               FROM proxima_core.goal g
              WHERE g.t = $1",
            )
            .bind(goal_id.into_inner())
            .fetch_optional(executor)
            .await
        }
    }
    .map_err(map_err)?;

    Ok(row.map(OwnerId::new))
}

#[cfg(test)]
#[path = "group_manager_race_pg_tests.rs"]
mod group_manager_race_pg_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::flavor::TransferRule;

    fn shipped() -> OwnerSurfaces {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        OwnerSurfaces::for_registry(&registry)
    }

    /// The substrate half of the transfer partition's completeness, and the
    /// exact counterpart of `owner_erase`'s
    /// `every_declared_surface_has_a_leg_this_crate_can_run`.
    ///
    /// Freeze decides in core that every surface reaches SOME leg. What core
    /// cannot know is whether THIS crate can execute the answer: a `Rehomed`
    /// leg whose SQL this file declines to build would be skipped in exactly
    /// the way freeze prevents flavor-side. So every generated leg is
    /// asked for its statement here, and every non-generated one must be a
    /// table a named site in this file claims.
    #[test]
    fn every_declared_surface_has_a_transfer_leg_this_crate_can_run() {
        let surfaces = shipped();
        assert!(
            surfaces.surfaces().len() > 20,
            "the registry should carry the whole core contract, got {}",
            surfaces.surfaces().len()
        );
        for surface in surfaces.surfaces() {
            match surfaces.transfer_leg(surface.table) {
                TransferLeg::Unreachable => panic!(
                    "{} resolves to Unreachable, which freeze should have refused",
                    surface.table
                ),
                leg @ (TransferLeg::Rehomed { .. } | TransferLeg::Dropped { .. }) => {
                    series_leg_sql(surface.table, leg).unwrap_or_else(|e| {
                        panic!(
                            "{} is a generated leg this crate cannot build: {e}",
                            surface.table
                        )
                    });
                }
                TransferLeg::Deduped { .. } => assert!(
                    [BLOB_SURFACE, CONTENT_SURFACE].contains(&surface.table),
                    "{} is a dedupe leg no site in this file orchestrates",
                    surface.table
                ),
                TransferLeg::Bespoke => assert!(
                    [
                        "proxima_core.memory_head",
                        "proxima_core.blob_uploads",
                        "proxima_core.publication_counter",
                    ]
                    .contains(&surface.table),
                    "{} claims a bespoke transfer leg this file does not write",
                    surface.table
                ),
                TransferLeg::StaysOnKey
                | TransferLeg::Retained { .. }
                | TransferLeg::Refused { .. } => {}
            }
        }
    }

    /// The generated set only ever contains the two legs that are statements.
    /// A `StaysOnKey` surface appearing here would run an `UPDATE` over rows
    /// whose owner is already correct by construction.
    #[test]
    fn the_generated_set_is_exactly_the_legs_that_are_statements() {
        let surfaces = shipped();
        let generated = surfaces.generated_transfer_legs();
        assert!(!generated.is_empty(), "the core spine has rows that move");
        for (table, leg) in &generated {
            assert!(
                matches!(
                    leg,
                    TransferLeg::Rehomed { .. } | TransferLeg::Dropped { .. }
                ),
                "{table} is in the generated set as {leg:?}"
            );
        }
        let mut tables: Vec<&str> = generated.iter().map(|(table, _)| *table).collect();
        let sorted = {
            let mut copy = tables.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(tables, sorted, "the generated legs run in table order");
        tables.dedup();
        assert_eq!(tables.len(), generated.len(), "one leg per table");
    }

    /// A rehome sets the surface's ONE declared owner column, named as the
    /// declaration names it rather than as `owner_id` by convention.
    #[test]
    fn a_rehomed_leg_sets_the_owner_column_the_surface_declares() {
        let sql = series_leg_sql(
            "test_flavor.thing",
            TransferLeg::Rehomed {
                key_column: "t",
                owner_column: "custodian_id",
            },
        )
        .expect("a rehome is generated");
        assert_eq!(
            sql,
            "UPDATE test_flavor.thing SET custodian_id = $2 \
             WHERE t = ANY($1::uuid[]) AND custodian_id = $3"
        );
    }

    /// Both generated shapes bind the same three parameters in the same order,
    /// which is what lets `run_generated_transfer_legs` be one loop.
    #[test]
    fn a_dropped_leg_binds_the_same_three_parameters_as_a_rehome() {
        let sql = series_leg_sql(
            "test_flavor.thing",
            TransferLeg::Dropped {
                key_column: "t",
                why: "unused here",
            },
        )
        .expect("a drop is generated");
        assert_eq!(
            sql,
            "DELETE FROM test_flavor.thing WHERE t = ANY($1::uuid[]) \
             AND $2::uuid IS NOT NULL AND $3::uuid IS NOT NULL"
        );
    }

    /// The legs that are not statements are refused rather than approximated.
    #[test]
    fn a_leg_that_is_not_a_statement_is_refused_not_guessed() {
        for leg in [
            TransferLeg::StaysOnKey,
            TransferLeg::Bespoke,
            TransferLeg::Unreachable,
            TransferLeg::Retained { why: "x" },
            TransferLeg::Refused { why: "x" },
        ] {
            assert!(
                series_leg_sql("test_flavor.thing", leg).is_err(),
                "{leg:?} produced a statement"
            );
        }
    }

    /// The lookup's placeholders follow the DECLARED column order, because
    /// that is the order the caller binds in.
    #[test]
    fn the_dedupe_lookup_numbers_placeholders_in_declared_order() {
        let sql = dedupe_lookup_sql(
            "proxima_core.blob",
            "blob_id",
            &["owner_id", "schema_id", "content_hash"],
        )
        .expect("the declared key generates");
        assert_eq!(
            sql,
            "SELECT blob_id FROM proxima_core.blob WHERE owner_id = $1 \
             AND schema_id = $2 AND content_hash = $3"
        );
    }

    /// A remap names the REFERRING table, which is not the surface being
    /// deduped: `blob`'s remap is a column on `memory`.
    #[test]
    fn a_remap_repoints_the_referring_table_not_the_surface() {
        assert_eq!(
            remap_sql("memory.blob_id").expect("a remap generates"),
            "UPDATE proxima_core.memory SET blob_id = $3 \
             WHERE handle = $1 AND blob_id = $2"
        );
        assert!(
            remap_sql("blob_id").is_err(),
            "a remap without a table half is not a remap"
        );
    }

    /// Every `remaps` entry flavor #0 declares generates, and every one names
    /// a table other than its own surface.
    #[test]
    fn every_declared_remap_generates_a_statement() {
        let mut seen = 0;
        for surface in shipped().surfaces() {
            let TransferRule::FollowOrDedupe { remaps, .. } = surface.transfer else {
                continue;
            };
            for entry in remaps {
                remap_sql(entry)
                    .unwrap_or_else(|e| panic!("{} declares remap {entry}: {e}", surface.table));
                let (table, _) = entry.split_once('.').expect("checked by remap_sql");
                assert_ne!(
                    format!("proxima_core.{table}"),
                    surface.table,
                    "a remap repoints a REFERRING column, not the surface's own"
                );
                seen += 1;
            }
        }
        assert_eq!(
            seen, 2,
            "blob and content each declare one referring column"
        );
    }

    /// Goals refuse by DECLARATION, and the refusal quotes the declared
    /// reason rather than a message written next to the `if`.
    #[test]
    fn the_goal_refusal_carries_the_declared_reason() {
        let surfaces = shipped();
        let TransferLeg::Refused { why } = surfaces.transfer_leg(GOAL_SPINE) else {
            panic!("the goal spine declares NotTransferable");
        };
        let StorageError::ConstraintViolation(message) =
            refuse_untransferable_entity(&surfaces, GOAL_SPINE)
        else {
            panic!("an untransferable entity is a constraint the substrate holds");
        };
        let headline = why.split_once(':').map_or(why, |(head, _)| head);
        assert!(
            message.ends_with(headline),
            "the refusal ({message}) should carry the declaration's headline ({headline})"
        );
        assert_eq!(
            message, "goals do not transfer: a goal series cannot change owner",
            "and the wire text is pinned, because the differential golden reads it"
        );

        // A table nothing declares untransferable falls to Internal rather
        // than borrowing the goal wording.
        assert!(matches!(
            refuse_untransferable_entity(&surfaces, MEMORY_SPINE),
            StorageError::Internal(_)
        ));
    }
}
