//! Direct `OwnerRef` column helpers.

use proxima_core::{
    ColdObjectStore, EntityId, GroupId, MembershipRow, OwnerRef, OwnerRefKind, Relation,
    StorageError, UserId, cold_object_key, owner_hash_hex,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err, with_bounded_retry};

#[must_use]
pub fn owner_binds(owner: &OwnerRef) -> (OwnerRefKind, Option<uuid::Uuid>) {
    match *owner {
        OwnerRef::World => (OwnerRefKind::World, None),
        OwnerRef::Personal(user) => (OwnerRefKind::Personal, Some(user.into_inner())),
        OwnerRef::Group(group) => (OwnerRefKind::Group, Some(group.into_inner())),
    }
}

#[must_use]
pub fn owner_arrays(owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    owners.iter().map(owner_binds).unzip()
}

/// Fail closed when a NEW `memories`/`goals` entity row would be created
/// under [`OwnerRef::World`].
///
/// Kernel law: World is universally readable and never a write owner.
/// `memory` carries no World-owner CHECK because the publish-to-World owner
/// TRANSFER (`transfer_to_world`, an UPDATE) must persist World ownership —
/// so raw storage-verb callers (e.g. `flavors/code`, which invokes these
/// verbs directly and bypasses `Engine::authorize_write`'s World
/// short-circuit) have no DB-level backstop there. This helper is that
/// backstop one layer up: every row-creating verb choke point calls it
/// before its INSERT. Goals are never publishable at all, so `goal` /
/// `goal_head` additionally carry `*_not_world_owner_chk` in the DDL.
///
/// Deliberately NOT wired into [`owner_binds`]: rows that legitimately
/// reference a World-owned entity post-publish, and the memory transfer
/// UPDATE itself, must keep encoding World.
///
/// # Errors
///
/// Returns [`StorageError::ConstraintViolation`] — the same error class
/// a DDL CHECK produces — when `owner` is [`OwnerRef::World`].
pub(crate) fn reject_world_write_owner(owner: &OwnerRef) -> Result<(), StorageError> {
    if matches!(owner, OwnerRef::World) {
        return Err(StorageError::ConstraintViolation(
            "World is read-only and never a write owner; new entity rows cannot be created \
             under OwnerRef::World (the publish-to-World owner transfer is the only path \
             that sets it)"
                .into(),
        ));
    }
    Ok(())
}

/// Insert `proxima_core.owners` or confirm the stored kind matches `owner`.
///
/// The only production owners upsert. Memory / goal / wake / citation /
/// cited-blob writes all go through this so an `owner_id` reused under a
/// different kind cannot silently keep the first kind.
///
/// # Errors
///
/// [`StorageError::ConstraintViolation`] when `owner_id` already exists
/// with a different kind. Other storage errors from the upsert.
pub async fn ensure_owner_row(
    conn: &mut sqlx::PgConnection,
    owner: &OwnerRef,
) -> Result<uuid::Uuid, StorageError> {
    let owner_id = owner.stored_owner_id();
    let owner_kind = OwnerRefKind::of(owner).as_str();
    // Two statements: `owners` is append-only (no UPDATE), and a CTE that
    // `DO NOTHING` then SELECTs shares one snapshot — a concurrent first
    // insert waits, then neither arm sees the committed row (`RowNotFound`
    // on complete_upload under concurrency). The SELECT is a new statement
    // so it sees the row the waiter just conflicted with.
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING
         RETURNING kind::text",
    )
    .bind(owner_id)
    .bind(owner_kind)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_err)?;
    let existing = match inserted {
        Some(kind) => kind,
        None => {
            sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
                .bind(owner_id)
                .fetch_one(&mut *conn)
                .await
                .map_err(map_err)?
        }
    };
    if existing != owner_kind {
        return Err(StorageError::ConstraintViolation(
            "owners.kind conflict for owner_id".into(),
        ));
    }
    Ok(owner_id)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_membership(
    pool: &PgPool,
    owner: &OwnerRef,
) -> Result<Vec<MembershipRow>, StorageError> {
    let OwnerRef::Personal(user) = owner else {
        return Ok(Vec::new());
    };

    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT group_id, relation
           FROM proxima_core.group_memberships
          WHERE member_user_id = $1
          ORDER BY group_id, relation",
    )
    .bind(user.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(group, relation)| MembershipRow {
            group: GroupId::new(group),
            relation,
        })
        .collect())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn lock_group_membership_tx(
    tx: &mut Transaction<'_, Postgres>,
    group_id: GroupId,
) -> Result<(), StorageError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('proxima-group-membership:' || $1::text, 0))",
    )
    .bind(group_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Conflict` when any Admin row already exists for the group, and
/// `Internal` on sqlx failure.
pub(crate) async fn bootstrap_group_admin(
    pool: &PgPool,
    group_id: GroupId,
    first_admin_user_id: UserId,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on transient deadlock/serialization,
    // like the other atomic writers.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
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
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        if inserted.rows_affected() == 0 {
            tx.rollback().await.map_err(map_err)?;
            return Err(StorageError::Conflict(
                "group already has an Admin membership".into(),
            ));
        }

        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             VALUES ($1, $2, $3)
             ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .bind(relation)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "DELETE FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// True iff `member_user_id` currently holds exactly `relation` on `group_id`.
/// A point-in-time single-role probe, distinct from
/// [`resolve_membership`]'s full row enumeration.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn has_group_relation(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2
                AND relation = $3
         )",
    )
    .bind(group_id.into_inner())
    .bind(member_user_id.into_inner())
    .bind(relation)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members(
    pool: &PgPool,
    group_id: GroupId,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    // Same total order as `list_group_members_page`. The table has no
    // `created_at` (0001_v008 PK is `(group_id, member_user_id, relation)`).
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
          ORDER BY member_user_id, relation",
    )
    .bind(group_id.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
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
    pool: &PgPool,
    group_id: GroupId,
    after: Option<(UserId, Relation)>,
    limit: i64,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    let after_member = after.map(|(member, _)| member.into_inner());
    let after_relation = after.map(|(_, relation)| relation);
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND ($2::uuid IS NULL
                 OR (member_user_id, relation)
                    > ($2::uuid, $3::proxima_core.membership_relation))
          ORDER BY member_user_id, relation
          LIMIT $4",
    )
    .bind(group_id.into_inner())
    .bind(after_member)
    .bind(after_relation)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
        .collect())
}

/// Home owner when the row is in `read_owners`. Absent and foreign are
/// both `None`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn visible_home_owner(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[OwnerRef],
) -> Result<Option<OwnerRef>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }

    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])
         LIMIT 1",
    )
    .bind(entity.uuid())
    .bind(&owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| match kind {
        OwnerRefKind::World => proxima_core::OwnerRef::World,
        OwnerRefKind::Personal => proxima_core::OwnerRef::Personal(proxima_core::UserId::new(id)),
        OwnerRefKind::Group => proxima_core::OwnerRef::Group(proxima_core::GroupId::new(id)),
    }))
}

/// Transfer one memory **series** to [`OwnerRef::World`].
///
/// Same `(handle, t)`: publish is an owner UPDATE, not a copy. Head and
/// every version on the handle move together (`MemoryHeadAligned`), including
/// cooled stubs (owner + reminted `object_key`).
/// Returns `true` iff a row under `from_owner` matched and was updated.
///
/// Sidecar rows stay keyed by `t`. Cited `blob` rows move when no other
/// live non-World series still cites them. Embeddings / jobs follow the
/// transferred `t`s so ANN (`emb.owner_id`) stays Tesla-valve. `ingest_keys`
/// for those `t`s are deleted so the prior owner can mint a new series.
/// The same transaction announces the transfer under both lanes: the prior
/// owner's (the series left their owned view) and World's (it arrived).
///
/// Goals are never publishable — the engine refuses them before storage;
/// this backstop keeps a direct storage call from transferring one.
///
/// # Errors
///
/// `Conflict` when a cited blob is still referenced by another live
/// series. `ConstraintViolation` for a Goal entity, and for unique /
/// check violations.
pub(crate) async fn transfer_to_world(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    entity: EntityId,
    from_owner: OwnerRef,
) -> Result<bool, StorageError> {
    let memory_id = match entity {
        EntityId::Memory(memory_id) => memory_id,
        EntityId::Goal(_) => {
            return Err(StorageError::ConstraintViolation(
                "goals are never publishable: World owns no goals, so a goal series cannot \
                 be transferred to OwnerRef::World"
                    .into(),
            ));
        }
    };
    let from_id = from_owner.stored_owner_id();
    let world = OwnerRef::World.stored_owner_id();
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let (transferred, stale_cold_keys) =
            transfer_memory_t(&mut tx, cold, memory_id.into_inner(), from_id, world).await?;
        if !transferred {
            // A false persist can follow real writes: when the head is gone
            // or changed owner after the series reads, cooled/blob/content
            // rows are already re-homed to World in this transaction with no
            // announce row. Roll back so they revert to the prior owner and
            // object keys (whose cold objects still exist); the minted World
            // objects were already compensation-deleted in
            // `transfer_memory_handle`.
            tx.rollback().await.map_err(map_err)?;
            return Ok(false);
        }
        tx.commit().await.map_err(map_err)?;
        for key in stale_cold_keys {
            match cold.delete(&key).await {
                Ok(()) | Err(StorageError::NotFound) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        key,
                        "published series left a personal cold object after remint"
                    );
                }
            }
        }
        Ok(transferred)
    })
    .await
}

async fn transfer_memory_t(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
    t: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(bool, Vec<String>), StorageError> {
    let handle: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(handle) = handle else {
        return Ok((false, Vec::new()));
    };
    transfer_memory_handle(tx, cold, handle, from_id, world).await
}

const SERIES_TS_SQL: &str = "SELECT t FROM proxima_core.memory WHERE handle = $1 AND owner_id = $2
     UNION
     SELECT t FROM proxima_core.cooled WHERE handle = $1 AND owner_id = $2";

/// Lock-acquisition rounds for [`lock_series_ts`] before it gives up with
/// `Retryable`. Growth between rounds needs a concurrent same-series ingest
/// inside a statement-sized window, so one extra round is already rare;
/// three keep an adversary appending versions in a tight loop from wedging
/// a publish — the bounded work ends in a typed transient error, never in
/// an unlocked t.
const MAX_SERIES_LOCK_ROUNDS: usize = 3;

/// Serialize the transfer against forget before any row write: hold the
/// per-memory advisory lock the forget path takes, over every `t` of the
/// series, so a concurrent cool cannot land between the series reads and
/// the head persist. The set can drift between a read and the locks it
/// names — a forget keeps the hot-union-cooled set stable, an erase
/// shrinks it, an ingest of a new version grows it — so lock and re-read
/// until the locked set covers the read set, acquiring only the missing
/// `t`s each round in sorted order (xact-scoped advisory locks cannot be
/// released; the set only grows). Two transfers acquiring
/// overlapping-but-different sets across rounds can deadlock; that
/// surfaces as 40P01 → `Retryable`, and `with_bounded_retry` re-runs the
/// whole transaction — the established clean path, same as the round
/// budget running out.
///
/// Returns the authoritative (fully locked) set; empty means the series
/// left the owner while locking.
async fn lock_series_ts(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    mut ts: Vec<uuid::Uuid>,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut locked = std::collections::BTreeSet::new();
    for _ in 0..MAX_SERIES_LOCK_ROUNDS {
        let missing: Vec<uuid::Uuid> = ts.iter().copied().filter(|t| !locked.contains(t)).collect();
        if missing.is_empty() {
            return Ok(ts);
        }
        crate::verbs::forget::lock_forget_memories_tx(tx, &missing).await?;
        locked.extend(missing);
        ts = sqlx::query_scalar(SERIES_TS_SQL)
            .bind(handle)
            .bind(from_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
        if ts.is_empty() {
            return Ok(ts);
        }
    }
    if ts.iter().all(|t| locked.contains(t)) {
        return Ok(ts);
    }
    Err(StorageError::Retryable(
        "series kept growing new versions while acquiring transfer locks".into(),
    ))
}

async fn transfer_memory_handle(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(bool, Vec<String>), StorageError> {
    let ts: Vec<uuid::Uuid> = sqlx::query_scalar(SERIES_TS_SQL)
        .bind(handle)
        .bind(from_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)?;
    if ts.is_empty() {
        return Ok((false, Vec::new()));
    }
    let ts = lock_series_ts(tx, handle, from_id, ts).await?;
    if ts.is_empty() {
        return Ok((false, Vec::new()));
    }
    let expected_head_t: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory_head
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(expected_head_t) = expected_head_t else {
        return Ok((false, Vec::new()));
    };
    if !ts.contains(&expected_head_t) {
        return Err(StorageError::Retryable(
            "series head advanced after transfer locked its version set".into(),
        ));
    }
    transfer_exclusive_blobs(tx, handle, from_id, world).await?;
    transfer_content_for_handle(tx, handle, from_id, world).await?;
    let reminted = remint_cooled_for_handle(tx, cold, handle, from_id, world).await?;
    let persist =
        persist_hot_series_transfer(tx, handle, from_id, world, expected_head_t, &ts).await;
    match persist {
        Ok(true) => {
            announce_series_transfer(tx, handle, from_id, world).await?;
            Ok((true, reminted.old_keys))
        }
        Ok(false) => {
            for key in reminted.new_keys {
                crate::verbs::forget::delete_cold_object(cold, &key).await;
            }
            Ok((false, Vec::new()))
        }
        Err(err) => {
            for key in reminted.new_keys {
                crate::verbs::forget::delete_cold_object(cold, &key).await;
            }
            Err(err)
        }
    }
}

struct RemintedCold {
    old_keys: Vec<String>,
    new_keys: Vec<String>,
}

async fn remint_cooled_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<RemintedCold, StorageError> {
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT t, object_key FROM proxima_core.cooled
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let world_hash = owner_hash_hex(&OwnerRef::World);
    let mut reminted = RemintedCold {
        old_keys: Vec::new(),
        new_keys: Vec::new(),
    };
    for (t, old_key) in rows {
        let new_key = cold_object_key(&world_hash, handle, t);
        if new_key != old_key {
            let bytes = match cold.get(&old_key).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    for key in &reminted.new_keys {
                        crate::verbs::forget::delete_cold_object(cold, key).await;
                    }
                    return Err(err);
                }
            };
            let bytes = match crate::verbs::forget::rehome_cold_record(&bytes, world) {
                Ok(bytes) => bytes,
                Err(err) => {
                    for key in &reminted.new_keys {
                        crate::verbs::forget::delete_cold_object(cold, key).await;
                    }
                    return Err(err);
                }
            };
            if let Err(err) = cold.put(&new_key, &bytes).await {
                for key in &reminted.new_keys {
                    crate::verbs::forget::delete_cold_object(cold, key).await;
                }
                return Err(err);
            }
            reminted.old_keys.push(old_key);
            reminted.new_keys.push(new_key.clone());
        }
        sqlx::query(
            "UPDATE proxima_core.cooled
                SET owner_id = $3, object_key = $4
              WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(from_id)
        .bind(world)
        .bind(&new_key)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(reminted)
}

async fn persist_hot_series_transfer(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
    expected_head_t: uuid::Uuid,
    ts: &[uuid::Uuid],
) -> Result<bool, StorageError> {
    let head = sqlx::query(
        "UPDATE proxima_core.memory_head
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2 AND t = $4",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
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
    sqlx::query(
        "UPDATE proxima_core.memory
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("UPDATE proxima_core.sketch SET owner_id = $2 WHERE t = ANY($1::uuid[])")
        .bind(ts)
        .bind(world)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    follow_embedding_owners(tx, ts, world).await?;
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = ANY($1::uuid[])")
        .bind(ts)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(true)
}

/// One `'transfer'` announce row per lane, same series `(handle, head t)`:
/// the prior owner's projectors learn the series left their owned view, and
/// World-side pull consumers learn it arrived. World's `owners` row is
/// migration-seeded and never erased, so the `announce.owner_id` FK holds.
async fn announce_series_transfer(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    let head_t: uuid::Uuid = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory_head
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(world)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'transfer', 'memory', $3, $4),
                ($2, 'transfer', 'memory', $3, $4)",
    )
    .bind(from_id)
    .bind(world)
    .bind(handle)
    .bind(head_t)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn transfer_exclusive_blobs(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    let blob_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT blob_id
           FROM proxima_core.memory
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL
         UNION
         SELECT blob_id
           FROM proxima_core.cooled
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    for blob_id in blob_ids {
        let shared: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM proxima_core.memory
                  WHERE blob_id = $1
                    AND handle <> $2
                    AND owner_id <> $3
                 UNION ALL
                 SELECT 1
                   FROM proxima_core.cooled
                  WHERE blob_id = $1
                    AND handle <> $2
                    AND owner_id <> $3
             )",
        )
        .bind(blob_id)
        .bind(handle)
        .bind(world)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
        if shared {
            return Err(StorageError::Conflict(
                "cited blob is still referenced by another live non-World series".into(),
            ));
        }
        sqlx::query("UPDATE proxima_core.blob SET owner_id = $2 WHERE blob_id = $1")
            .bind(blob_id)
            .bind(world)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// Re-home Content under World so `Memory.owner = Content.owner` after publish.
/// Shared payloads stay on the origin owner; only this series is remapped.
async fn transfer_content_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    let rows: Vec<(uuid::Uuid, String, Vec<u8>)> = sqlx::query_as(
        "SELECT DISTINCT c.content_id, c.schema_id, c.content_hash
           FROM proxima_core.content c
           JOIN (
                 SELECT content_id
                   FROM proxima_core.memory
                  WHERE handle = $1 AND owner_id = $2 AND content_id IS NOT NULL
                 UNION
                 SELECT content_id
                   FROM proxima_core.cooled
                  WHERE handle = $1 AND owner_id = $2 AND content_id IS NOT NULL
                ) series_content ON series_content.content_id = c.content_id",
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
        let new_id = crate::verbs::content::ensure_content(tx, world, &schema_id, &hash).await?;
        if new_id == old_id {
            sqlx::query("UPDATE proxima_core.content SET owner_id = $2 WHERE content_id = $1")
                .bind(old_id)
                .bind(world)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
        } else {
            sqlx::query(
                "UPDATE proxima_core.memory
                    SET content_id = $3
                  WHERE handle = $1 AND owner_id = $2 AND content_id = $4",
            )
            .bind(handle)
            .bind(from_id)
            .bind(new_id)
            .bind(old_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            sqlx::query(
                "UPDATE proxima_core.cooled
                    SET content_id = $3
                  WHERE handle = $1 AND owner_id = $2 AND content_id = $4",
            )
            .bind(handle)
            .bind(from_id)
            .bind(new_id)
            .bind(old_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            crate::verbs::content::gc_unreferenced_content(tx, old_id).await?;
        }
    }
    Ok(())
}

async fn follow_embedding_owners(
    tx: &mut Transaction<'_, Postgres>,
    ts: &[uuid::Uuid],
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.embeddings SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_heads SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1
         LIMIT 1",
    )
    .bind(entity.uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| match kind {
        OwnerRefKind::World => proxima_core::OwnerRef::World,
        OwnerRefKind::Personal => proxima_core::OwnerRef::Personal(proxima_core::UserId::new(id)),
        OwnerRefKind::Group => proxima_core::OwnerRef::Group(proxima_core::GroupId::new(id)),
    }))
}
